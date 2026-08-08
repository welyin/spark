/**
 * 桥调用 dispatcher + 权限中间件（设计文档「权限模型」运行时强制）。
 *
 * createBridgeHost 的 handler 工厂：把桥 call（module/method/args）分发到
 * ./sdk-browser.ts 的后端实现（createPluginBackend，域一律显式下传；
 * messages 域走 ./messages 壳层服务，pluginId/space 由桥按绑定身份注入），
 * 每次调用前做三重过滤：
 *
 * 1) grantedPermissions：读市场安装状态（pluginMarket.list 聚合的
 *    grantedPermissions，内核侧持久化，渲染进程不可自报）；读取失败按空清单
 *    （最小授权，仅免权限基础调用放行）；
 * 2) view type 裁剪：app 主视图全量、message-card 仅 docs 只读与验签类
 *    （VIEW_ALLOWED_CALLS 映射表，后续按 view 扩充）；
 * 3) 当前 space：manifest supportedSpaces 不含当前 space 类型时整域拒绝；
 *    org 域调用（runtime.syncOrganizationData/listMineOrganizations）在 personal
 *    空间下一律拒绝；org 空间下 syncOrganizationData 的 org 实参必须与当前 space 一致。
 *
 * 未授权一律抛 `Access denied: ...`（与 TS 旧权限中间件文案同前缀）。
 *
 * identity:sign 为「使用时询问」高危权限：首次调用经 ElMessageBox 确认，
 * 按 插件 ID+域名 记忆本次会话决定（会话级，不落盘）；并发首调复用同一确认。
 *
 * 身份三元组（pluginId/viewId/space）来自 createBridgeHost 绑定的值，
 * 不信插件自报（hello 自报仅做一致性核对，见 bridge/host.ts）。
 */

import { invoke } from '@tauri-apps/api/core';
import { ElMessageBox } from 'element-plus';
import type { PluginSpaceContext } from '../../../packages/plugin-sdk/src';
import type { BridgeHostHandler } from '../../../packages/plugin-sdk/src/bridge/host';
import { createPluginBackend } from './sdk-browser';
import { listAppMessages, markAppMessagesRead, sendAppMessage } from './messages';
import type { AppMessageCardDto } from '../api/types';
import { onMessageForContact, type MessageRelayEvent } from '../stores/messages';
import { refreshContacts, ensurePluginContactTag } from '../mock/contacts';

type PluginViewType = 'app' | 'message-card' | 'background';

/** 调用 → 所需权限；不在表内 = 免权限基础调用（验签纯函数、运行时状态读取等） */
const CALL_PERMISSIONS: Record<string, string> = {
  'docs.get': 'storage:read',
  'docs.query': 'storage:read',
  'docs.defineCollection': 'storage:write',
  'docs.put': 'storage:write',
  'docs.delete': 'storage:write',
  'runtime.listMineOrganizations': 'org:read',
  'runtime.syncOrganizationData': 'org:sync',
  'p2p.broadcast': 'network:broadcast',
  'identity.sign': 'identity:sign',
  // 应用会话读写（高级权限 + 内核限流 10 条/60s，§20.5）
  'messages.sendAppMessage': 'message:app',
  'messages.listAppMessages': 'message:app',
  'messages.markRead': 'message:app',
  // sys 代理（内核外呼）：高危操作，每个方法独立授权
  'sys.exec': 'system:exec',
  'sys.fetch': 'network:fetch',
  // 插件联系人消息方法（统一在 messages 命名空间下）
  'messages.registerAsContact': 'message:app',
  'messages.unregisterAsContact': 'message:app',
  'messages.sendResponse': 'message:app',
  'messages.waitForMessage': 'message:app'
};

/**
 * 联系人归属校验：插件只能操作自己注册的联系人。
 * contactId 约定格式 `bot:{pluginId}:{botId}`，pluginId 段必须与桥绑定身份一致，
 * 防止插件注册/注销/读写他人（其他插件或真人）的联系人。校验失败抛错（同步抛，
 * 在 dispatcher 的 try/catch 内被转为拒绝）。
 */
function assertOwnsContact(contactId: string, pluginId: string): void {
  const ownerPluginId = contactId.startsWith('bot:') ? contactId.split(':')[1] : undefined;
  if (ownerPluginId !== pluginId) {
    throw new Error(`Access denied: contact ${contactId} is not owned by plugin ${pluginId}`);
  }
}

/** view type 裁剪表：null = 全量（仅 grantedPermissions 过滤）；未列出的 view type 整域拒绝 */
const VIEW_ALLOWED_CALLS: Record<PluginViewType, ReadonlySet<string> | null> = {
  app: null,
  // 后台常驻视图：全量能力（同 app 主视图）——承载消息监听等常驻任务，
  // 无 UI 但与主视图同为插件的可信执行环境
  background: null,
  // 消息卡片：docs 只读 + 验签/存证读取（无网络、无签名，设计文档「UI 集成点」）；
  // 不含 messages.*——卡片视图无应用会话写权限，卡片回调只经 action 上行（triggerCardAction）
  'message-card': new Set(['docs.get', 'docs.query', 'identity.verify', 'evidence.headHash', 'evidence.verify'])
};

/** org 域调用：需组织空间上下文，personal 空间下一律拒绝（无 org 实参可校验） */
const ORG_SPACE_CALLS = new Set(['runtime.syncOrganizationData', 'runtime.listMineOrganizations']);

export type PluginBridgeIdentity = {
  pluginId: string;
  viewId: string;
  /** 插件域身份（plugin: 前缀，由 pluginId 推出） */
  domain: string;
  space: PluginSpaceContext;
  /** 插件显示名（使用时询问文案）；缺省用 pluginId */
  pluginName?: string;
  /** manifest supportedSpaces（缺省 = 不做 space 类型校验） */
  supportedSpaces?: Array<'personal' | 'org'>;
  /** 视图类型（默认 app） */
  viewType?: PluginViewType;
};

/** 使用时询问的会话级决定记忆：key = `${pluginId}|${domain}`（pluginName 仅用于弹窗文案） */
const useTimeConsent = new Set<string>();
/** 确认进行中的 in-flight Promise：并发首调复用同一确认，避免弹多个框 */
const pendingConsent = new Map<string, Promise<void>>();

/** identity:sign 首次使用确认；用户拒绝抛 Access denied */
async function confirmIdentitySign(pluginId: string, pluginName: string, domain: string): Promise<void> {
  const consentKey = `${pluginId}|${domain}`;
  if (useTimeConsent.has(consentKey)) {
    return;
  }
  const inflight = pendingConsent.get(consentKey);
  if (inflight) {
    return inflight;
  }
  const prompt = (async () => {
    try {
      await ElMessageBox.confirm(
        `应用「${pluginName}」（${domain}）请求使用插件域身份签名。签名以该应用域身份出具，可用于数据确权与存证，是否允许？（本次会话内记住选择）`,
        '签名确认',
        { confirmButtonText: '允许', cancelButtonText: '拒绝', type: 'warning' }
      );
    } catch {
      throw new Error('Access denied: identity:sign rejected by user');
    }
    useTimeConsent.add(consentKey);
  })();
  pendingConsent.set(consentKey, prompt);
  try {
    await prompt;
  } finally {
    pendingConsent.delete(consentKey);
  }
}

/**
 * 构造桥 handler：先读 grantedPermissions 与后端实例，返回逐调用过滤的分发器。
 */
export async function createPluginBridgeDispatcher(identity: PluginBridgeIdentity): Promise<BridgeHostHandler> {
  const viewType = identity.viewType ?? 'app';
  const pluginName = identity.pluginName ?? identity.pluginId;

  // 三重过滤之一：grantedPermissions（内核持久化的安装授权；读取失败按空清单）
  let granted = new Set<string>();
  try {
    const items = await window.electronAPI.pluginMarket.list();
    granted = new Set(items.find((item) => item.id === identity.pluginId)?.grantedPermissions ?? []);
  } catch {
    // 最小授权：仅免权限基础调用放行
  }

  const backend = createPluginBackend(identity.domain);

  // messages 域：pluginId/space 由桥按绑定身份注入（插件自报一律忽略）。
  // pluginId 剥离域前缀（'plugin:spark-example' → 'spark-example'，§20.1 存储键口径；
  // 与 identity.pluginId 同源，domain 推导失败时回退绑定 pluginId）
  const boundPluginId = identity.domain.startsWith('plugin:')
    ? identity.domain.slice('plugin:'.length)
    : identity.pluginId;
  const boundSpaceKey = identity.space.type === 'org' ? `org:${identity.space.id}` : 'personal';

  const modules: Record<string, Record<string, (...args: any[]) => Promise<unknown>>> = {
    docs: {
      get: backend.docs.get,
      defineCollection: backend.docs.defineCollection,
      put: backend.docs.put,
      delete: backend.docs.delete,
      query: backend.docs.query
    },
    identity: {
      sign: backend.identity.sign,
      verify: backend.identity.verify
    },
    evidence: {
      headHash: backend.evidence.headHash,
      verify: backend.evidence.verify
    },
    p2p: {
      start: backend.p2p.start,
      stop: backend.p2p.stop,
      broadcast: backend.p2p.broadcast
    },
    runtime: {
      currentRoot: backend.runtime.currentRoot,
      syncOrganizationData: backend.runtime.syncOrganizationData,
      listMineOrganizations: backend.runtime.listMineOrganizations
    },
    // 统一消息模块：服务号（应用会话 §20）+ 插件联系人
    messages: {
      sendAppMessage: (payload: Record<string, unknown>, card?: AppMessageCardDto) =>
        sendAppMessage(boundSpaceKey, boundPluginId, payload, card),
      listAppMessages: () => listAppMessages(boundSpaceKey, boundPluginId),
      markRead: () => markAppMessagesRead(boundSpaceKey, boundPluginId),
      registerAsContact: (contactId: string, displayName: string) => {
        assertOwnsContact(contactId, identity.pluginId);
        return invoke('contact_ensure_bot', {
          botRootId: contactId,
          displayName
        }).then(async (result) => {
          // ensure_bot 是内核本地写入，不产生 P2P 事件——通讯录缓存
          // 首次水合后不会自动感知，主动刷新让新 bot 立即出现在列表
          await refreshContacts(boundSpaceKey);
          // 统一打上"以插件显示名命名"的标签，标识联系人来源于哪个插件
          ensurePluginContactTag(boundSpaceKey, identity.pluginName || identity.pluginId, contactId);
          return result;
        });
      },
      unregisterAsContact: (contactId: string) => {
        assertOwnsContact(contactId, identity.pluginId);
        // 插件删除 bot 时注销联系人：内核删好友记录 + 刷新缓存让列表立即移除
        return invoke('contact_remove_friend', { rootId: contactId, block: false })
          .then(async (result) => {
            await refreshContacts(boundSpaceKey);
            return result;
          });
      },
      sendResponse: (convId: string, contactId: string, displayName: string, messageId: string, text: string) => {
        assertOwnsContact(contactId, identity.pluginId);
        return invoke('message_bot_reply', {
          spaceKey: boundSpaceKey,
          convId,
          botRootId: contactId,
          botName: displayName,
          messageId,
          text
        });
      },
      waitForMessage: (contactId: string, timeoutMs?: number) => {
        assertOwnsContact(contactId, identity.pluginId);
        console.log(`[bridge] waitForMessage 注册监听 contactId=${contactId}`);
        return new Promise<MessageRelayEvent | null>((resolve) => {
          const waitMs = timeoutMs ?? 30000;
          const timer = setTimeout(() => {
            unsub();
            resolve(null);
          }, waitMs);
          const unsub = onMessageForContact(contactId, (event) => {
            console.log(`[bridge] waitForMessage 收到消息 contactId=${contactId} text=${event.text?.slice(0, 30)}`);
            clearTimeout(timer);
            unsub();
            resolve(event);
          });
        });
      }
    },
    // sys 代理（内核外呼）：仅代理不加工；插件享有完整权限，内核命令侧负责业务安全
    sys: {
      exec: (program: string, execArgs: string[], workdir?: string) => {
        console.log(`[bridge] sys.exec 收到 program=${program} args=${JSON.stringify(execArgs)} workdir=${workdir ?? '(继承)'}`);
        return backend.sys!.exec(program, Array.isArray(execArgs) ? execArgs : [], workdir) as Promise<unknown>;
      },
      fetch: (url: string, options?: Record<string, unknown>) =>
        backend.sys!.fetch(url, options) as Promise<unknown>
    },
  };

  return async (module, method, args) => {
    const callKey = `${module}.${method}`;
    const fn = modules[module]?.[method] as ((...callArgs: unknown[]) => Promise<unknown>) | undefined;
    if (!fn) {
      throw new Error(`Access denied: unknown SDK call ${callKey}`);
    }

    // 三重过滤之二：view type 裁剪
    const viewAllowed = VIEW_ALLOWED_CALLS[viewType];
    if (viewAllowed && !viewAllowed.has(callKey)) {
      throw new Error(`Access denied: ${callKey} is not available in ${viewType} view`);
    }

    // 三重过滤之一（续）：grantedPermissions
    const required = CALL_PERMISSIONS[callKey];
    if (required && !granted.has(required)) {
      throw new Error(`Access denied: permission "${required}" is not granted for plugin ${identity.pluginId}`);
    }

    // 三重过滤之三：当前 space
    if (identity.supportedSpaces && !identity.supportedSpaces.includes(identity.space.type)) {
      throw new Error(
        `Access denied: plugin ${identity.pluginId} does not support ${identity.space.type} space`
      );
    }
    // personal 空间无 org 上下文：org 域调用一律拒绝
    if (ORG_SPACE_CALLS.has(callKey) && identity.space.type === 'personal') {
      throw new Error(`Access denied: ${callKey} requires org space`);
    }
    if (
      callKey === 'runtime.syncOrganizationData' &&
      identity.space.type === 'org' &&
      args[0] !== identity.space.id
    ) {
      throw new Error(`Access denied: org ${String(args[0])} is outside current space ${identity.space.id}`);
    }

    // 使用时询问：identity:sign 首次确认（会话级记忆，并发首调复用同一确认）
    if (callKey === 'identity.sign') {
      await confirmIdentitySign(identity.pluginId, pluginName, identity.domain);
    }

    return fn(...args);
  };
}

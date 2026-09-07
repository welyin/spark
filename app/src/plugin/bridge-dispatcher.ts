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
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { ElMessageBox } from 'element-plus';
import type { PluginCredentialsAPI, PluginSpaceContext } from '../../../packages/plugin-sdk/src';
import type { BridgeHostHandler } from '../../../packages/plugin-sdk/src/bridge/host';
import { createPluginBackend } from './sdk-browser';
import { listAppMessages, markAppMessagesRead, sendAppMessage } from './messages';
import type { AppMessageCardDto } from '../api/types';
import { refreshContacts, ensurePluginContactTag } from '../mock/contacts';

/** 桥事件泵：由外部（PluginIframeHost）注入，用于将 Tauri 事件转发为桥 event。 */
export interface BridgeEventPump {
  pushEvent: (event: string, payload?: unknown) => void;
}

let _eventPump: BridgeEventPump | null = null;

/** 注入事件泵（仅 PluginIframeHost 调用一次）。 */
export function setBridgeEventPump(pump: BridgeEventPump | null) {
  _eventPump = pump;
}

type PluginViewType = 'app' | 'message-card' | 'background';

/** 调用 → 所需权限；不在表内 = 免权限基础调用（验签纯函数、运行时状态读取等） */
const CALL_PERMISSIONS: Record<string, string> = {
  'docs.get': 'storage:read',
  'docs.query': 'storage:read',
  'docs.defineCollection': 'storage:write',
  'docs.put': 'storage:write',
  'docs.delete': 'storage:write',
  // P6 声明式数据 API（与内核 host_env.rs capability_permission 逐字对齐）
  'data.get': 'storage:read',
  'data.query': 'storage:read',
  'data.readBlob': 'storage:read',
  'data.declareCollection': 'storage:write',
  'data.save': 'storage:write',
  'data.delete': 'storage:write',
  'data.dropVersion': 'storage:write',
  'data.saveBlob': 'storage:write',
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
  'sys.fetchStream': 'network:fetch',
  'sys.pickFolder': 'system:exec',
  // 插件联系人消息方法（统一在 messages 命名空间下）
  'messages.registerAsContact': 'message:app',
  'messages.unregisterAsContact': 'message:app',
  'messages.sendResponse': 'message:app',
  // 通讯录只读门面（社交投递层 §9.4 contact:read；高级 + 使用时询问）
  'contacts.listFriends': 'contact:read',
  'contacts.listGroups': 'contact:read',
  'contacts.listTags': 'contact:read',
  // 社交定向投递（social-feed §9.3 feed:deliver 高级 + 内核限流；onReceive/pull
  // 接收侧免权限——不在本表即放行）
  'feed.deliver': 'feed:deliver',
  // 内容面 blob（public-topics §七 sdk.content）：读/拉取/列表归 storage:read，
  // 保存/根标记/GC 归 storage:write（与 data.readBlob/saveBlob 同权限口径）
  'content.readBlob': 'storage:read',
  'content.fetchBlob': 'storage:read',
  'content.listBlobs': 'storage:read',
  'content.saveBlob': 'storage:write',
  'content.pinRoot': 'storage:write',
  'content.unpinRoot': 'storage:write',
  'content.gcSweep': 'storage:write',
  // community-affairs §7.2：事务读/写（写含关注/取关/提交操作）、凭证只读、
  // 策略读/写。读位 affairs:read / credentials:read / policy:read，写位
  // affairs:write / policy:write；与内核 market/permissions.rs 逐字对齐。
  'affairs.listFollowed': 'affairs:read',
  'affairs.readLog': 'affairs:read',
  'affairs.readRules': 'affairs:read',
  'affairs.readResolution': 'affairs:read',
  'affairs.ladderStatus': 'affairs:read',
  'affairs.publicProfile': 'affairs:read',
  'affairs.snapshotPayload': 'affairs:read',
  'affairs.readExec': 'affairs:read',
  'affairs.orgEffects': 'affairs:read',
  'affairs.follow': 'affairs:write',
  'affairs.unfollow': 'affairs:write',
  'affairs.submitOp': 'affairs:write',
  // 回执编排写 org:effectrcpt: 并逐条存证（幂等、LWW）——与提交操作同写位
  'affairs.applyOrgEffects': 'affairs:write',
  'credentials.listHeld': 'credentials:read',
  'credentials.presentHolderProof': 'credentials:read',
  'credentials.queryVerifiers': 'credentials:read',
  'credentials.verify': 'credentials:read',
  'credentials.queryRevocations': 'credentials:read',
  'policy.read': 'policy:read',
  'policy.submitDraft': 'policy:write',
  // 发布合入（org:policydoc: 同步键域，管理员授权面）与草稿提交同写位
  'policy.publish': 'policy:write'
  // 注：affairs.create 是桥 client 侧组合（identity.sign + affairs.follow，
  // 逐调用各自由本表强制）；affairs.onChange 走事件订阅通道（subscribe
  // 不经 call 表，与 data.onChange 同口径，事件载荷仅 affairId+变更类别）
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

/**
 * 社交投递 topic 前缀校验（架构 §8「topic 前缀即插件归属」，出站侧）：
 * 出站 deliver 的 topic 前缀必须 == 调用方插件 id，防止插件向他人 topic 投递
 * 或冒充其它插件归属。校验失败抛错（同步抛，dispatcher try/catch 内转拒绝）。
 */
function assertTopicOwned(topic: string, pluginId: string): void {
  const prefix = topic.split(':')[0] ?? topic;
  if (prefix !== pluginId) {
    throw new Error(
      `InvalidTopic: topic prefix "${prefix}" does not match plugin "${pluginId}"`
    );
  }
}

/** view type 裁剪表：null = 全量（仅 grantedPermissions 过滤）；未列出的 view type 整域拒绝 */
const VIEW_ALLOWED_CALLS: Record<PluginViewType, ReadonlySet<string> | null> = {
  app: null,
  // background 视图已下线（插件常驻逻辑迁往内核 QuickJS 后台运行时，
  // 见 plugin_system.md「后台运行时」）；类型保留仅为兼容历史清单的解析
  background: null,
  // 消息卡片：docs/data 只读 + 验签/存证读取（无网络、无签名，设计文档「UI 集成点」）；
  // 不含 messages.*——卡片视图无应用会话写权限，卡片回调只经 action 上行（triggerCardAction）
  'message-card': new Set(['docs.get', 'docs.query', 'data.get', 'data.query', 'data.readBlob', 'identity.verify', 'evidence.headHash', 'evidence.verify'])
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
  /** 插件请求关闭自身视图（sdk.close()）时触发 */
  onClose?: () => void;
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

  // O3 org 上下文：data.* 读写经 createPluginBackend 注入 orgId（org space
  // 取 identity.space.id；personal space 为 undefined）——插件 SDK 签名不变，
  // orgId 由内核按插件实例所属空间解析，桥按绑定身份下发。
  const boundOrgId = identity.space.type === 'org' ? identity.space.id : undefined;
  const backend = createPluginBackend(identity.domain, boundOrgId, identity.onClose);

  // messages 域：pluginId/space 由桥按绑定身份注入（插件自报一律忽略）。
  // pluginId 剥离域前缀（'plugin:spark-example' → 'spark-example'，§20.1 存储键口径；
  // 与 identity.pluginId 同源，domain 推导失败时回退绑定 pluginId）
  const boundPluginId = identity.domain.startsWith('plugin:')
    ? identity.domain.slice('plugin:'.length)
    : identity.pluginId;
  const boundSpaceKey = identity.space.type === 'org' ? `org:${identity.space.id}` : 'personal';

  const modules: Record<string, Record<string, (...args: any[]) => Promise<unknown>>> = {
    // 应用级控制（免权限）：插件请求关闭自身视图
    app: {
      close: async () => {
        identity.onClose?.();
      }
    },
    docs: {
      get: backend.docs.get,
      defineCollection: backend.docs.defineCollection,
      put: backend.docs.put,
      delete: backend.docs.delete,
      query: backend.docs.query
    },
    data: {
      declareCollection: backend.data.declareCollection,
      save: backend.data.save,
      delete: backend.data.delete,
      get: backend.data.get,
      query: backend.data.query,
      dropVersion: backend.data.dropVersion,
      saveBlob: backend.data.saveBlob,
      readBlob: backend.data.readBlob
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
      }
      // waitForMessage（长轮询监听 bot 消息）已随后台运行时迁移下线：
      // bot 消息由内核直接推送到插件的 QuickJS 后台线程（spark.onMessage），
      // iframe 视图不再有消费 bot 消息的场景
    },
    // 通讯录只读门面（社交投递层 §9.4 contact:read；CALL_PERMISSIONS 强制；
    // 经 backend.contacts 走 electronAPI.contacts，与 docs/data 域一致；
    // backend.contacts 在 SDK 类型上为可选，但 createPluginBackend 恒注入，非空断言同 sys）
    contacts: {
      listFriends: () => backend.contacts!.listFriends(),
      listGroups: () => backend.contacts!.listGroups(),
      listTags: () => backend.contacts!.listTags()
    },
    // 社交定向投递（social-feed §9.1 sdk.feed）。deliver 经 CALL_PERMISSIONS
    // 强制 feed:deliver + 出站 topic 前缀校验（架构 §8）；pull/onReceive 接收侧
    // 免权限（onReceive 为事件订阅，不在此 call 表内，由 PluginIframeHost 经桥
    // FeedReceived 事件推送）。pluginId 由桥绑定身份注入（不信插件自报）。
    feed: {
      deliver: (input: {
        topic: string;
        payload: unknown;
        recipients: string[];
        replyTo?: string;
        feedId?: string;
      }) => {
        assertTopicOwned(input.topic, identity.pluginId);
        return backend.feed!.deliver(input);
      },
      pull: (input: { topic: string; cursor?: string; limit?: number }) => {
        // B2：pull 与 deliver 同构做 topic 前缀归属校验——防任一插件
        // `sdk.feed.pull({topic:"spark-moments:posts"})` 读他人收件箱。
        // 桥校验 + 壳层 feed_pull_inner 校验双保险（不信插件自报）。
        assertTopicOwned(input.topic, identity.pluginId);
        return backend.feed!.pull(input);
      }
    },
    // 内容面 blob（public-topics §七 sdk.content；CALL_PERMISSIONS 强制
    // storage:read/storage:write；CID 形状与哈希校验在内核兜底）
    content: {
      saveBlob: (dataBase64: string) => backend.content!.saveBlob(dataBase64),
      readBlob: (cid: string) => backend.content!.readBlob(cid),
      fetchBlob: (cid: string) => backend.content!.fetchBlob(cid),
      listBlobs: () => backend.content!.listBlobs(),
      pinRoot: (cid: string, root: string) => backend.content!.pinRoot(cid, root),
      unpinRoot: (cid: string, root: string) => backend.content!.unpinRoot(cid, root),
      gcSweep: () => backend.content!.gcSweep()
    },
    // community-affairs §7.2 sdk.affairs：内核 affair 门面薄壳（权限经
    // CALL_PERMISSIONS 强制 affairs:read/affairs:write；affairId 自报面由
    // 内核门面以创世复算/形态校验兜底）
    affairs: {
      follow: (genesis: Record<string, unknown>) => backend.affairs!.follow(genesis),
      unfollow: (affairId: string) => backend.affairs!.unfollow(affairId),
      listFollowed: () => backend.affairs!.listFollowed(),
      submitOp: (op: Record<string, unknown>) => backend.affairs!.submitOp(op),
      readLog: (affairId: string) => backend.affairs!.readLog(affairId),
      readRules: (affairId: string) => backend.affairs!.readRules(affairId),
      readResolution: (affairId: string) => backend.affairs!.readResolution(affairId),
      ladderStatus: (affairId: string) => backend.affairs!.ladderStatus(affairId),
      publicProfile: (identity: string) => backend.affairs!.publicProfile(identity),
      snapshotPayload: (affairId: string, asOf?: string) => backend.affairs!.snapshotPayload(affairId, asOf),
      readExec: (affairId: string) => backend.affairs!.readExec(affairId),
      orgEffects: (orgId: string, affairId: string) => backend.affairs!.orgEffects(orgId, affairId),
      applyOrgEffects: (orgId: string, affairId: string) => backend.affairs!.applyOrgEffects(orgId, affairId)
    },
    // community-affairs §7.2 sdk.credentials：backend 已按绑定域注入签名域
    // （presentHolderProof 的 pluginDomain 插件不可自报）
    credentials: {
      listHeld: () => backend.credentials!.listHeld(),
      presentHolderProof: (input: { credId: string; requestId: string; orgId: string; collection: string }) =>
        backend.credentials!.presentHolderProof(input),
      queryVerifiers: (orgId: string) => backend.credentials!.queryVerifiers(orgId),
      // 桥入参为插件自报的宽松 JSON；结构校验在内核验证链（§6 第 1–5 步），此处仅做类型过渡
      verify: (credential: Record<string, unknown>) =>
        backend.credentials!.verify(credential as Parameters<PluginCredentialsAPI['verify']>[0]),
      queryRevocations: (issuer: string) => backend.credentials!.queryRevocations(issuer)
    },
    // community-affairs §7.2 sdk.policy：本地策略草稿读写 + 发布合入
    policy: {
      read: (orgId: string) => backend.policy!.read(orgId),
      submitDraft: (doc: Record<string, unknown>) => backend.policy!.submitDraft(doc),
      publish: (orgId: string) => backend.policy!.publish(orgId)
    },
    // sys 代理（内核外呼）：仅代理不加工；插件享有完整权限，内核命令侧负责业务安全
    sys: {
      exec: (program: string, execArgs: string[], workdir?: string) => {
        console.log(`[bridge] sys.exec 收到 program=${program} args=${JSON.stringify(execArgs)} workdir=${workdir ?? '(继承)'}`);
        return backend.sys!.exec(program, Array.isArray(execArgs) ? execArgs : [], workdir) as Promise<unknown>;
      },
      fetch: (url: string, options?: Record<string, unknown>) =>
        backend.sys!.fetch(url, options) as Promise<unknown>,
      pickFolder: (title?: string) =>
        backend.sys!.pickFolder!(title) as Promise<unknown>,
      fetchStream: async (url: string, options?: Record<string, unknown>) => {
        // 1. 发起流式请求，取得 streamId
        const { streamId } = await backend.sys!.fetchStream!(url, options) as { streamId: string };
        const eventName = `sys-stream:${streamId}`;

        // 2. 监听 Tauri 事件，逐块转发为桥 event
        const unlisten: UnlistenFn = await listen(eventName, (event) => {
          const chunk = event.payload as { text: string; done: boolean; status: number; headers: Record<string, string> };
          if (_eventPump) {
            _eventPump.pushEvent(eventName, chunk);
          }
          if (chunk.done) {
            unlisten();
          }
        });

        return { streamId };
      }
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

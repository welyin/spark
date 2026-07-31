/**
 * Spark 插件 SDK（@spark/plugin-sdk）。
 *
 * 纯类型 + 入口契约包，零运行时依赖（不依赖 vue/element-plus/Tauri）：
 * - SDK 类型（PluginSDK 等）：插件与壳层共享的唯一类型来源，
 *   壳层 src/plugin-sdk-browser.ts 从这里 re-export 并持有 Tauri 实现；
 * - definePlugin：插件入口契约（第三方插件约定，保留）——沙箱化后壳层不再
 *   编译期装载，插件 bundle 在沙箱 iframe 内经 bridge/client 握手自挂载；
 * - getPluginSDK/ensurePluginSDK：从全局注入点 window.__sparkPluginSDK
 *   读取宿主注入的 SDK 实例（插件入口在桥握手完成时写入）。
 *
 * app 与 plugins 均经相对路径引用本包（../../packages/plugin-sdk/src），不发布 npm。
 */

// ------------------------------------------------------------------
// SDK 类型（迁自 app/src/plugin-sdk-browser.ts，原为内联定义）
// ------------------------------------------------------------------

export type PluginQueryFilter = {
  field: string;
  value: string | number | boolean;
  op?: 'eq' | 'startsWith' | 'gt' | 'lt' | 'gte' | 'lte';
};

export type PluginDocQueryOptions = {
  limit?: number;
  reverse?: boolean;
  filter?: PluginQueryFilter[];
};

export interface PluginEvidenceAPI {
  headHash: () => Promise<{ hash: string | null }>;
  verify: () => Promise<{ valid: boolean; height: number }>;
}

export interface PluginP2PAPI {
  start: () => Promise<{ started: boolean }>;
  stop: () => Promise<{ started: boolean }>;
  broadcast: (topic: string, message: Record<string, any>) => Promise<{ success: boolean }>;
}

export interface PluginRuntimeAPI {
  currentRoot: () => Promise<{ unlocked: boolean; rootId: string | null }>;
  syncOrganizationData: (orgId: string) => Promise<{ orgId: string; attempted: number; pulled: number }>;
  listMineOrganizations: () => Promise<Array<{
    orgId: string;
    name: string;
    description: string;
    currentUserRole: 'admin' | 'member' | null;
    isCurrentUserAdmin: boolean;
    memberCount: number;
    adminCount: number;
    members: Array<{
      rootId: string;
      role: 'admin' | 'member';
      joinedAt: number;
      addedBy: string;
      nodeInfo?: {
        peerId?: string;
        addresses: string[];
      };
    }>;
  }>>;
}

export interface PluginDocAPI {
  get: <T extends Record<string, unknown> = Record<string, unknown>>(collection: string, id: string) => Promise<T | null>;
  /** 声明集合同步策略：写入前必须调用，syncStrategy 必填；声明持久化且不可变更 */
  defineCollection: (collection: string, schema: PluginCollectionSchema) => Promise<PluginDeclaredCollectionSchema>;
  put: (collection: string, id: string, doc: Record<string, unknown>) => Promise<{ success: boolean }>;
  delete: (collection: string, id: string) => Promise<{ success: boolean }>;
  query: <T extends Record<string, unknown> = Record<string, unknown>>(
    collection: string,
    options?: PluginDocQueryOptions
  ) => Promise<{
    items: Array<{ id: string; data: T }>;
    nextCursor?: string;
  }>;
}

/** 域签名结果（与壳层 api/types.ts DomainSignature 同形，结构类型天然兼容） */
export type DomainSignature = {
  domain: string;
  domainId: string;
  publicKey: string;
  signature: string;
  payloadHash: string;
};

/**
 * 插件身份能力
 * 签名使用调用方插件域身份（域私钥永不离开内核），根身份不暴露；
 * 验签为纯函数，可用于校验其他成员在对应域内的签名
 */
export interface PluginIdentityAPI {
  sign: (payload: string) => Promise<DomainSignature>;
  verify: (payload: string, signature: string, publicKey: string) => Promise<{ valid: boolean }>;
}

/**
 * 集合同步策略声明（设计文档 V2 §4.3.4）
 * - `syncStrategy` 必填，类型层面强制显式选择：
 *   - `append-only`（默认推荐）：仅追加、不覆盖、不删除，自动配合链式存证
 *   - `lww`：最后写入获胜，仅适用于可容忍覆盖的普通状态数据
 * - `governance`：治理类数据（投票、成员、账目）标记，强制 append-only + 链式存证，插件无权降级
 * - `enableEvidence`：仅 lww 集合可选；append-only 集合强制开启
 * 声明持久化且不可变更，重复声明必须与首次一致。
 */
export interface PluginCollectionSchema {
  syncStrategy: 'append-only' | 'lww';
  governance?: boolean;
  enableEvidence?: boolean;
}

export interface PluginDeclaredCollectionSchema {
  collection: string;
  syncStrategy: 'append-only' | 'lww';
  governance: boolean;
  enableEvidence: boolean;
}

// ------------------------------------------------------------------
// 消息模块（应用会话，服务号模型，p2p-messages.md §20）
// ------------------------------------------------------------------

/** 应用消息卡片（message-card 富渲染视图；viewId 为清单声明的 message-card 视图） */
export type PluginAppMessageCard = {
  viewId: string;
  data?: unknown;
};

/** 应用消息（本地生成、本地消费，状态恒 'local'，无投递语义） */
export type PluginAppMessage = {
  id: string;
  pluginId: string;
  /** 纯文本摘要（未装插件时壳层原生渲染此字段） */
  summary: string;
  payload: Record<string, unknown>;
  card?: PluginAppMessageCard;
  createdAt: number;
  status: 'local';
  read: boolean;
};

/** 卡片按钮回调载荷（壳层从 message-card 收到 action 后路由给主视图实例） */
export type PluginCardActionPayload = {
  cardId: string;
  actionId: string;
  data?: unknown;
};

/**
 * 消息模块：应用会话读写与卡片交互回调（高级权限 `message:app`，内核限流 10 条/60s）。
 * 插件侧不传 pluginId/space——由桥按已认证身份注入，插件只能读写自己的应用会话；
 * 桥协议层面不提供人际会话接口（隐私红线）。
 * 仅 iframe 桥模式可用，故在 PluginSDK 上为可选字段（同 events）。
 */
export interface PluginMessagesAPI {
  /** 写入应用消息：payload 必须含非空字符串 summary（trim 后 ≤200 字符，超限拒绝） */
  sendAppMessage: (payload: Record<string, unknown>, card?: PluginAppMessageCard) => Promise<PluginAppMessage>;
  /** 本插件在当前空间的应用消息（时间升序） */
  listAppMessages: () => Promise<PluginAppMessage[]>;
  /** 清零本插件应用会话未读（语义与人际会话一致） */
  markRead: () => Promise<{ success: boolean }>;
  /**
   * 注册卡片按钮回调（主视图）：壳层从 message-card iframe 收到 action 并校验
   * 卡片归属本插件后推送过来；返回注销函数
   */
  onCardAction: (handler: (action: PluginCardActionPayload) => void) => () => void;
  /**
   * 触发卡片按钮回调（仅 message-card 视图）：action 上行给壳层，
   * 经归属校验后路由给本插件主视图实例的 onCardAction；cardId 取桥握手 ctx 注入值
   *
   * @throws 非 message-card 视图（ctx.mount 无 cardId）
   */
  triggerCardAction: (actionId: string, data?: unknown) => void;
  /**
   * 申请卡片高度（仅 message-card 视图）：壳层封顶 400px
   *
   * @throws 非 message-card 视图（ctx.mount 无 cardId）
   */
  requestCardHeight: (height: number) => void;
}

// ------------------------------------------------------------------
// 事件模块（随桥协议落地，见 bridge/client.ts）
// ------------------------------------------------------------------

/** 系统事件回调（payload 为结构化克隆安全的 JSON） */
export type PluginEventHandler = (payload: unknown) => void;

/**
 * 事件模块：订阅/取消订阅系统事件。
 * iframe 桥模式下由 connectPluginBridge 提供；tab 模式（同进程注入）未实现，
 * 故在 PluginSDK 上为可选字段。
 */
export interface PluginEventsAPI {
  subscribe: (event: string, handler: PluginEventHandler) => Promise<void>;
  unsubscribe: (event: string, handler?: PluginEventHandler) => Promise<void>;
}

export interface PluginSDK {
  /** 当前插件的域身份：tab 模式下由 URL query `pluginDomain` 解析（对齐旧 tab 语义） */
  domain: string;
  evidence: PluginEvidenceAPI;
  p2p: PluginP2PAPI;
  runtime: PluginRuntimeAPI;
  docs: PluginDocAPI;
  identity: PluginIdentityAPI;
  /** 事件模块：仅 iframe 桥模式可用（tab 模式未注入） */
  events?: PluginEventsAPI;
  /** 消息模块（应用会话）：仅 iframe 桥模式可用（tab 模式未注入） */
  messages?: PluginMessagesAPI;
}

// ------------------------------------------------------------------
// 插件运行上下文（桥握手 ready 时由宿主下发，见 bridge/protocol.ts）
// ------------------------------------------------------------------

/** 插件运行的空间容器（个人空间与组织并列的顶层容器） */
export type PluginSpaceContext = {
  type: 'personal' | 'org';
  /** 空间 id：个人空间为 'personal'，组织空间为 orgId */
  id: string;
};

/** 视图挂载信息（壳层分配；挂载区域矩形随宿主组件波次补充） */
export type PluginMountInfo = {
  /** 视图类型，对齐 manifest.views[].type */
  viewType: 'app' | 'message-card';
  /** 卡片 id（仅 message-card 视图）：壳层分配，动作回调与归属校验的凭据 */
  cardId?: string;
  /** 卡片视图数据（仅 message-card 视图）：应用消息 card.data 透传 */
  cardData?: unknown;
};

/** 宿主 srcdoc 注入的视图引导信息（`window.__sparkPluginView`）：
 *  插件握手前唯一能拿到 viewId/卡片上下文的途径（hello 的 viewId 必须与桥绑定一致） */
export type PluginViewBootstrap = {
  viewId: string;
  viewType: 'app' | 'message-card';
  cardId?: string;
  cardData?: unknown;
};

/** 桥握手 ready 下发的插件运行上下文 */
export type PluginContext = {
  pluginId: string;
  viewId: string;
  /** 插件域身份（plugin: 前缀） */
  domain: string;
  space: PluginSpaceContext;
  /** 壳层当前主题（变更经事件桥推送） */
  theme: 'light' | 'dark';
  mount: PluginMountInfo;
};

// ------------------------------------------------------------------
// 入口契约（definePlugin）
// ------------------------------------------------------------------

/** 插件视图声明（manifest.views 元素） */
export type PluginViewDeclaration = {
  id: string;
  type: 'app' | 'message-card';
  title?: string;
};

/** 插件声明式清单（与插件目录 manifest.json 一一对应） */
export type PluginManifest = {
  id: string;
  /** 插件域，必须以 'plugin:' 开头 */
  domain: string;
  name: string;
  version: string;
  description: string;
  /** 默认入口视图 id，必须存在于 views 中 */
  entryView: string;
  /** 插件可运行的空间类型 */
  supportedSpaces: Array<'personal' | 'org'>;
  views: PluginViewDeclaration[];
  /** 权限声明（如 storage:read / storage:write / org:read / org:sync） */
  permissions: string[];
  /** 依赖的 SDK 契约版本 */
  sdkVersion: string;
  package?: {
    updateManifestUrl: string;
    packageName: string;
  };
};

/** 插件 setup 上下文：sdk 为壳层注入的插件 SDK；registerView 注册视图组件 */
export type PluginSetupContext = {
  sdk: PluginSDK;
  registerView: (viewId: string, component: unknown) => void;
};

export type PluginDefinition = {
  manifest: PluginManifest;
  setup: (ctx: PluginSetupContext) => void;
};

/**
 * 插件入口契约：插件 index.ts 默认导出 definePlugin 的返回值。
 * 运行时为 identity 函数，仅做类型约束（第三方插件约定保留；壳层编译期
 * 装载已退役，沙箱 iframe 内插件经 bridge/client 握手自挂载）。
 */
export function definePlugin(def: PluginDefinition): PluginDefinition {
  return def;
}

// ------------------------------------------------------------------
// 全局注入点（插件入口在桥握手完成时写入）
// ------------------------------------------------------------------

declare global {
  interface Window {
    __sparkPluginSDK?: PluginSDK;
    /** 宿主 srcdoc 注入的视图引导信息（见 PluginViewBootstrap；握手前读取 viewId） */
    __sparkPluginView?: PluginViewBootstrap;
  }
}

/**
 * 获取壳层已注入的插件 SDK 实例
 *
 * @throws 如果壳层尚未注入（非插件上下文或注入未完成）
 */
export function getPluginSDK(): PluginSDK {
  const sdk = window.__sparkPluginSDK;
  if (!sdk) {
    throw new Error('Plugin SDK is not injected. Wait for the host injection (ensurePluginSDK) first.');
  }
  return sdk;
}

/**
 * 挂起等待宿主注入插件 SDK（插件视图 onMounted 时入口的桥握手可能尚未完成，
 * 轮询直至就绪）。
 *
 * @throws 超时（默认 10s）仍未注入，说明当前不处于插件运行上下文
 */
export function ensurePluginSDK(timeoutMs = 10_000, intervalMs = 50): Promise<PluginSDK> {
  const existing = window.__sparkPluginSDK;
  if (existing) {
    return Promise.resolve(existing);
  }

  return new Promise<PluginSDK>((resolve, reject) => {
    const startedAt = Date.now();
    const timer = setInterval(() => {
      const sdk = window.__sparkPluginSDK;
      if (sdk) {
        clearInterval(timer);
        resolve(sdk);
        return;
      }
      if (Date.now() - startedAt >= timeoutMs) {
        clearInterval(timer);
        reject(new Error('Plugin SDK injection timed out: not running in a plugin context.'));
      }
    }, intervalMs);
  });
}

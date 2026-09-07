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
  currentRoot: () => Promise<{
    unlocked: boolean;
    rootId: string | null;
    /** 当前身份昵称（未设置为 null；供插件初始化「我的资料」） */
    nickname?: string | null;
    /** 当前身份头像 data URL（无头像为 null） */
    avatar?: string | null;
    gender?: string | null;
    region?: string | null;
    signature?: string | null;
  }>;
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
      // 端点化：单设备退化为 `{deviceUid?, peerId?, addresses}` 对象，多设备为数组
      nodeInfo?:
        | { deviceUid?: string; peerId?: string; addresses: string[] }
        | Array<{ deviceUid?: string; peerId?: string; addresses: string[] }>;
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

/** P6 集合声明（wiki design/plugin-data-api.md §2）：策略随声明走，读写零同步参数 */
export interface PluginDataDeclaration {
  /** 集合名 `{pluginId}:{collection}`，前缀必须与插件 id 一致 */
  name: string;
  /** 代际标签（字符串，建议 semver；框架按声明时间定新旧），缺省 "1" */
  version?: string;
  /** sync（缺省）/ local；同步到自设备间还是组织内由内核按运行空间处理 */
  scope?: 'sync' | 'local';
  /** 持有账号内部驻留到哪类设备，缺省 all */
  devices?: 'all' | 'pc-backup' | 'pc-only' | 'mobile-only';
  /** 合并规则，缺省 lww-record */
  merge?: 'lww-record' | 'append-only' | 'whole';
  /**
   * 读授权门禁（read-gate §2，org scope 专有）：缺省省略 = members（组织成员可读，现状）。
   * 代际内不可变——改 readPolicy = 同名新 version 声明。
   */
  readPolicy?: PluginDataReadPolicy;
}

/** 读授权门禁声明（read-gate §2 readPolicy 线形；kind=credential 时 credTypes/verifierDomain 必填） */
export interface PluginDataReadPolicy {
  /** members（缺省=现状）/ public（公开发布无需凭证）/ credential（持凭证放行） */
  kind: 'members' | 'public' | 'credential';
  /** 放行的凭证类型集（kind=credential 必填，任一匹配） */
  credTypes?: string[];
  /** 验证人信任声明所在域 orgId（kind=credential 必填） */
  verifierDomain?: string;
  /** B1 策略文档哈希（字段级掩码等细化规则）；缺省 null = 凭证类型匹配即可读全集合 */
  policyRef?: string | null;
}

/** P6 声明式数据 API（personal scope 已落地；org 轴随组织同步架构启用） */
export interface PluginDataAPI {
  /** 声明集合（幂等；代际内策略冲突报错）。返回声明记录 */
  declareCollection: (declaration: PluginDataDeclaration) => Promise<Record<string, unknown>>;
  /** 写记录（version 缺省 = 最新代际；写库即同步） */
  save: (name: string, key: string, value: unknown, version?: string) => Promise<{ success: boolean }>;
  /** 删记录（墓碑传播到复制组） */
  delete: (name: string, key: string, version?: string) => Promise<{ success: boolean }>;
  /** 读单条（未命中 → null） */
  get: <T = unknown>(name: string, key: string, version?: string) => Promise<T | null>;
  /** 前缀分页查询（limit 缺省 500 上限 2000；返回集合内相对键） */
  query: <T = unknown>(
    name: string,
    options?: { prefix?: string; limit?: number; cursor?: string },
    version?: string
  ) => Promise<{ items: Array<{ key: string; value: T }>; nextCursor?: string }>;
  /** 清理一个代际（声明 + 全部数据键墓碑化传播） */
  dropVersion: (name: string, version: string) => Promise<{ success: boolean }>;
  /** 内建 blob：base64 入、{hash,size} 出；记录内以 { $blob: hash, name, size, mime } 引用 */
  saveBlob: (dataBase64: string) => Promise<{ hash: string; size: number }>;
  /** 命中 → {status:'ready', data(base64)}；未命中 → {status:'pending'}（已登记拉取意图，稍后重读） */
  readBlob: (hash: string) => Promise<{ status: 'ready'; data: string } | { status: 'pending' }>;
  /**
   * 远端合入本插件集合（pdoc/pdecl）时回调（iframe 桥侧封装
   * spark.events.subscribe('PluginDataChanged')，已按 pluginId 过滤；
   * QuickJS 后台运行时的同名 API 走内核插件路由任务）。
   * 本地写不触发（本地路径即时可见）。
   */
  onChange: (handler: (event: { pluginId: string; name: string; keys: string[] }) => void) => Promise<void>;
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
 * 消息模块：统一的消息收发入口，支持两种交互模式：
 *
 * - 服务号（应用会话）：sendAppMessage / listAppMessages / markRead / 卡片回调
 *   → 消息以结构化卡片形式出现在"应用"分组中，适合通知/播报场景；
 *
 * - 插件联系人：registerAsContact / unregisterAsContact / sendResponse
 *   → 插件注册为 Spark 通讯录联系人，出现在"单聊"分组中，
 *     用户可像与真人一样自由打字对话。bot 消息的接收在内核 QuickJS 后台
 *     运行时（manifest background 入口，spark.onMessage 推送模型）；
 *     iframe 侧只保留注册/回复能力。
 *
 * 两种模式共享 `message:app` 权限（高级权限，内核限流 10 条/60s）。
 * pluginId/space 由桥按已认证身份注入，插件侧不传。
 * 仅 iframe 桥模式可用，故在 PluginSDK 上为可选字段（同 events）。
 */
export interface PluginMessagesAPI {
  // ── 服务号（应用会话） ──

  /** 写入应用消息：payload 必须含非空字符串 summary（trim 后 ≤200 字符，超限拒绝） */
  sendAppMessage: (payload: Record<string, unknown>, card?: PluginAppMessageCard) => Promise<PluginAppMessage>;
  /** 本插件在当前空间的应用消息（时间升序） */
  listAppMessages: () => Promise<PluginAppMessage[]>;
  /** 清零本插件应用会话未读（语义与人际会话一致） */
  markRead: () => Promise<{ success: boolean }>;
  /** 注册卡片按钮回调（主视图），返回注销函数 */
  onCardAction: (handler: (action: PluginCardActionPayload) => void) => () => void;
  /** 触发卡片按钮回调（仅 message-card 视图） */
  triggerCardAction: (actionId: string, data?: unknown) => void;
  /** 申请卡片高度（仅 message-card 视图，壳层封顶 400px） */
  requestCardHeight: (height: number) => void;

  // ── 插件联系人（注册为通讯录联系人，直接收发消息） ──

  /** 将插件注册为 Spark 通讯录联系人（contactId 为全限定标识，由插件自行构造） */
  registerAsContact: (contactId: string, displayName: string) => Promise<{ success: boolean }>;
  /** 注销插件联系人（删除对应通讯录好友记录；插件删除 bot 时调用） */
  unregisterAsContact: (contactId: string) => Promise<{ success: boolean }>;
  /** 向联系人会话插入一条回复消息（senderId = contactId，内核直接落库不经 P2P） */
  sendResponse: (convId: string, contactId: string, displayName: string, messageId: string, text: string) => Promise<unknown>;
  // waitForMessage（长轮询收 bot 消息）已下线：iframe 侧不再消费 bot 消息，
  // 由内核后台运行时按 bot 归属直接推送（plugin_system.md「后台运行时」）
}

// ------------------------------------------------------------------
// 通讯录模块（社交投递层 social-feed §9.4 `contact:read` 最小只读面）
// ------------------------------------------------------------------

/** 朋友只读摘要（相对 FriendDto 裁剪：剔除签名/性别/电话/备忘/照片/设备寻址等）。 */
export type PluginFriendSummary = {
  rootId: string;
  nickname: string;
  /** 无头像时缺省键不出现 */
  avatar?: string;
  /** 所属分组 id；'' = 未分组 */
  groupId: string;
  tagIds: string[];
  permission: 'open' | 'chatOnly';
};

/** 通讯录标签（个人空间扁平分组，数组顺序即显示顺序）。 */
export type PluginContactTag = {
  id: string;
  name: string;
};

/** 个人空间分组（扁平一层，数组顺序即显示顺序）。 */
export type PluginContactGroup = {
  id: string;
  name: string;
};

/**
 * 通讯录只读模块：`contact:read` 权限（高级 + 使用时询问）。
 * 仅 iframe 桥模式可用，故在 PluginSDK 上为可选字段（同 events/messages）。
 * 返回字段均经内核只读门面裁剪，不暴露签名等敏感资料。
 */
export interface PluginContactsAPI {
  /** 所有朋友的只读摘要（「谁可以看」选择器依赖） */
  listFriends: () => Promise<PluginFriendSummary[]>;
  /** 所有分组（按 order 升序） */
  listGroups: () => Promise<PluginContactGroup[]>;
  /** 所有标签（按 order 升序） */
  listTags: () => Promise<PluginContactTag[]>;
}

// ------------------------------------------------------------------
// 社交投递模块（social-feed §9，sdk.feed 域）
// ------------------------------------------------------------------

/** 单条 feed 消息（收件箱 pull / 在线推送 onReceive 共用形状，§9.1）。 */
export type FeedMessage = {
  feedId: string;
  /** 发送方 rootId（信封 from） */
  from: string;
  /** topic（`{pluginId}:{sub}`，前缀即插件归属） */
  topic: string;
  /** 业务 payload（明文，解密后分发） */
  payload: unknown;
  /** 回执语义（指向原 feedId）；可省 */
  replyTo?: string;
  /** 信封时间戳（ms，发送方时间；I6 后落库/事件用信封 ts 而非接收方本地时间） */
  ts: number;
};

/**
 * 社交定向投递模块（social-feed §9.1）。`feed:deliver` 权限（高级 + 内核
 * 限流）；接收侧（onReceive/pull）免权限。topic 前缀必须 == 本插件 id（出站
 * 侧由桥 dispatcher / 壳层校验）。仅 iframe 桥模式可用，故在 PluginSDK 上为
 * 可选字段（同 events/messages）。
 */
export interface PluginFeedAPI {
  /**
   * 定向投递（§9.1 deliver）：把 payload 投递给 rootId 名单。
   * 返回 `{requested, accepted}`（被拉黑/仅聊天/非朋友静默跳过，不计 accepted）。
   * 收件人须为本机朋友（否则静默跳过）。payload 紧凑序列化 ≤ 32 KiB。
   */
  deliver: (input: {
    /** `{pluginId}:{sub}`，前缀须等于本插件 id */
    topic: string;
    payload: unknown;
    /** rootId 名单，≤ 500 */
    recipients: string[];
    /** 回执语义：原数据 feedId */
    replyTo?: string;
    /** 可省，缺省壳层生成 */
    feedId?: string;
  }) => Promise<{ requested: number; accepted: number }>;

  /** 订阅收件（§9.1 onReceive；在线推送，topic 前缀匹配；接收侧免权限） */
  onReceive: (
    topic: string,
    handler: (msg: FeedMessage) => void
  ) => Promise<void>;

  /** 补读收件箱（§9.1 pull；启动/恢复路径，接收侧免权限） */
  pull: (input: {
    topic: string;
    cursor?: string;
    limit?: number;
  }) => Promise<{ items: FeedMessage[]; nextCursor?: string }>;
}

// ------------------------------------------------------------------
// 内容面模块（public-topics §七「持有即做种」，sdk.content 域）
// ------------------------------------------------------------------

/** 内容面 blob 信息（saveBlob 返回；cid = SHA-256 hex，64 位小写） */
export type PluginContentBlobInfo = {
  cid: string;
  size: number;
};

/**
 * 内容面 blob 模块（public-topics §七）：内容寻址存储 + Kad provider
 * 「持有即做种」。与 `data.saveBlob`/`data.readBlob`（pdsync 面，同身份自
 * 设备间附件同步）是两个互不复用的存储区：本模块是跨主体内容面——保存
 * 即声明 provider，fetchBlob 本地未命中时经 Kad 检索 provider 并直连拉回
 * 本体（接收侧 CID 哈希校验，hash 即能力）。
 * blob 本体一律 base64 出入。pinRoot/unpinRoot 管理 GC 根标记（root 为
 * 持有理由标签，如 `topic:{topicId}`），无根 blob 经宽限期后由 gcSweep
 * 两段式回收（回收即停止做种）。
 * 仅 iframe 桥模式可用，故在 PluginSDK 上为可选字段（同 events/messages）。
 */
export interface PluginContentAPI {
  /** 保存 blob（base64 入，幂等；同内容同 cid）并声明 provider */
  saveBlob: (dataBase64: string) => Promise<PluginContentBlobInfo>;
  /** 本地读取（命中 → base64；未命中 → null，不触发网络拉取） */
  readBlob: (cid: string) => Promise<string | null>;
  /** 按 cid 取 blob：本地未命中时经 Kad provider 逐台拉取；全部失败 → null */
  fetchBlob: (cid: string) => Promise<string | null>;
  /** 本地持有的全部 blob cid（升序） */
  listBlobs: () => Promise<string[]>;
  /** 打 GC 根标记（root 为持有理由标签，如 `topic:{topicId}`、`user-pin`） */
  pinRoot: (cid: string, root: string) => Promise<{ success: boolean }>;
  /** 移除一个 GC 根标记；最后一个根移除后进入宽限期回收路径 */
  unpinRoot: (cid: string, root: string) => Promise<{ success: boolean }>;
  /** 无根 blob 两段式回收（每回收一个即同步停止做种）；返回回收的 cid 列表 */
  gcSweep: () => Promise<string[]>;
}

// ------------------------------------------------------------------
// 共同体事务模块（community-affairs §7.2 sdk.affairs：内核 affair 门面的
// 类型化暴露；决议/阶梯只从链上锚定时间确定性推导，插件伪造不了）
// ------------------------------------------------------------------

// 事务间引用（affair.md §10）与创世线形构造：类型自 affair-wire 模块
// re-export（运行实现同处，sdk.affairs.create 与插件共用一份协议线形代码）。
// 注：export type ... from 不带入本地作用域，接口签名内引用需另行 import type。
import type { AffairGenesisInput } from './affair-wire';
export type {
  AffairActor,
  AffairGenesisInput,
  AffairRef,
  AffairRefRel
} from './affair-wire';

/** 提交操作的判定状态（与复制面入站同口径：未知指向持久暂存为 pending） */
export type AffairOpStatus = 'accepted' | 'pending' | 'duplicate';

/**
 * 事务变更事件（sdk.affairs.onChange 载荷）：本地副本在关注/取关/本地提交/
 * 复制面入站合入后由内核发出（P2pEvent::AffairChanged → 桥事件）。
 * 变更通知不是可靠队列（重启/慢订阅会丢），收到后应重读 readLog 收敛——
 * 与 data.onChange（PluginDataChanged）同口径。
 */
export type AffairChangeEvent = {
  affairId: string;
  change: 'followed' | 'unfollowed' | 'submitted' | 'replicated';
  /** submitted：本条操作哈希 */
  opHash?: string;
  /** submitted：本条操作判定状态 */
  status?: AffairOpStatus;
  /** replicated：本批接受/暂存补齐条数 */
  accepted?: number;
  drained?: number;
};

/** 操作日志条目（opHash 字典序，§8 排序键） */
export type AffairLogEntry = {
  opHash: string;
  op: Record<string, unknown>;
};

/** 本地副本日志（sdk.affairs.readLog）：创世 + 已接受操作 + DAG 头 + 关注状态 */
export type AffairLog = {
  affairId: string;
  /** 创世记录（本地完全未知的事务为 null） */
  genesis: Record<string, unknown> | null;
  ops: AffairLogEntry[];
  heads: string[];
  /** 关注时刻；未关注为 null */
  followedAt: number | null;
};

/** 决议公示期状态（§6.2 两态 + unanchored：未锚定不用声明时间冒充链上时间） */
export type AffairResolutionState = 'pending' | 'effective' | 'vetoed' | 'unanchored';

/** 单条决议（opType=resolution 的操作 + 公示期状态） */
export type AffairResolution = {
  opHash: string;
  result: unknown;
  condition: unknown;
  countedOps: unknown;
  rulesHash: string;
  pubPeriodMs: number;
  /** 本副本存证链锚定时刻；未锚定为 null */
  anchoredMs: number | null;
  objections: number;
  state: AffairResolutionState;
};

/** 决议集合（sdk.affairs.readResolution） */
export type AffairResolutions = {
  affairId: string;
  resolutions: AffairResolution[];
};

/** 阶梯名册条目（§5.5 一人一票，不加权；账龄从链上最早活跃推导） */
export type AffairLadderEntry = {
  identity: string;
  /** observer / contributor / voter（§5.5 取值） */
  tier: 'observer' | 'contributor' | 'voter';
  /** 累计采纳次数（生效） */
  accepts: number;
  /** 进入当前级别时刻（ms）；从未晋级为 null */
  tierSinceMs: number | null;
  /** 最近活跃时刻（ms）；无活跃为 null */
  lastActivityMs: number | null;
  /** 账龄（ms，链上最早活跃至 now）；无链上记录为 null */
  accountAgeMs: number | null;
};

/** 阶梯/账龄状态（sdk.affairs.ladderStatus；时间源只认链上锚定时刻） */
export type AffairLadderStatus = {
  affairId: string;
  nowMs: number;
  entries: AffairLadderEntry[];
  /** 当前有投票权的身份集合 */
  voters: string[];
};

// ------------------------------------------------------------------
// 规则版本链 / 快照 / 执行状态 / 组织效力（sdk.affairs 读出口与效力编排）
// ------------------------------------------------------------------

/** 规则文档单个版本（§5.4「每一版本确定性可溯」） */
export type AffairRulesVersion = {
  seq: number;
  /** 版本依据（创世 = affairId；rule-change = 依据操作 opHash） */
  basisOpHash: string;
  rulesHash: string;
  /** 生效时刻（链上锚定 ms）；未生效为 null */
  effectiveMs: number | null;
};

/** 未生效 rule-change 条目的归宿（pending / rejected + 稳定 reason） */
export type AffairRuleChangeFate = {
  opHash: string;
  fate: 'pending' | 'rejected';
  reason: string;
};

/** 规则文档版本链（sdk.affairs.readRules）：现行版本 = 创世规则 + 已生效 rule-change 链 */
export type AffairRulesView = {
  affairId: string;
  nowMs: number;
  current: {
    seq: number;
    rulesHash: string;
    /** 现行规则文档原文（JSON，插件语义参数内核不解释） */
    rules: Record<string, unknown>;
  };
  versions: AffairRulesVersion[];
  changes: AffairRuleChangeFate[];
};

/** 阶梯快照 payload（sdk.affairs.snapshotPayload，§9 投票前快照的获取侧） */
export type AffairSnapshotPayload = {
  affairId: string;
  /** 供插件签名后以 snapshot 操作提交的 payload */
  payload: { basis: string; asOf: string; rosterHash: string };
  /** 名册原文（透明呈现） */
  roster: string[];
  /** asOf 切口的链上锚定时刻（ms） */
  asOfAnchoredMs: number;
};

/** 执行型事务状态机状态（§6.2-3 八态，与内核 ExecState::as_str 逐字对齐） */
export type AffairExecStateName =
  | 'unanchored'
  | 'resolution-pending'
  | 'resolution-vetoed'
  | 'awaiting-execution'
  | 'in-progress'
  | 'verifying'
  | 'returned'
  | 'closed';

/** 单决议的执行状态（derive_exec_states 推导结果） */
export type AffairExecState = {
  resolutionOpHash: string;
  state: AffairExecStateName;
  /** 最新有效执行回报 opHash；无回报为 null */
  reportOpHash: string | null;
  /** 决议链上锚定时刻（ms）；未锚定为 null */
  anchoredMs: number | null;
  /** 决议生效时刻（ms）；未生效为 null */
  effectiveMs: number | null;
};

/** 执行状态读出口（sdk.affairs.readExec）；exec == null 的事务 states 为空、exec 为 null */
export type AffairExecView = {
  affairId: string;
  nowMs: number;
  /** 现行规则版本的 exec 声明原文；非执行型事务为 null */
  exec: unknown;
  /** vote 核查名册大小（执行型事务才有） */
  rosterSize?: number;
  states: AffairExecState[];
};

/** 组织效力判定结果（org-genesis §6 三线判定；与内核 EffectHookOutcome 逐字对齐） */
export type AffairEffectOutcome =
  | 'apply'
  | 'notDeclared'
  | 'revoked'
  | 'resolutionNotEffective'
  | 'grantNotAnchored'
  | 'resolutionNotAnchored'
  | 'notPrior';

/** 待应用效力事件（outcome = apply 时携带） */
export type AffairPendingEffect = {
  orgId: string;
  affairId: string;
  resolutionOpHash: string;
  scope: string;
  grantKey: string;
};

/** 效力回执状态标注（apply_org_effects 的消费留痕读口） */
export type AffairEffectReceipt = {
  /** recorded = 同键回执且决议一致；unrecorded = 待消费 */
  state: 'recorded' | 'unrecorded';
  receiptKey: string;
};

/** 单条效力判定行（声明 × 决议） */
export type AffairOrgEffect = {
  scope: string;
  grantKey: string;
  resolutionOpHash: string;
  outcome: AffairEffectOutcome;
  pendingEffect?: AffairPendingEffect;
  receipt?: AffairEffectReceipt;
};

/** 决议组织效力钩子读出口（sdk.affairs.orgEffects） */
export type AffairOrgEffects = {
  orgId: string;
  affairId: string;
  nowMs: number;
  effects: AffairOrgEffect[];
  /** 复算无效被剔除的决议 opHash（§6.1 无效集，不产生效力） */
  invalidResolutions: string[];
};

/** 回执编排逐条动作（sdk.affairs.applyOrgEffects 返回的 actions 条目） */
export type AffairOrgEffectApplyAction = {
  scope: string;
  resolutionOpHash: string;
  /** recorded / already-recorded / superseded / skipped（幂等 + 链上时间 LWW） */
  action: 'recorded' | 'already-recorded' | 'superseded' | 'skipped';
  /** skipped 时的判定归宿（如 superseded-by-newer 或非 apply 的 outcome） */
  outcome?: string;
  receiptKey?: string;
};

/** 待应用效力事件的消费编排结果（sdk.affairs.applyOrgEffects） */
export type AffairOrgEffectsApplyResult = {
  orgId: string;
  affairId: string;
  nowMs: number;
  actions: AffairOrgEffectApplyAction[];
  invalidResolutions: string[];
};

/** 单事务履历分量（sdk.affairs.publicProfile 的 perAffair 条目） */
export type AffairProfileStats = {
  affairId: string;
  /** 该身份在本事务的 person 操作数（全类型） */
  opCount: number;
  /** 提议数（meta-revise + rule-change，全机制） */
  proposals: number;
  /** 采纳数（affair §13 口径：delayed-veto 生效者；vote/multisig 生效不计） */
  adoptions: number;
  /** 内核级表决票总数 / 其中 yes / 其中 no */
  votes: number;
  votesYes: number;
  votesNo: number;
  /** 本事务内最早 / 最近链上活跃时刻（ms，只认锚定时刻）；无 = null */
  firstActivityMs: number | null;
  lastActivityMs: number | null;
};

/** 投票历史条目（内核级表决票，affair §4 vote 操作） */
export type AffairProfileVote = {
  affairId: string;
  opHash: string;
  /** 表决目标（rule-change / meta-revise 提议 opHash） */
  proposal: string;
  choice: 'yes' | 'no';
  /** 本副本存证链锚定时刻；未锚定为 null */
  anchoredMs: number | null;
};

/**
 * 公开履历聚合视图（sdk.affairs.publicProfile；community-affairs §7.3/§10
 * 决策 4：内核确定性聚合，同一查询任何节点对同一副本集合复算一致）。
 * 本地副本所见：未关注/未复制到的事务不参与聚合（诚实边界）。
 */
export type AffairPublicProfile = {
  identity: string;
  nowMs: number;
  /** 参与的事务数（≥1 条 person 操作） */
  affairsParticipated: number;
  /** 跨事务最早链上活跃时刻（ms）；无已锚定活动为 null */
  firstActivityMs: number | null;
  /** 账龄（ms）= nowMs − firstActivityMs；无链上活动为 null */
  accountAgeMs: number | null;
  /** 提议/采纳精确计数对（采纳率 = adoptions/proposals，呈现归客户端） */
  proposals: number;
  adoptions: number;
  /** 跨事务内核级表决票总数 / yes / no */
  votes: number;
  votesYes: number;
  votesNo: number;
  /** 单事务分量，按 affairId 字典序 */
  perAffair: AffairProfileStats[];
  /** 逐票历史，按（affairId, 锚定时刻, opHash）排序 */
  voteHistory: AffairProfileVote[];
};

/**
 * 共同体事务模块（community-affairs §7.2 sdk.affairs）。
 * 关注/取关/提交操作/回执编排（applyOrgEffects）须 `affairs:write`（高级），
 * 只读查询须 `affairs:read`。
 * affairId 一律以创世记录自认证复算为准，插件不传不猜。
 * 仅 iframe 桥模式可用，故在 PluginSDK 上为可选字段（同 events/messages）。
 */
export interface PluginAffairsAPI {
  /**
   * 创建事务（affairs:write；插件内嵌创建流的 SDK 承载）：按类型化描述
   * 构造创世记录（线形见 affair-wire 模块）→ 插件域身份签名 → follow
   * （内核全链校验 + affairId 自认证复算）。返回 affairId 与已落库的
   * 签名创世记录（供调用方展示/转发关注）。refs 走 §10 类型化枚举，
   * 形状非法在签名前拒绝；自指禁令由内核 enforced。
   */
  create: (input: AffairGenesisInput) => Promise<{ affairId: string; genesis: Record<string, unknown> }>;
  /** 关注事务（genesis 创世记录全链校验；返回自认证 affairId） */
  follow: (genesis: Record<string, unknown>) => Promise<string>;
  /** 取关（只删关注簿记，保留已复制数据） */
  unfollow: (affairId: string) => Promise<void>;
  /** 本机关注的事务 id 列表（字典序） */
  listFollowed: () => Promise<string[]>;
  /** 提交一条操作（须先关注；与复制面入站同一校验链） */
  submitOp: (op: Record<string, unknown>) => Promise<{ affairId: string; opHash: string; status: AffairOpStatus }>;
  /** 读本地副本操作日志 */
  readLog: (affairId: string) => Promise<AffairLog>;
  /** 读规则文档版本链（现行版本 + 未生效条目归宿，§5.4 确定性可溯） */
  readRules: (affairId: string) => Promise<AffairRulesView>;
  /** 读决议（公示期状态按链上锚定时刻推导） */
  readResolution: (affairId: string) => Promise<AffairResolutions>;
  /** 阶梯/账龄状态（确定性推导） */
  ladderStatus: (affairId: string) => Promise<AffairLadderStatus>;
  /** 公开履历聚合（公共身份跨事务账龄/采纳/投票历史，确定性推导） */
  publicProfile: (identity: string) => Promise<AffairPublicProfile>;
  /**
   * 阶梯快照 payload 生产助手（§9 投票前快照的获取侧）：asOf 缺省 = 当前
   * 最大 opHash（无操作 = 创世切口）；返回 payload 供插件签名后以 snapshot
   * 操作提交。asOf 未知/未锚定 → 报错（fail-closed）。
   */
  snapshotPayload: (affairId: string, asOf?: string) => Promise<AffairSnapshotPayload>;
  /** 执行型事务状态读出口（八态状态机推导；exec == null 的事务返回空状态集） */
  readExec: (affairId: string) => Promise<AffairExecView>;
  /**
   * 决议组织效力钩子（org-genesis §6）：对 (orgId, affairId) 逐声明 × 逐有效
   * 决议求值三线，产出待应用事件并标注回执状态（recorded/unrecorded）。
   * 只产出事件，不做名册/策略应用。
   */
  orgEffects: (orgId: string, affairId: string) => Promise<AffairOrgEffects>;
  /**
   * 待应用效力事件的消费编排（affairs:write）：对 orgEffects 判为 Apply 的
   * 事件写 org:effectrcpt: 回执并逐条存证（幂等；同 scope 多决议并存时回执
   * 只跟踪最新决议——链上时间 LWW）。**名册/策略内容的实际变更不在内核**——
   * 调用方（插件/组织侧）须先完成内容应用再调用本方法写回执凭据。
   */
  applyOrgEffects: (orgId: string, affairId: string) => Promise<AffairOrgEffectsApplyResult>;
  /**
   * 订阅本机事务副本变更（AffairChanged 桥事件；变更通知非可靠队列，
   * 收到后重读 readLog 收敛）。替代视图轮询/手动刷新。
   */
  onChange: (handler: (event: AffairChangeEvent) => void) => Promise<void>;
}

// ------------------------------------------------------------------
// 资格凭证模块（community-affairs §7.2 sdk.credentials；无签发接口——
// 签发走验证插件的人机流程，内核只验格式与签名）
// ------------------------------------------------------------------

/** 凭证身份引用（credential §2） */
export type PluginCredentialIdentityRef = {
  kind: string;
  identity: string;
  publicKey: string;
};

/** 凭证持有者引用 */
export type PluginCredentialHolderRef = {
  kind: string;
  identity: string;
  publicKey: string;
};

/** 持有凭证（credential §2 线形；claims 最小披露，禁止身份标识） */
export type PluginCredential = {
  credV: number;
  credType: string;
  issuer: PluginCredentialIdentityRef;
  holder: PluginCredentialHolderRef;
  subjectDomain: string;
  claims: Record<string, unknown>;
  /** 核验方式标识（验证插件 id + 方法名） */
  method: string;
  linkRef: string | null;
  issuedAt: number;
  sig: string;
};

/** 本机持有凭证条目（sdk.credentials.listHeld；cred:held: 键域） */
export type HeldCredential = {
  credId: string;
  credential: PluginCredential;
};

/** 持有证明（read-gate §3 载荷的域身份签名；域私钥不出内核） */
export type HolderProof = {
  credId: string;
  sig: string;
};

/** holderProof 呈现结果（sdk.credentials.presentHolderProof） */
export type HolderProofPresentation = {
  credential: PluginCredential;
  holderProof: HolderProof;
  /** 呈现时刻（ms） */
  presentedAt: number;
};

/** 验证人授权条目（org:verifiers: 信任声明 verifiers[]） */
export type VerifierGrant = {
  identity: string;
  publicKey: string;
  credTypes: string[];
  /** 授权方法模式集；尾部 * 为前缀通配 */
  methods: string[];
};

/** 验证人信任声明（sdk.credentials.queryVerifiers） */
export type VerifierSet = {
  orgId: string;
  effectiveFrom: number;
  seq: number;
  updatedAt: number;
  verifiers: VerifierGrant[];
};

/** 注销检查三态（快照缺失/链无效 = unavailable，fail-closed，与 read-gate 同口径） */
export type CredentialRevocationStatus = 'not-revoked' | 'revoked' | 'unavailable';

/**
 * 凭证验证裁决（sdk.credentials.verify；credential §6 第 1–5 步逐段结果）。
 * 结构化返回而非整体报错——凭证来自不可信来源，逐项失败原因如实回显；
 * valid=true 当且仅当静态链全过、签发人信任链匹配且注销检查明确通过。
 * reason 为首个失败段的稳定诊断码（内核 CredentialError::kind 逐字）。
 */
export type CredentialVerifyResult = {
  /** 复算的 credId（静态链不过为 null） */
  credId: string | null;
  valid: boolean;
  checks: {
    /** 结构 + credId 复算 + 验签（不随时间变化） */
    static: boolean;
    /** 签发人信任链（org:verifiers: 声明，按 issuedAt 时刻） */
    trust: boolean;
    revocation: CredentialRevocationStatus;
  };
  reason: string | null;
};

/** 注销条目视图（sdk.credentials.queryRevocations） */
export type RevocationEntryView = {
  seq: number;
  credId: string;
  revokedAt: number;
  reason: string | null;
};

/**
 * 注销快照视图（sdk.credentials.queryRevocations）。available:false = 本地
 * 无该 issuer 快照（分发承载面未定，快照由分发渠道落地后写入）——如实报告，
 * 不冒充「无注销」（fail-closed 取舍归消费方）。
 */
export type RevocationSnapshotView =
  | { issuer: string; available: false }
  | {
      issuer: string;
      available: true;
      headSeq: number;
      headHash: string;
      asOf: number;
      entries: RevocationEntryView[];
    };

/**
 * 资格凭证模块（community-affairs §7.2 sdk.credentials）。
 * 只读持有凭证/验证人/验证/注销查询须 `credentials:read`；presentHolderProof
 * 的域身份由桥按绑定身份注入（插件不自报），凭证持有者的公钥必须与该域
 * 身份一致。无签发接口。仅 iframe 桥模式可用（同 events/messages）。
 */
export interface PluginCredentialsAPI {
  /** 本机持有的凭证列表（credId 字典序） */
  listHeld: () => Promise<HeldCredential[]>;
  /** 呈现 holderProof：静态校验持有凭证后用本插件域身份签 read-gate §3 载荷 */
  presentHolderProof: (input: {
    credId: string;
    requestId: string;
    orgId: string;
    collection: string;
  }) => Promise<HolderProofPresentation>;
  /** 查询某组织的验证人信任声明（缺失返回空集，结构损坏报错） */
  queryVerifiers: (orgId: string) => Promise<VerifierSet>;
  /** 验证协议线形凭证（§6 第 1–5 步结构化裁决；holderProof 绑定归 read-gate） */
  verify: (credential: PluginCredential) => Promise<CredentialVerifyResult>;
  /** 按 issuer identity 查询本地注销快照（缺失如实报 available:false） */
  queryRevocations: (issuer: string) => Promise<RevocationSnapshotView>;
}

// ------------------------------------------------------------------
// 策略模块（community-affairs §7.2 sdk.policy：策略插件只产出声明式文档，
// B1 求值器在内核；本面只有本地草稿的读取与提交）
// ------------------------------------------------------------------

/** 静态分析发现项（severity 字面量对齐内核线形） */
export type PolicyFinding = {
  severity: 'error' | 'warning';
  code: string;
  detail: string;
};

/** 本地策略草稿（sdk.policy.read；无草稿为 null） */
export type PolicyDraft = {
  doc: Record<string, unknown>;
  policyDocHash: string;
  savedAt: number;
};

/** 草稿提交结果（sdk.policy.submitDraft） */
export type PolicySubmitResult = {
  policyDocHash: string;
  findings: PolicyFinding[];
};

/**
 * 发布结果（sdk.policy.publish）：草稿附组织签名包（OrgSigSet）落
 * `org:policydoc:` 键域（org:structure@v1，随组织同步分发，入站合入以
 * 同一五步链把关）。degraded=true 表示 legacy 组织的降级证明
 * （org-signature §5.1，信任裁决归消费方）。
 */
export type PolicyPublishResult = {
  orgId: string;
  policyDocHash: string;
  publishedAt: number;
  /** 签名主体（本机 root 身份 id，须为名册 admin） */
  signer: string;
  degraded: boolean;
};

/**
 * 策略模块（community-affairs §7.2 sdk.policy）。读草稿须 `policy:read`，
 * 提交草稿与发布须 `policy:write`（高级，管理员授权面）。
 * 提交 = 结构/引擎校验（非 b1 拒绝）+ §5 静态分析 + 落本地草稿键
 * （policy:draft: 本地工作副本，不进同步流量）；
 * 发布 = 草稿附 OrgSigSet 落 org:policydoc: 同步键域（本机须为名册
 * admin；AnyAdmin 单签自足，m-of-n 多签收集流不在本面，如实报错）。
 * 仅 iframe 桥模式可用（同 events/messages）。
 */
export interface PluginPolicyAPI {
  /** 读本组织最新本地策略草稿（无草稿为 null） */
  read: (orgId: string) => Promise<PolicyDraft | null>;
  /** 提交策略文档草稿：返回 policyDocHash 与静态分析 findings */
  submitDraft: (doc: Record<string, unknown>) => Promise<PolicySubmitResult>;
  /** 发布本地草稿：附组织签名包落同步键域（重复发布同文档 = 幂等覆写） */
  publish: (orgId: string) => Promise<PolicyPublishResult>;
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

// ── sys 模块：内核代理执行命令 / HTTP 请求 ──

/** sys.exec 返回 */
export type SysExecResult = {
  stdout: string;
  stderr: string;
  exitCode: number;
};

/** sys.fetch 选项 */
export type SysFetchOptions = {
  method?: string;
  headers?: Record<string, string>;
  body?: string;
};

/** sys.fetch 返回 */
export type SysFetchResult = {
  status: number;
  headers: Record<string, string>;
  body: string;
};

/** sys.fetchStream 流式响应块 */
export type SysFetchChunk = {
  text: string;
  done: boolean;
  status: number;
  headers: Record<string, string>;
};

/** sys.fetchStream 返回的流控制句柄 */
export interface FetchStreamHandle {
  streamId: string;
  /** 完成的 Promise（done=true 时 resolve，异常时 reject） */
  done: Promise<SysFetchChunk>;
  /** 注册块回调（每次数据到达时调用） */
  onChunk: (handler: (chunk: SysFetchChunk) => void) => void;
  /** 取消流（取消订阅，后继块不再触发） */
  cancel: () => void;
}

/** 系统代理 API：插件通过内核代理执行外部命令 / 发起 HTTP 请求，绕过浏览器沙箱限制 */
export interface PluginSysAPI {
  /**
   * 执行外部命令。workdir：可选工作目录——CLI 工具（如 codebuddy 读当前目录
   * 上下文）对 cwd 敏感，缺省继承宿主进程 cwd（不可控），应由插件显式指定。
   */
  exec: (program: string, args: string[], workdir?: string) => Promise<SysExecResult>;
  fetch: (url: string, options?: SysFetchOptions) => Promise<SysFetchResult>;
  /** 发起 HTTP 流式请求。每个文本块到达时通过 onChunk 回调推送，done 的 Promise 在流结束时 resolve。 */
  fetchStream: (url: string, options?: SysFetchOptions) => Promise<FetchStreamHandle>;
  /**
   * 打开操作系统目录选择对话框，返回所选目录的绝对路径；用户取消返回 null。
   * 用于让用户图形化选目录（如 CLI 工作目录），替代手动输入路径。
   */
  pickFolder: (title?: string) => Promise<string | null>;
}

export interface PluginSDK {
  /** 当前插件的域身份：tab 模式下由 URL query `pluginDomain` 解析（对齐旧 tab 语义） */
  domain: string;
  /** 请求壳层关闭当前插件视图（退出插件返回来源页）。仅 iframe 桥模式可用 */
  close: () => Promise<void>;
  evidence: PluginEvidenceAPI;
  p2p: PluginP2PAPI;
  runtime: PluginRuntimeAPI;
  docs: PluginDocAPI;
  /** P6 声明式数据 API（写库即同步；iframe 桥与 QuickJS 后台运行时均可用） */
  data: PluginDataAPI;
  identity: PluginIdentityAPI;
  /** 事件模块：仅 iframe 桥模式可用（tab 模式未注入） */
  events?: PluginEventsAPI;
  /** 消息模块（服务号 + Bot 联系人）：仅 iframe 桥模式可用（tab 模式未注入） */
  messages?: PluginMessagesAPI;
  /** 通讯录只读模块（social-feed §9.4 contact:read）：仅 iframe 桥模式可用 */
  contacts?: PluginContactsAPI;
  /** 社交投递模块（social-feed §9.1 sdk.feed：deliver/onReceive/pull）：仅 iframe 桥模式可用 */
  feed?: PluginFeedAPI;
  /** 内容面 blob 模块（public-topics §七「持有即做种」sdk.content）：仅 iframe 桥模式可用 */
  content?: PluginContentAPI;
  /** 共同体事务模块（community-affairs §7.2 sdk.affairs）：仅 iframe 桥模式可用 */
  affairs?: PluginAffairsAPI;
  /** 资格凭证模块（community-affairs §7.2 sdk.credentials：无签发接口）：仅 iframe 桥模式可用 */
  credentials?: PluginCredentialsAPI;
  /** 策略模块（community-affairs §7.2 sdk.policy：本地草稿读写）：仅 iframe 桥模式可用 */
  policy?: PluginPolicyAPI;
  /** 系统代理模块（sys.exec / sys.fetch）：仅 iframe 桥模式可用 */
  sys?: PluginSysAPI;
  /**
   * 注册宿主反向调用处理器：宿主经 host.request(event, payload) 主动查询插件时，
   * 由本处注册的 handler 应答（返回值即答复，可返回 Promise）。
   * 用于宿主需向插件求证的场景——如删除 bot 联系人前询问「该 bot 是否还存在」。
   * 仅 iframe 桥模式可用。
   */
  onHostCall?: (event: string, handler: (payload: unknown) => unknown | Promise<unknown>) => void;
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
  /** 视图类型，对齐 manifest.views[].type；background 为隐藏常驻后台视图（无 UI，随插件启用启动） */
  viewType: 'app' | 'message-card' | 'background';
  /** 卡片 id（仅 message-card 视图）：壳层分配，动作回调与归属校验的凭据 */
  cardId?: string;
  /** 卡片视图数据（仅 message-card 视图）：应用消息 card.data 透传 */
  cardData?: unknown;
};

/** 宿主 srcdoc 注入的视图引导信息（`window.__sparkPluginView`）：
 *  插件握手前唯一能拿到 viewId/卡片上下文的途径（hello 的 viewId 必须与桥绑定一致） */
export type PluginViewBootstrap = {
  viewId: string;
  viewType: 'app' | 'message-card' | 'background';
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
  /** background：隐藏常驻后台视图，无 UI，随插件启用即启动（用于常驻任务如消息监听） */
  type: 'app' | 'message-card' | 'background';
  title?: string;
};

/** 插件展示分类（市场筛选用，枚举便于扩展） */
export type PluginCategory = 'ai-assistant' | 'social' | 'tool' | 'game' | 'foundation';

/** 插件运行时前提（壳层在安装/启用时校验，不满足则拒绝或降级） */
export type PluginRequires = {
  /** 需要的系统能力子集（permissions 的超集校验；如 system:exec / network:fetch） */
  capabilities?: string[];
  /** 明确限定平台（缺省 = 全平台） */
  platforms?: Array<'desktop' | 'mobile'>;
  /** 移动端只读豁免：声明后移动端可安装但禁用写能力（默认 false） */
  mobileReadonly?: boolean;
};

/** 插件声明式清单（与插件目录 manifest.json 一一对应） */
export type PluginManifest = {
  id: string;
  /** 插件域，必须以 'plugin:' 开头 */
  domain: string;
  name: string;
  version: string;
  description: string;
  /** 展示分类（市场筛选用） */
  category: PluginCategory;
  /** 默认入口视图 id，必须存在于 views 中 */
  entryView: string;
  /** 插件可运行的空间类型 */
  supportedSpaces: Array<'personal' | 'org'>;
  views: PluginViewDeclaration[];
  /** 后台入口（可选）：包内 JS 文件相对路径（如 "background.js"），内容跑在
   *  内核 QuickJS 沙箱（无 DOM），随插件启用由内核拉起常驻线程；
   *  承载 bot 消息监听等无界面逻辑（plugin_system.md「后台运行时」） */
  background?: string;
  /** 权限声明（如 storage:read / storage:write / org:read / org:sync） */
  permissions: string[];
  /** 运行时前提（壳层安装/启用校验） */
  requires?: PluginRequires;
  /** 依赖的 SDK 契约版本 */
  sdkVersion: string;
  /** 宿主 chrome（壳层 UI 声明）。可选；缺省时壳层显示默认插件顶栏（返回+标题） */
  chrome?: {
    /** false：插件自接管顶栏，壳层隐藏默认顶栏主体、仅保留左上角悬浮返回图标 */
    hostTitleBar?: boolean;
  };
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

# p2p-app-messages

## 20. 应用消息（服务号模型，本地协议）

> 语义来源 `plugin_system`（wiki architecture/plugins/）「应用会话（服务号模型）」节。
> 应用消息**不走网络**：由本机插件实例从本机数据算出并写入本机会话，本地生成、
> 本地消费，零投递、零网络开销。本节是内核 message 模块应用消息扩展的权威规格。
> Rust 实现：`code/core/src/message/`（types / app 服务与限流器）、
> `code/core/src/kernel/message_ops.rs`（`message_app_*` 门面）。

### 20.1 应用会话

- 会话 id = `app:{pluginId}`（`APP_CONV_PREFIX`，确定性，由内核 `app_conversation_id`
  派生；与人际会话 `dm:{peerRootId}` 同一约定风格）。`pluginId` 为插件清单 id，
  字符集 `^[a-z0-9][a-z0-9-]{0,63}$`——不含 `:`，保证存储键分段无歧义；
  桥层持有的域形式 `plugin:{id}` 须先 strip `plugin:` 前缀再传入。
- 空间隔离：与人际会话同一 `space` 维度——`'personal'` 或 `'org:<orgId>'`
  （orgId = `org_<16hex>`），同一插件在不同空间各有独立会话与消息序列。
- 会话记录复用 §19 的 `msg:conv:{space}:{convId}` 键与 ConversationRecord 线形，
  `kind` 枚举新增 `'app'`；`peerRootId` 字段填插件 id（无对端语义，仅占位）；
  未读数与人际会话同一 `unreadCount` 字段，壳层角标正常聚合。
- 「不启用就没有会话」：会话由首次 `message_app_send` 惰性创建（标题缺省取
  pluginId，壳层/SDK 波次可据清单刷新）；删除应用会话连带清空该插件在本空间的
  全部应用消息。

### 20.2 应用消息结构

存储键 `msg:app:{spaceId}:{pluginId}:{createdAt:013}:{msgId}`（13 位零填充
时间戳段，scan 字典序 = 时间序，同 §19 `msg:item:` 口径），值为 JSON
（serde camelCase）：

```
{ id:        string,                    // 内核生成 m{now_ms}-{seq}（同 generate_message_id）
  pluginId:  string,                    // 冗余落盘，恒等于键内 pluginId
  summary:   string,                    // 纯文本摘要 = trim 后的 payload.summary（冗余提升）
  payload:   object,                    // 插件自描述 JSON，**必须含非空字符串 summary 字段**
  card?:     { viewId: string, data: any },  // 可省；message-card 富渲染视图
  createdAt: number,                    // epoch 毫秒（本地生成时刻）
  status:    "local",                   // 本地状态集，见 §20.3
  read:      boolean }                  // false 时序列化省略（同 §19 read 口径）
```

- **summary 不变量（写入校验，按序）**：`payload.summary` 缺失 / 非字符串 /
  trim 后为空 → **拒绝写入**（`missing-summary`）；trim 后超 200 字符
  （`APP_SUMMARY_MAX_CHARS`，建议值即强制值）→ 拒绝（`summary-too-long`）。
  记录内 `summary` = trim 后的 `payload.summary`，壳层未装插件时原生渲染该字段 +
  「安装插件查看完整内容」，可达性不依赖插件代码（组织公告全员可达的协议根基）。
- `card` 可省：`viewId` 为插件清单声明的 message-card 视图 id，`data` 为视图数据；
  内核只透传不校验 viewId 指向（渲染期由壳层解析；装了插件走 iframe 富渲染）。
- msgId 由内核生成，插件不得自报；同 id 语义 = 同一条消息（无重投路径，本地写入
  即唯一）。

### 20.3 状态与已读语义

- 状态集 = `{ local }`：本地消息无投递语义，落库即终态——不存在
  sending/delivered/failed，也没有 read 回执（§19.3 的人际状态机不适用于应用消息）。
- 未读/已读语义与人际会话一致：写入时所属会话 `unreadCount + 1`、消息 `read=false`；
  `appMarkRead` 清零会话未读并把会话内全部未读消息批量置 `read=true`；
  删除会话连带删除全部消息键。

### 20.4 不变量

1. summary 校验（§20.2）先于一切落库。
2. **归属**：应用会话 id 由 pluginId 确定性派生，写入 API 只接受 pluginId、不接受
   调用方自报会话 id——插件经桥调用时 pluginId 由桥按已认证插件域注入，因此插件
   只能写自己 pluginId 的会话；应用会话（`app:`）与人际会话（`dm:`/`sys:`）键
   空间互不相交。
3. **不参与 dm 同步**：应用消息不产生 dm 信封、不走 `/spark/dm/1.0.0`（§19）、
   不进入 append-only 存证链（evidence）、不出现在 `spark-sync` pubsub。组织空间的
   「全员可达」由 org 同步的业务数据 + 各成员本机插件实例各自本地生成保证——
   同步的是数据，不是消息。
4. **隐私红线**：插件不可读人际会话——桥协议层面不提供该接口（本波只落内核与
   命令域，`message:app` 权限与桥接随 SDK 波次落地）。

### 20.5 限流（壳层骚扰治理的内核兜底）

- 每 `(spaceId, pluginId)` 固定窗口 60 s 内最多写入 **10 条**
  （`APP_MSG_RATE_LIMIT = 10`，`APP_MSG_RATE_WINDOW_MS = 60_000`）；超限**拒绝写入**
  （错误 `rate-limited`：消息不落库、未读不变、限流窗口不前进），并按会话累计
  拒绝计数（熔断观测面，`rejectedCount` 单调递增、不落盘）。
- 校验顺序：summary/pluginId 校验先于限流判定——非法消息不消耗配额。
- 限流器为内核内存态：键 = `{space}:{pluginId}`，容量上限 1024 条（满时先回收
  过期窗口条目，仍满则整体清空，同 §19 dm 入站限流器口径），进程重启清零。

### 20.6 存储键与 data-mgmt 兼容

| 键 | 值 |
|---|---|
| `msg:conv:{spaceId}:app:{pluginId}` | ConversationRecord（`kind='app'`） |
| `msg:app:{spaceId}:{pluginId}:{createdAt:013}:{msgId}` | AppMessageRecord（§20.2） |

两类键均以 `msg:` 开头，data-mgmt `classifyKey` 归入 `messages` 类
（见 [data-mgmt](../data-mgmt.md) 数据分级表），用量统计、L1 过期清理与 L2 purge 口径
与人际消息一致，不新增分类、不需要 data-mgmt 变更。

### 20.7 命令域（src-tauri）

| 命令 | 通道 | 语义 |
|---|---|---|
| `message_app_send` | `message-app-send` | `(spaceKey, pluginId, payload, card?) → AppMessageView`；校验 + 限流 + 落库 + 未读 +1 |
| `message_app_list` | `message-app-list` | `(spaceKey, pluginId) → AppMessageView[]`（时间升序） |
| `message_app_mark_read` | `message-app-mark-read` | `(spaceKey, pluginId) → { success }` |
| `message_app_delete_conversation` | `message-app-delete-conversation` | `(spaceKey, pluginId) → { success }`（会话与消息一并删除） |

AppMessageView 线形 = §20.2 记录线形原样（camelCase）。SDK messages 域
（下一波）经桥注入已认证 pluginId 后调用同组命令；`message:app` 权限校验在桥层。

§17 常量速查增补：

| 参数 | 值 | 位置 |
|---|---|---|
| 应用会话 id 前缀 / pluginId 字符集 | `app:` / `^[a-z0-9][a-z0-9-]{0,63}$` | §20.1 |
| 应用消息键前缀 | `msg:app:` | §20.6 |
| summary 上限（APP_SUMMARY_MAX_CHARS） | 200 字符（trim 后） | §20.2 |
| 应用消息限流（APP_MSG_RATE_LIMIT / 窗口） | 10 条 / 60 s（固定窗口，超限拒绝并计数） | §20.5 |

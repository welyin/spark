# P2P 消息与协议规格（p2p）

> 来源：反向提取自 `desktop/src/main/p2p/`（p2p-node / pubsub-message-handler / node-announce /
> peer-exchange / org-recovery / org-share-sync / org-pull-sync / overlay-peer-store /
> peer-activity-store / stream-utils / peer-targets / constants）与 `desktop/src/main/db/collection.ts`。
> Rust 实现必须逐字节对齐签名/哈希输入构造；行号对应 TS 源码，便于复核。
> 组织层语义（邀请码、nodeInfoClaim、K 副本口径）见 `org.md`。

## 1. 网络栈与监听

### 1.1 libp2p 装配（p2p-node.ts:640-677）

- transport：`webSockets()`、`circuitRelayTransport()`（v2 中继传输）
- streamMuxer：`mplex({ disconnectThreshold: 100 })`（默认 5 被显式调高到 100）+ `yamux()`
  - 【互通验证记录 2026-07】Rust 内核仅有 yamux（rust-libp2p 已弃 mplex）。TS 侧按迁移桥接破例
    在 mplex 之后追加 yamux：拨号方按序提议，TS↔TS 仍优先 mplex，TS↔Rust 经 multistream-select
    落到 `/yamux/1.0.0`。已验证：lab interop 场景 A 连接 `multiplexer='/yamux/1.0.0'`，
    TS↔TS 既有 lab 场景（overlay/invite/recovery）无回归
- connectionEncrypter：`noise()`
- peerDiscovery：`mdns()`
- services：`identify()`、`autoNAT()`、`uPnPNAT()`、`dcutr()`、
  `circuitRelayServer({ reservations: { maxReservations: 15, defaultDurationLimit: 2*60*60*1000, defaultDataLimit: 256*1024*1024 } })`、
  `gossipsub({ emitSelf: false, allowPublishToZeroTopicPeers: true, floodPublish: true })`

### 1.2 监听地址与端口（p2p-node.ts:631-702、listen-port.ts、constants.ts:20-25）

- 监听 multiaddr：`/ip4/0.0.0.0/tcp/<port>/ws`；OS 支持 IPv6 时追加 `/ip6/::/tcp/<port>/ws`（双栈同端口）；
  双栈绑定失败时回退 IPv4 单栈重建节点
- 默认首选端口 `15002`（`P2P_DEFAULT_LISTEN_WS_PORT`）；持久化键 `p2p:listen:wsPort`（值是十进制字符串）
- 端口选择：从首选端口起向后扫描最多 50 个（`pickListenPort`，listen-port.ts:63-82），
  可用性以"能绑定 0.0.0.0（且 IPv6 模式下能绑定 ::）"判定；全部占用时退化为 0（OS 分配临时端口）
- 实际绑定端口从 `getMultiaddrs()` 里用正则 `/\/tcp\/(\d+)\/ws(?:\/|$)/` 解析并写回 `p2p:listen:wsPort`

### 1.3 libp2p 节点身份（identity-store.ts:13-28、constants.ts:14）

- Ed25519 keypair（`@libp2p/crypto` `generateKeyPair('Ed25519')`）
- 持久化：LevelDB 键 `p2p:identity:privateKey`，值 = `privateKeyToProtobuf(privateKey)` 的 **base64**；
  读取失败即重新生成并写回。PeerId 由该公钥派生，重启稳定
- node-announce 签名用的就是这把密钥（见 §5）

## 2. 主题（topic）命名

| 主题 | 用途 | 来源 |
|---|---|---|
| `spark-sync` | 业务数据与组织同步（update/delete/history-response/org-share/org-share-ack，以及插件经 IPC 的自定义广播） | p2p-node.ts:770、org-share-sync.ts:390 |
| `spark-overlay` | 覆盖网控制面，目前仅 node-announce | constants.ts:76、p2p-node.ts:775 |

两个主题在节点启动时均订阅（p2p-node.ts:771-776）。入站按 topic 分流：
`spark-overlay` → NodeAnnounceService；其余 → 统一 pubsub 消息处理器（p2p-node.ts:795-802）。

## 3. pubsub 信封（P2PMessageBody）与签名

### 3.1 信封字段（p2p/types.ts:7-24）

```
version:  string                      // 固定 "1"
type:     string                      // 'update' | 'delete' | 'history-response' | 'org-share' | 'org-share-ack' | 插件自定义
domain:   string                      // 业务域；org-share/org-share-ack 固定 'system'
collection?: string
id?:      string
payload:  any                         // delete 时为 null
meta?:    { vv: Record<string,number>, ts: number, nodeId?: string, tombstone?: boolean }
schema?:  { syncStrategy: 'append-only'|'lww', governance?: boolean, enableEvidence?: boolean }
evidenceHeadHash?: string | null      // sha256 hex 或 null（恒存在，见下）
timestamp: number                     // Date.now()，毫秒
pubKey?:  string                      // SPKI PEM（"-----BEGIN PUBLIC KEY-----\n..."）
signature?: string                    // base64
```

### 3.2 发送侧构造（p2p-node.ts:843-857 `broadcast`）

1. 组信封：`envelope = { version:'1', ...body, evidenceHeadHash: await getEvidenceHeadHash(db), timestamp: Date.now() }`
   - `evidenceHeadHash` 键**总是存在**（无存证头时为 `null`，序列化为 `"evidenceHeadHash":null`）
2. `envelope.pubKey = this.publicKeyPem`
3. `envelope.signature = signEnvelope(envelope)`
4. 发布字节 = `Buffer.from(JSON.stringify(envelope))`（紧凑 JSON，无空格）

`update` 发送方：db/collection.ts:227-235；`delete`：db/collection.ts:284-292（payload=null）。
meta 由 `generateUpdatedMeta` 生成（db/sync.ts:38-46）：`vv[nodeId] += 1`、`ts = Date.now()`，nodeId = 本机 PeerId 字符串。

### 3.3 签名算法与验签输入构造（p2p-node.ts:950-969）

- 算法：**Ed25519（PureEd25519，无预哈希）**。Node `crypto.sign(null, data, key)` / `crypto.verify(null, ...)`
- 签名密钥：**每次 P2PNode 构造时 `crypto.generateKeyPairSync('ed25519')` 临时生成，不持久化**（p2p-node.ts:105-108）。
  既不是 root 身份，也不是域身份，也不是 libp2p 节点密钥
- `pubKey` = 该临时公钥的 **SPKI PEM** 字符串
  - 【互通验证记录 2026-07】Rust 内核的上线形态是同一 SPKI DER 的 **base64**（无 PEM 头尾，
    DER = 12 字节 Ed25519 SPKI 前缀 `302a300506032b6570032100` + 32 字节原始公钥）。
    TS 验签侧按桥接破例扩展为 PEM / DER base64 双形态（p2p-node.ts `createEnvelopeVerifyKey`）；
    Rust 验签侧本就兼容 PEM/DER/raw32。已验证：lab interop 场景 B 双向签名 update 验签通过
- 签名输出：64 字节签名的 **base64**（标准字母表，含 padding）
- **签名输入字节** = `JSON.stringify({ ...envelope, signature: undefined })` 的 UTF-8：
  - 即整个信封去掉 `signature` 字段后的紧凑 JSON
  - **键序 = 对象插入序**：`version` → body 各键（按调用方书写顺序；`update` 为
    `type, domain, collection, id, payload, meta, schema`；`org-share`/`org-share-ack` 为 `type, domain, payload`）
    → `evidenceHeadHash` → `timestamp` → `pubKey`
  - 值为 `undefined` 的键被 JSON.stringify 丢弃；嵌套对象（payload/meta）按其自身插入序递归序列化
- **验签输入**（p2p-node.ts:958-969）：对**接收到的 JSON 文本** `JSON.parse` 后做同样变换
  （`{...parsed, signature: undefined}` 再 stringify）。`JSON.parse` 保留 wire 上的键序，
  因此验签字节 = 接收文本移除 `signature` 成员后的结果（键序不变）
- 验签公钥 = 消息内嵌的 `pubKey` 字段（`crypto.createPublicKey(pem)`）——**自证式，无身份绑定**：
  该签名只提供完整性/反垃圾门槛，不证明任何 rootId/域身份。Rust 侧不得把它当身份凭证
- 数字按 JS Number→JSON 规则序列化（毫秒时间戳为整数，无小数点）

### 3.4 入站处理与强制签名规则（pubsub-message-handler.ts:49-138）

处理顺序（全部在 JSON.parse 之后）：

1. 若消息**携带** `pubKey` 且 `signature`：必须验签通过，否则丢弃（所有类型一视同仁，:57-64）
2. `update` / `delete` / `history-response` 三类**强制要求签名**；未携带签名直接丢弃（:68-72）。
   其余类型（org-share、org-share-ack、插件自定义）不强制签名
3. `update`/`delete`：要求 `domain/collection/id/meta` 齐备（payload 缺省归一为 null），
   调 `applyRemoteUpdate` 落库；若带 `evidenceHeadHash` 且与本地存证头不一致，仅告警不丢弃（:74-90）
4. `history-response`：同样落库（meta 缺省归一 null）。**注意：当前代码库没有任何 history-response
   的发送方，也不存在 history-request 消息类型**——该分支仅为入站兼容保留（:92-101）
5. `org-share`：交 OrgShareSyncService.applyIncomingOrgShare（见 org.md §8）；接受且有 syncId 时
   经 `broadcast('spark-sync', { type:'org-share-ack', domain:'system', payload: ackPayload })` 回发 ack（:103-118）
6. `org-share-ack`：按 `payload.syncId` 唤醒发送方等待器（只匹配 syncId，不校验 receiverRootId，:120-132）

### 3.5 org-share / org-share-ack 的 pubsub payload

- org-share（org-share-sync.ts:423-433）：
  ```
  payload = {
    targetRootId: string,           // 64 hex
    syncId: string,                 // 12 字节随机 → 24 hex（org-share-sync.ts:391）
    organization: <组织记录或快照，两种线形并存，见 org.md §4.5>,
    pluginDocs: PluginDocSyncItem[], // 见 org.md §11
    nodeInfo: { peerId?: string, addresses: string[] }   // 目标节点地址回显
  }
  ```
- org-share-ack（org-share-sync.ts:243-251）：
  `payload = { syncId, orgId, targetRootId, receiverRootId }`

## 4. 直连协议通用约定（stream-utils.ts）

- 四个直连协议均为"写一帧 JSON → 读一帧 JSON"的 request-response：
  - 写：`writeStringToStream`（stream-utils.ts:101-132）——整段 UTF-8 JSON 作为**单帧**写入
    （sink 模式 yield 一次；send 模式 send 后按需 onDrain 并 close）
  - 读：`readStreamAsString`（stream-utils.ts:66-99）——**读第一个非空帧即返回**，不读到 EOF；
    帧文本剔除 `\u0000` 并 trim 后为空则继续等下一帧，直到超时
  - 应用层无长度前缀、无分隔符；帧边界由 mplex/ws 承载保证
- 解析失败一律返回 null 并告警（`parseJsonSafely`），不抛异常

## 5. node-announce（spark-overlay 主题，node-announce.ts）

### 5.1 消息格式（node-announce.ts:25-32）

```json
{
  "type": "spark-node-announce",
  "version": 1,
  "peerId": "<libp2p peerId 字符串>",
  "addresses": ["<multiaddr 字符串>", ...],
  "timestamp": 1720000000000,
  "signature": "<base64>"
}
```

### 5.2 签名（node-announce.ts:35-43、90-105）

- **待签名载荷**（`buildNodeAnnouncePayload`）= 固定键序的紧凑 JSON：
  `{"type":...,"version":...,"peerId":...,"addresses":...,"timestamp":...}`（不含 signature）
- 密钥：本机 **libp2p Ed25519 私钥**（§1.3 持久化那把）。优先 `nacl.sign.detached(utf8(payload), rawSecretKey)`，
  退回 `privateKey.sign(...)`——Ed25519 确定性签名，两者逐字节一致
- 输出：64 字节签名的 **base64**；整条消息为 `{...unsigned, signature}` 的紧凑 JSON 发布到 `spark-overlay`
- 验签（node-announce.ts:185-216）：从 `peerId` 字符串解析出内嵌的 Ed25519 原始公钥
  （`peerIdFromString(...).publicKey.raw`，须 32 字节），tweetnacl detached verify；
  签名 base64 解码后须恰为 64 字节

### 5.3 接收侧校验链（node-announce.ts:116-162）

按序全部通过才入池，任一失败静默丢弃：

1. JSON 可解析且结构匹配（type/version/字段类型）
2. 时间戳新鲜度：`|now - timestamp| ≤ 10 min`（`NODE_ANNOUNCE_MAX_AGE_MS`，constants.ts:100；未来 10 分钟内也算新鲜）
3. 地址数 1–20（`MAX_ANNOUNCE_ADDRESSES=20`），单地址长度 1–512（`MAX_ANNOUNCE_ADDRESS_LENGTH=512`）（:21-23）
4. 非本机 peerId
5. 限流：同一 peerId 距上次接受 ≥ 60s；若携带邻居池中**未知的新地址**则放宽到 ≥ 5s
   （constants.ts:88-95；判定依据 OverlayPeerStore 中已存地址，node-announce.ts:169-182）
6. 验签通过
7. `overlayPeers.remember(peerId, addresses, 'announce', verified=true)` 入池

### 5.4 发送节奏

- 周期：每 5 min（`NODE_ANNOUNCE_INTERVAL_MS`，constants.ts:81），由 keepalive tick 内 `announceIfDue` 触发（p2p-node.ts:341-353）
- 地址变化（`self:peer:update` 事件：UPnP 映射、relay 预约、前缀轮换）立即补发一次（p2p-node.ts:779-785）
- 发布内容 = 当前 `getMultiaddrs()` 全部地址（含 /p2p-circuit 预约地址），过滤空串与超长后截断到 20 条；无地址则不发

## 6. 直连协议 `/spark/version/1.0.0`

- 常量 `DIRECT_VERSION_PROTOCOL`（constants.ts:9）
- 响应侧（p2p-node.ts:748-756）：连接打开后**立即写入**一帧：
  `{"type":"peer-version","appVersion":<string>,"nodeId":<本机 peerId>,"timestamp":<ms>}`
- 请求侧（p2p-node.ts:188-219）：`dialProtocol` 后**不写任何内容**，直接读一帧（超时 2500ms）；
  取 `appVersion` 上报。每次 peer:connect 与 connectPeer 成功后触发，in-flight 去重
- 【互通验证记录 2026-07】Rust 初版以 request-response 行为承载本协议：请求方发**空请求帧**，
  响应侧须先读到请求 EOF 才回帧。该实现 Rust→TS 方向可用（TS 响应侧不读请求、开流即写），
  但 TS→Rust 方向必败：TS 请求方不写字也不半关闭，Rust 入站 `read_request` 等 EOF 直至
  2500ms 超时重置子流（TS 侧报 `StreamResetError`）。判定 TS 为规格基准，**修 Rust 侧**：
  version 协议改用专用 `VersionFrameCodec`（code/core/src/p2p/behaviour.rs），
  `read_request` 不读字节立即返回空串 → 响应侧子流打开即写版本帧，与 TS 语义一致；
  Rust 请求方仍写空帧（0 字节）以保持 request-response 框架形状。lab interop 场景 A3
  双向版本探测通过，Rust↔Rust loopback 测试无回归

## 7. 直连协议 `/spark/peer-exchange/1.0.0`（peer-exchange.ts）

- 常量 `DIRECT_PEER_EXCHANGE_PROTOCOL`（constants.ts:55）
- 请求（peer-exchange.ts:88）：`{"type":"peer-exchange-request","want":<int>}`
  - want 缺省/非法 → 16；上限 16（`PEER_EXCHANGE_MAX`，constants.ts:60）
- 响应：`{"ok":bool,"type":"peer-exchange-response","peers":[{"peerId":string,"addresses":string[],"lastSeenAt":ms}], "reason"?:string}`
  - 非 `peer-exchange-request` 或限流时 `ok:false`（限流附 `reason:"rate-limited"`）
- 响应侧规则（:30-64、139-160）：
  - 读请求超时 3000ms
  - 同一请求方服务间隔 ≥ 60s（`PEER_EXCHANGE_MIN_INTERVAL_MS`，constants.ts:70）
  - 抽样：排除请求方、排除 `lastSeenAt` 早于 14 天（`PEER_EXCHANGE_MAX_AGE_MS`，constants.ts:65）的条目；
    verified 优先、其余按 lastSeenAt 降序；取前 want 条
- 请求侧规则（:70-118）：仅向**已连接**邻居发起；读响应超时 4000ms；
  每条样本取 ≤16 条处理，跳过自 peerId 与应答方 peerId，地址过滤空串后截 20 条；
  一律 `remember(..., 'exchange', verified=false)` 入池（未验证线索）
- 发起节奏：keepalive 每 tick 轮选一个已连接邻居交换一次（游标轮转，p2p-node.ts:356-370）

## 8. 直连协议 `/spark/org-recovery/1.0.0`（org-recovery.ts）

### 8.1 恢复 token（org-recovery.ts:33-41、constants.ts:110）

```
timeBucket = floor(nowMs / 600000)                      // 10 分钟桶，十进制整数（JS number → string，无前导零）
token      = sha256hex(`${orgId}:${recoverySecret}:${timeBucket}`)
```

- 输入字节 = 上述**冒号拼接**字符串的 UTF-8（注意分隔符是 `:`，orgId 形如 `org_<16hex>`，recoverySecret 为 64 hex）
- 输出：sha256 的 **hex**（64 字符小写）
- 有效 token 集合 = 当前桶 + 上一桶两个 token（消除桶边界漏配）；发起查询时用当前桶 token

### 8.2 消息格式

- 请求（org-recovery.ts:143）：`{"type":"org-recovery-query","token":<64hex>,"ttl":<int>,"want":<int>}`
- 响应：`{"ok":bool,"type":"org-recovery-response","peers":[{"peerId"?:string,"addresses":string[]}], "reason"?:string}`

### 8.3 响应侧（org-recovery.ts:59-116）

1. 读请求超时 3000ms；type/token 校验（token 必须匹配 `^[0-9a-f]{64}$`），不符回 `ok:false`
2. 同一请求方服务间隔 ≥ 30s（`RECOVERY_QUERY_MIN_INTERVAL_MS`，constants.ts:135），命中回 `ok:false, reason:"rate-limited"`
3. want 归一：缺省/非法 → 8，上限 8（`RECOVERY_QUERY_WANT`，constants.ts:130）
4. 命中：遍历本机恢复视图（当前身份为成员的组织，见 org.md §10），
   token ∈ activeRecoveryTokens 即返回该组织 `memberNodeInfos` 前 want 条（仅含有地址的成员）
5. 未命中且 `min(max(0,ttl), RECOVERY_TTL=2) > 0`：向**除请求方外**的已连接邻居取前 2 个，
   以 `ttl-1` 转发查询，结果按 peerId 去重合并地址后截断到 want；ttl≤0 回空

### 8.4 请求侧（org-recovery.ts:119-159、p2p-node.ts:453-504）

- 触发条件（keepalive 内）：组织"全员不可达"连续 3 个 tick（`RECOVERY_TRIGGER_CONSECUTIVE_TICKS`，constants.ts:125），
  且距上轮查询 ≥ 10 min（`RECOVERY_COOLDOWN_MS`，constants.ts:120；**冷却为全局单值，非每组织**）
- 每轮：恢复视图前 3 个组织 × 已连接邻居前 3 个，ttl=2、want=8；读响应超时 3000ms
- 候选过滤：peerId 或地址须存在，地址滤空截 20；合并后最多取 16（`RECOVERY_QUERY_WANT*2`）；
  每轮最多拨号 4 个候选；命中只拨号，**不写组织成员表**（组织校验仍走 pull/claim 链路）

## 9. 直连协议 `/spark/org-share/1.0.0`（org-share-sync.ts / org-pull-sync.ts）

同一协议承载三类请求（按 `type` 分派，org-share-sync.ts:281-290）。

### 9.1 org-share（推送组织快照）

- 请求：`{"type":"org-share","payload":<§3.5 的 org-share payload>}`
- 成功响应：`{"ok":true,"syncId":...,"orgId":...,"receiverRootId":...}`
- 失败响应：`{"ok":false,"reason":<string>}`（含 'empty or invalid json' / 'invalid type' / 'not accepted' / 异常文本）
- 响应侧读超时 4000ms；接收语义（targetRootId 匹配、成员包含校验、合并落库）见 org.md §8
- 请求侧 `tryDirectOrgShare`（org-share-sync.ts:320-382）：逐个拨号目标地址（buildDialTargets），
  读响应超时 4000ms；`ok && syncId 匹配` 即视为送达（等价于收到 ack），随后写 org-sync-state（§11）

### 9.2 org-pull-list（列出对请求方可见的组织）

- 请求（org-pull-sync.ts:26-33）：
  ```json
  {"type":"org-pull-list","payload":{"requesterRootId":"<64hex>","requesterPeerId":"<可省>","nodeInfoClaim":<可省>}}
  ```
- 响应：`{"ok":bool,"type":"org-pull-list-response","organizations":[{"orgId":string,"sync":<versions>}],"reason"?:string}`
  - 缺 requesterRootId → `ok:false, reason:"missing-requester-root"`
  - 仅列出通过成员校验的组织（见 org.md §9.2），`sync` 为该组织 `record.sync.versions`
- **nodeInfoClaim 处理顺序**（org-pull-sync.ts:162-198）：先判定 requesterRootId 是否为本地任一组织成员，
  是才处理 claim（claim 可能 bump 版本），处理后**重新读取全部组织记录**再生成响应列表

### 9.3 org-pull-org（拉取单个组织）

- 请求：`{"type":"org-pull-org","payload":{"requesterRootId":...,"requesterPeerId"?:...,"orgId":...}}`
- 响应：`{"ok":bool,"type":"org-pull-org-response","orgId":...,"status"?:'member'|'removed',"organization"?:<快照>,"pluginDocs"?:[...],"reason"?:string}`
  - 组织不存在 → `ok:true, status:'removed', reason:'org-not-found'`
  - 成员校验失败 → `ok:true, status:'removed', reason:'not-member'|'peer-mismatch'`（**与真删除不可区分**，见 org.md §9.4）
  - 成功 → `status:'member'` + `normalizeIncomingSnapshot(record)` 重建的快照 + pluginDocs
- 请求侧读响应超时 4000ms，逐个地址尝试直到拿到可解析响应（org-pull-sync.ts:243-272）

## 10. 邻居记录

### 10.1 覆盖网邻居池 OverlayPeerRecord（overlay-peer-store.ts:20-29）

存储键 `p2p:overlay:peer:<peerId>`（constants.ts:35），值 JSON：

```
{ peerId, addresses: string[], firstSeenAt: ms, lastSeenAt: ms,
  source: 'connect'|'exchange'|'announce'|'org'|'mdns',
  verified: boolean,                 // announce 验签通过即 true；只升不降（sticky）
  lastDialResult?: 'success'|'failure' }
```

- 合并规则（:84-108）：按 peerId 合并地址（去重、trim、滤空，每 peer 截 20 条 `MAX_ADDRESSES_PER_PEER`）；
  每次 remember 刷新 lastSeenAt；firstSeenAt 保留首值
- 容量 200（`OVERLAY_POOL_MAX`，constants.ts:40）；超限淘汰：未验证者优先淘汰（同组内最久未见先走），
  全部已验证时才淘汰最久未见的验证条目；拨号失败不触发淘汰（:162-180）
- 拨号抽样（:145-156）：排除给定 peerId 集，verified 优先、其余按 lastSeenAt 降序
- 覆盖网拨号目标：活跃连接 < 4（`OVERLAY_DIAL_TARGET`）时补拨，每 tick 预算 2 次
  （`OVERLAY_TICK_DIAL_BUDGET`，constants.ts:45-50；p2p-node.ts:301-338）

### 10.2 节点活跃度 PeerActivityRecord（p2p/types.ts:60-73、peer-activity-store.ts）

存储键 `p2p:peer:record:<peerId>`（constants.ts:30），值 JSON：

```
{ peerId, addresses: string[],
  firstSeenAt, lastSeenAt, lastConnectedAt: ms|null, lastDisconnectedAt: ms|null,
  successCount, failureCount, consecutiveFailureCount?: number,
  cumulativeConnectedMs, currentSessionConnectedAt?: ms, lastError?: string }
```

- `rememberNodeInfo(result)`：'seen' 仅刷地址与 lastSeenAt；'success' 累计 successCount、
  置 lastConnectedAt、清零 consecutiveFailureCount；'failure' 累计 failureCount、
  consecutiveFailureCount+1（旧数据缺省时的基线：successCount==0 ? failureCount : 0）、记录 lastError
- 清除（:17、131-142）：`consecutiveFailureCount ≥ 10` 且"完全不活跃"
  （successCount==0 && cumulativeConnectedMs==0 && 无当前会话 && lastConnectedAt==null）时整条删除
- 连接结算：markConnected 记 currentSessionConnectedAt（幂等）；markDisconnected 把会话时长累入
  cumulativeConnectedMs 并置 lastDisconnectedAt
- **打分公式**（computePriority，:201-204）：
  ```
  priority = cumulativeConnectedMs + successCount*60000 - failureCount*30000 - max(0, now - lastSeenAt)
  ```
  无记录的候选按 `Number.MIN_SAFE_INTEGER` 处理（排最后）
- 到期清理：lastSeenAt 超 90 天删除（data-management/constants.ts:14、cleanup.ts:65-77）

## 11. org-sync-state 记账（org-share-sync.ts:31-34、63-86）

- 存储键 `p2p:org-sync-state:<peerId>:<orgId>`（constants.ts:146），值 JSON：
  `{ "versions": {summaryVersion,membersVersion,memberDetailsVersion,transactionsVersion}, "lastSyncedAt": ms }`
- 写入时机：
  1. org-share 直连送达确认后（org-share-sync.ts:439）
  2. org-share pubsub 收到 ack 后（org-share-sync.ts:464）
  3. org-pull 成功拉取某组织后（org-pull-sync.ts:279-296，经 onSyncState 回调）
- 到期清理：lastSyncedAt 超 90 天删除（data-management/constants.ts:17、cleanup.ts:80-92）
- ⚠️ **形状污染（已知不一致，详见 §13.2）**：路径 1/2 写入的 `versions` 实际是
  `{versions, sections, lastSyncedAt}` 外壳对象，路径 3 写入的才是规范 versions；两种形状共存于同一键前缀下

## 12. keepalive 与拨号候选

- 保活周期 60s（`ORG_KEEPALIVE_INTERVAL_MS`，bootstrap.ts:21），tick = `maintainOrganizationNetwork`（p2p-node.ts:379-445）：
  1. 覆盖网维护（§10.1 拨号目标 + §7 peer-exchange + §5.4 周期通告）
  2. 组织候选拨号：按活跃度打分排序，每 tick 最多新拨 3 个
  3. 反熵拉取：从最多 2 个已连接候选执行 org-pull（捎带自签 nodeInfoClaim）
  4. 管理员补副本（见 org.md §12）
  5. org-recovery 触发判定（§8.4）
- 登录引导 `bootstrapOrganizationNetworkOnLogin`（p2p-node.ts:252-293，由解锁 IPC 触发 ipc/identity.ts:22）：
  先覆盖网维护，再按打分遍历全部组织候选：连接 → org-pull（捎带 nodeInfoClaim）
- 拨号目标构造（peer-targets.ts:46-60）：原始地址 + （地址无 `/p2p/` 段且已知 peerId 时）自动补
  `<addr>/p2p/<peerId>` 候选；peerId 可从地址尾段 `/\/p2p\/([^/]+)$/` 反解（:23-37）

## 13. 已知不一致与实现坑（Rust 对齐必读）

1. **信封验签是"自证式"**：公钥取自消息自身，密钥每次进程启动临时生成。签名不绑定任何身份，
   只起完整性/反垃圾作用。不要把 pubKey 与 rootId/libp2p peerId 做任何关联假设
2. **org-sync-state 两种形状共存**（org-share-sync.ts:393、396、439、464 vs org-pull-sync.ts:288）：
   share 路径把 `{versions, sections, lastSyncedAt}` 外壳当 versions 写入；
   由此 share 路径的 stale 检查（:396，`isOrganizationSyncStale(previousState.versions, snapshot.sync)`）
   传入的 incoming 是外壳对象，`summaryVersion` 等字段全为 undefined，四个比较恒 false →
   **只要存在历史 sync-state，后续对该 peer 的 org-share 推送恒被 "skip stale sync" 跳过**；
   且 overview 的 coversCurrent 对污染记录恒为 true（见 org.md §12.3 的影响）
3. **org-share 推送与 org-pull 响应的 organization 线形不同**：推送发的是**原始 OrganizationRecord**
   （service 传入的记录带 `sync` 字段，org-share-sync.ts:393 原样上线），pull 响应发的是
   `normalizeIncomingSnapshot` **重建的快照**（org-pull-sync.ts:232）。接收方两种都能吃
   （normalizeIncomingSnapshot 按有无 `summary` 分派），Rust 必须同时接受两种形状
4. **history-response 无生产方**：当前代码只有入站处理；`history-request` 类型不存在
5. pubsub 上 **org-share/org-share-ack 不强制签名**（只有 update/delete/history-response 强制）；
   但任何类型只要带了 pubKey+signature 就必须验签通过，否则丢弃
6. 直连读帧是"**第一个非空帧即返回**"，不是读到 EOF；响应方写完即关闭写侧（send 模式）。
   实现时不要在响应侧等待请求方半关闭
7. 版本探测协议请求方**不写请求体**，响应方连接打开即写；读超时 2500ms 与其余协议（3000/4000ms）不同
8. org-recovery 冷却计时是**全局单值**（lastRecoveryQueryAt），多组织失联时一轮最多查 3 个组织
9. announce 时间戳校验用 `Math.abs`：未来 10 分钟内的公告同样接受（claim 同口径；邀请码只查过去 24h，
   **未来 createdAt 不设上限**，invite.ts:76-79）
10. gossipsub 配置 `floodPublish: true` + `allowPublishToZeroTopicPeers: true` + `emitSelf: false`：
    发布不依赖 mesh，零订阅主题也发；本机收不到自己的发布
11. 【互通验证记录 2026-07（阶段②收官）】TS↔Rust 真实互通实验
    （desktop `npm run p2p:lab -- interop`；Rust 例程 `code/core/examples/lab_node.rs`，
    stdio JSON 行驱动）四场景全过：A 互连（multistream-select 协商出 `/yamux/1.0.0`）+
    gossipsub topic 互见 + 版本探测双向；B 双向签名 update（Rust 验 TS 的 PEM、
    TS 验 Rust 的 SPKI DER base64）且两侧落库回调触发；C node-announce 双向验签 verified 入池；
    D peer-exchange 双向请求响应并入池。实验发现的真实线级不一致仅版本协议一处
    （见 §6 记录，修 Rust 侧）；muxer 与公钥线形差异按迁移桥接破例收口（见 §1.1、§3.3 记录），
    TS 其余协议面未动

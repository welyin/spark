# p2p-dht

## 13. 公共 DHT（Kademlia）承载约定

> 本节所述 DHT 三件套**已实现并验证**（见 `development_plan`（wiki product/））；原「先行规格（实施计划 Phase 1–3，尚无线上实现）」标注作废。
> 常量落在 `code/core/src/p2p/constants.rs`。

### 13.1 Kad 挂载与 DhtMode

- Kademlia 协议名 `/spark/kad/1.0.0`（`KAD_PROTOCOL_NAME`）
- `SparkBehaviour`（code/core/src/p2p/behaviour.rs）挂 `Toggle<kad::Behaviour<kad::store::MemoryStore>>`：
  内存存储 + 本地周期重发，不接 sled；记录都带 TTL（§13.2），到期淘汰
- `DhtMode` 三档（`BehaviourOptions.dht_mode`，默认 **Server**）：

  | 档位 | 行为 |
  |---|---|
  | `Off` | 不挂 kad behaviour（隐私开关「完全私有」） |
  | `Client` | 挂载但 `set_mode(Client)`：可发起查询，不响应入站查询、不提供记录存储（移动端预留） |
  | `Server` | 全量：参与路由、存记录、响应查询（默认） |
- bootstrap：`OverlayPeerStore` 现有条目 + 启动时已知节点 `kad.add_address`，随后 `kad.bootstrap()`；
  路由表可用节点数低于 5 时重试 bootstrap，目标维持 5–10（`DHT_MIN_PEERS`）
- Kad 与邻居池双写并存：Kad 路由表自动维护，`OverlayPeerStore` 照旧；DHT 发现的节点经三层确认
  （§13.3）后按 `source=Exchange` 同款**未验证口径**入池（`verified=false`），不改现有信任边界（§10.1）

### 13.2 节点存在记录（公共 DHT 节点发现）

- key = `H("spark:node:" + peerId)` = sha256(`"spark:node:" + peerId` 拼接字符串的 UTF-8)；
  文档与日志中记作小写 hex
- 记录内容 = §5.1 node-announce 签名报文的完整紧凑 JSON 字节（含 `signature`；
  复用 `sign_node_announce`，code/core/src/p2p/announce.rs）；字段、签名与验签口径同 §5.1–§5.3
- 记录 TTL 8h（`DHT_RECORD_TTL_MS = 8*60*60*1000`）；周期重发 `DHT_REPUBLISH_INTERVAL_MS = 4h`
  （取 TTL 之半），挂 60s keepalive tick 计数触发（§12），不另起定时器；地址变化即时补发同 §5.4

### 13.3 三层身份确认（DHT 查询命中后）

按序全部通过才入池，任一失败静默丢弃：

1. **记录签名与 PeerId 匹配**：按 §5.2/§5.3 校验记录内嵌的 node-announce 报文——从 `peerId`
   字符串提取内嵌 Ed25519 公钥（`public_key_from_peer_id_str`，announce.rs）验签通过
2. **identify 协议识别**：与该 peer 的 identify 交换中 protocols 列表含 `/spark/` 前缀协议即判定
   Spark 节点；identify 协议串本身不改（现为 `/ipfs/id/1.0.0`，对齐 JS，无需改动即满足语义）
3. **challenge-response**：向候选发起 `/spark/node-challenge/1.0.0`（§14），回执验签通过且
   回执 `peerId` 与连接对端一致

通过后 `overlayPeers.remember(peerId, addresses, 'exchange', verified=false)` 入池（§10.1）。

### 13.4 限流与存储配额节制

与 peer-exchange（§7）/ org-recovery（§8）同一节制哲学：

- 存储：MemoryStore 有界内存配额，记录 TTL 到期淘汰，不持久化、不接 sled
- 写入：仅发布本机节点存在记录与本机网关职责内记录（§15/§16）；周期重发统一挂 keepalive
  tick 计数，不另起定时器
- 查询：对同一目标的重复查询按既有口径限流（参照 `PEER_EXCHANGE_MIN_INTERVAL_MS` 60s /
  `RECOVERY_QUERY_MIN_INTERVAL_MS` 30s 的同一请求方最小间隔模式）
- 结果：DHT 线索一律未验证入池，与 exchange/recovery 候选同口径；邻居池容量、拨号预算
  与淘汰规则不变（§10.1）


## 14. 直连协议 `/spark/node-challenge/1.0.0`

轻量 challenge-response：请求方发 nonce，响应方用本机 libp2p 私钥签回执，
证明"对端确实持有该 peerId 对应的私钥"（§13.3 第 ③ 层）。

### 14.1 消息格式

- 请求：
  ```json
  {
    "type": "spark-node-challenge",
    "nonce": "<16 字节随机 → 32 hex 小写>",
    "timestamp": 1720000000000
  }
  ```
- 响应：
  ```json
  {
    "type": "spark-node-challenge-response",
    "nonce": "<回显请求 nonce>",
    "peerId": "<响应方 libp2p peerId 字符串>",
    "signature": "<base64>"
  }
  ```
- request-response 模式与帧形状同 §4（写一帧 JSON → 读一帧 JSON；单帧、无长度前缀、
  读第一个非空帧即返回；解析失败返回 null 不抛异常）
- 超时对齐既有 rr 协议：响应侧读请求 3000ms、请求侧读响应 4000ms（同 peer-exchange 口径，§7）
- 响应侧限流：同一请求方两次服务的最小间隔 ≥ 30s（对齐 `RECOVERY_QUERY_MIN_INTERVAL_MS` 口径）；
  命中限流直接关闭子流不回帧（请求方按超时处理）

### 14.2 签名

- **待签名载荷** = 固定键序紧凑 JSON（不含 type/peerId/signature）：
  `{"nonce":<请求 nonce 原样>,"timestamp":<请求 timestamp 原样>}`
- 密钥：响应方本机 **libp2p Ed25519 私钥**（§1.3 持久化那把，与 node-announce 同钥）
- 输入字节 = 载荷字符串 UTF-8；输出 = 64 字节签名的 **base64**
- ⚠️ 响应帧**不携带 timestamp**：请求方以本地保存的（nonce, timestamp）重建载荷验签

### 14.3 校验链

响应侧（决定是否为该请求签回执，按序）：

1. 结构：`type == "spark-node-challenge"`；`nonce` 匹配 `^[0-9a-f]{32}$`；`timestamp` 为整数毫秒
2. 新鲜度：`|now - timestamp| ≤ 10 min`（同 `NODE_ANNOUNCE_MAX_AGE_MS` 的 `Math.abs` 口径）
3. 限流（§14.1）

请求侧（验收回执，按序，任一失败视为挑战未通过）：

1. 结构：type/字段形状匹配；`signature` base64 解码后恰 64 字节
2. `nonce` 回显 == 本端发出的请求 nonce
3. `peerId` == 连接层对端 peerId（防代答）
4. 验签：从 `peerId` 提取内嵌 Ed25519 公钥（同 §5.2 口径），对 §14.2 重建载荷做 detached verify

> ⚠️ **回执新鲜度窗口（待议）**：Rust 实现在请求侧验收回执时另加一道
> `|now - timestamp| ≤ 60s`（`CHALLENGE_MAX_AGE_MS`）——远严于响应侧的 ±10min
> 与 node-announce 的 ±10min（§5.3）。该窗口是**有意更严**：回执是一次性临场
> 身份证明，nonce 唯一已防重放，无需宽窗口；代价是与本端时钟偏移 >60s 的节点
> 挑战会失败（校验链报 Stale）。是否放宽至与 announce 同口径待议。


## 15. 组织级私有 DHT（网关代理）

- **key 派生**：`H(orgSecret + ":members")` = sha256(`orgSecret + ":members"` 拼接字符串的 UTF-8)
  （code/core/src/org/recovery.rs 的 sha256hex 模式；orgSecret 见 [org-address](../../community/org/org-address.md) §13）
- **网关节点**（[org-address](../../community/org/org-address.md) §14 `gateways` 字段指定的成员节点）：检测到本机是某组织网关 →
  在该 key 上 `start_providing` + 周期重发（节奏同 §13.2，挂 keepalive tick 计数）
- **查询响应**：响应成员地址查询时只返回 `{peerId, addresses}` 条目，
  不含 orgId、组织名等任何组织语义
- **查询方**：组织失联触发 RecoveryTrigger（§8.4 口径）后，向已知网关发起该 key 的 provider
  查询；命中结果按未验证提示入邻居池（`verified=false`），组织校验仍走 pull/claim 链路
  （`org-sync`（历史档案，wiki protocol/org/） §9），信任边界不变
- **不可枚举性**：key 由 orgSecret 单向派生——非持密者无法计算 key、无法枚举该组织的
  provider 集合；provider 记录本身不携带组织标识


## 16. 自认证组织地址记录的 DHT 与 gossip 承载

记录线形、签名与五步校验链见 [org-address](../../community/org/org-address.md) §16；本节只定承载方式。

- **DHT key** = sha256(orgPublicKey 原始 32 字节)——即 orgAddress 内嵌的哈希本体
  （orgAddress 与该 key 一一对应，[org-address](../../community/org/org-address.md) §15）
- **发布**：**公开组织**（org 记录 `isPublic` 标志 + 组织设置开关）的网关节点 `put_record`，
  记录 TTL 8h、周期重发同 §13.2
- **gossip 扩散**：作为 §3 信封载荷随 `spark-overlay` 主题低频发布、随连接捎带：
  - 信封 `type='org-address'`（§3.1 枚举新增）、`domain='system'`、
    `payload` = 组织地址记录线形（[org-address](../../community/org/org-address.md) §16）
  - 信封**不强制签名**（§3.4「其余类型」口径）；记录的权威防伪是其自身的组织根密钥签名；
    信封层既有规则不变（携带 pubKey+signature 则必须验签通过，否则丢弃）
  - `spark-overlay` 入站分流见 §2
- **冲突裁决**：同一 orgAddress 的多条有效记录取 `seq` 最大者；`seq` 相同取 `publishedAt` 最新
- **本地缓存**：sled 键前缀 `p2p:org-address:`，尊重记录 `ttl`（过期即失效）；
  解析顺序 = 本地缓存 → gossip 副本 → DHT

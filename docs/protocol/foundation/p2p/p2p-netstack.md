# p2p-netstack

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
  - 【Rust 内核 dcutr 接入 2026-09（dcutr-hole-punch 设计；协议守护已会签——
    见 `dcutr-cosign`（联签记录，wiki protocol/p2p/））】
    阶段四D 起 Rust 侧 `SparkBehaviour` 挂载 libp2p `/libp2p/dcutr`（官方标准协议，
    零自定义线形）：电路连接建立后自动尝试打洞升级为直连，失败电路中继保底；
    与 TS 侧既有 `dcutr()` 同协议协商，TS↔Rust 直通；旧端（不挂 dcutr）协议
    协商缺失即不尝试，电路/直连现状不变——互通无破坏。

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
| `spark-overlay` | 覆盖网控制面：node-announce，以及组织地址记录（org-address 信封，§16） | constants.ts:76、p2p-node.ts:775 |
| `spark-affair-meta` | 议题元数据公告（type='affair-meta'，[affair-metadata](../../affairs/affair-metadata.md) §2/§6） | core/p2p/constants.rs（C4） |

`spark-sync`/`spark-overlay` 两主题在节点启动时均订阅（p2p-node.ts:771-776）；`spark-affair-meta` 同条件订阅（leaf 模式全砍，同既有三主题口径；affair-metadata §2「订阅与否由角色决定」的落点是宿主/内核层不订阅即可）。入站按 topic 分流：
`spark-overlay` 中线形为 §3 信封且 `type='org-address'` 的消息 → 组织地址记录校验链（§16），
其余 → NodeAnnounceService；`spark-affair-meta` 中线形为 §3 信封且 `type='affair-meta'` 的消息 →
元数据公告处理器（信封规则校验 + §3 线形 parse，不落业务库，归 affair-metadata §5 暂存区/C10）；
其余主题 → 统一 pubsub 消息处理器（p2p-node.ts:795-802）。


## 4. 直连协议通用约定（stream-utils.ts）

- 六个直连协议（§6 version、§7 peer-exchange、§8 org-recovery、§9 org-share、§14 node-challenge、§19 dm）均为"写一帧 JSON → 读一帧 JSON"的 request-response
  （Rust 侧另有 §21 org-mail 跨组织网关邮箱，同一帧约定、协议先行于实现）：
  - 写：`writeStringToStream`（stream-utils.ts:101-132）——整段 UTF-8 JSON 作为**单帧**写入
    （sink 模式 yield 一次；send 模式 send 后按需 onDrain 并 close）
  - 读：`readStreamAsString`（stream-utils.ts:66-99）——**读第一个非空帧即返回**，不读到 EOF；
    帧文本剔除 `\u0000` 并 trim 后为空则继续等下一帧，直到超时
  - 应用层无长度前缀、无分隔符；帧边界由 mplex/ws 承载保证
- 解析失败一律返回 null 并告警（`parseJsonSafely`），不抛异常


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


## 12. keepalive 与拨号候选

- 保活周期 60s（`ORG_KEEPALIVE_INTERVAL_MS`，bootstrap.ts:21），tick = `maintainOrganizationNetwork`（p2p-node.ts:379-445）：
  1. 覆盖网维护（§7 peer-exchange + §5.4 周期通告，均为已连接集上的只读/交换动作）。
     Rust 侧 tick **不发起任何拨号**（connection-policy M8）：孤岛自举改纯事件驱动——
     节点启动 / 网络变更确认时若 0 连接，从邻居池按排序拨一轮 ≤2 个候选（目的只有
     DHT 自举：0 连接时查询发不出去），失败即沉默到下个事件，**不做任何周期重试**。
     候选排序：verified 优先 → 最近拨号非失败优先（失败沉底）→ lastSeenAt 降序，
     且只取 `OVERLAY_DIAL_CANDIDATE_MAX_AGE_MS`（24h）内见过者（防 IPv6/DHCP 死快照）
  2. 组织候选拨号：按活跃度打分排序，每 tick 最多新拨 3 个
  3. 反熵拉取：从最多 2 个已连接候选执行 org-pull（P2 起不再捎带
     nodeInfoClaim——claim 应用面退役，见 `org-claim`（历史档案，wiki protocol/org/） 末注）
  4. org-recovery 触发判定（§8.4）
- 管理员补副本（见 `org-sync`（历史档案，wiki protocol/org/） §12）在 Rust 实现（连接策略 M6 收尾）中已移出 tick，
  改由组织写入事件点触发，tick 内零主动外联（见 `connection-policy-implementation`（wiki architecture/p2p/） §9.6）。
- 登录引导 `bootstrapOrganizationNetworkOnLogin`（p2p-node.ts:252-293，由解锁 IPC 触发 ipc/identity.ts:22）：
  先覆盖网维护，再按打分遍历全部组织候选：连接 → org-pull（P2 起不再捎带
  nodeInfoClaim；仅 legacy join 回退路径携带）
- 拨号目标构造（peer-targets.ts:46-60）：原始地址 + （地址无 `/p2p/` 段且已知 peerId 时）自动补
  `<addr>/p2p/<peerId>` 候选；peerId 可从地址尾段 `/\/p2p\/([^/]+)$/` 反解（:23-37）
- **网络变化重连（M9）**：两条路径——
  1. **A 秒级感知**：前端监听 `window` `online`/`offline` 事件 → 调 `p2p-network-changed`
     命令（command-map 已有映射；App.vue 挂载 `online`/`offline` 监听）。
  2. **B 兜底（必做）**：Rust 侧 `run_keepalive_tick` 内纯本地对比 `listen_addr_strings()`
     与上轮快照（`detect_local_network_change`），变化即武装 `pending_network_change` 防抖
     ——等价收到 `Command::NetworkChanged`，零网络开销、不依赖壳层。
  两者都汇聚到同一 debounce（`NETWORK_CHANGE_DEBOUNCE_MS`=5s），到期后若监听地址确已变化则
  重发布 announce + DHT 记录、重建 relay 预约、主动重拨优先类目 peer。

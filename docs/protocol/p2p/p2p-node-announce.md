# p2p-node-announce

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

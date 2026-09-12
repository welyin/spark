# p2p-envelope

## 3. pubsub 信封（P2PMessageBody）与签名

### 3.1 信封字段（p2p/types.ts:7-24）

```
version:  string                      // 固定 "1"
type:     string                      // 'update' | 'delete' | 'history-response' | 'org-share' | 'org-share-ack' | 'org-address'（§16） | 插件自定义
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

> org-share 系信封已随 C8/P4 退役，历史线形见 wiki protocol/org/org-sync.md（历史档案）。

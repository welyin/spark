# org-node-card
## 17. 节点名片（手动恢复连接）

### 17.1 线形与编码

```json
{
  "type": "spark-node-card",
  "version": 1,
  "peerId": "<libp2p peerId 字符串>",
  "addresses": ["<multiaddr>", "..."],
  "timestamp": 1720000000000,
  "recoveryToken": "<可省，64 hex>",
  "signature": "<base64>"
}
```

- **名片串** = `base64url(JSON.stringify(card) 的 UTF-8)`：编码口径复用邀请码（§2.2：
  `+`→`-`、`/`→`_`、去 `=` padding，紧凑 JSON 无空格；code/core/src/org/invite.rs 的
  `URL_SAFE_NO_PAD` 模式）
- `recoveryToken` 可省，面向"帮组织恢复"场景：复用 org-recovery 的 token 公式
  `sha256hex(orgId:recoverySecret:timeBucket)`（当前桶，§10 / [p2p-org-recovery](../../foundation/p2p/p2p-org-recovery.md) §8.1）

### 17.2 签名

- **待签名载荷** = 固定键序紧凑 JSON（不含 `signature`）：
  `{"type":...,"version":...,"peerId":...,"addresses":[...],"timestamp":...,"recoveryToken":<缺省为 null>}`
  （`?? null` 口径同 §16.2；线上对象缺 `recoveryToken` 时丢键）
- 密钥：本机 **libp2p Ed25519 私钥**（同 node-announce，[p2p-node-announce](../../foundation/p2p/p2p-node-announce.md) §5.2）；
  输入字节 = 载荷字符串 UTF-8；输出 = 64 字节签名的 **base64**

### 17.3 校验链与导入口径

按序：

1. **结构**：base64url 可解码、JSON 合法、`type == "spark-node-card" && version == 1`、字段形状匹配
2. **新鲜度**：`|now - timestamp| ≤ 10 min`（`Math.abs` 口径，同 §5.3）
3. **验签**：从 `peerId` 提取内嵌 Ed25519 公钥（[p2p-node-announce](../../foundation/p2p/p2p-node-announce.md) §5.2 口径）
   detached verify；`recoveryToken` 若存在只校验形状 `^[0-9a-f]{64}$`——
   不校验有效性（接收方未必持有对应 recoverySecret）
4. **导入**：地址过滤空串后截 20 条（`MAX_ADDRESSES_PER_PEER` 口径），**一律按未验证提示**
   `remember(..., verified=false)` 入邻居池并发起连接；后续组织校验照旧走 pull/claim
   链路（§9），信任边界不变

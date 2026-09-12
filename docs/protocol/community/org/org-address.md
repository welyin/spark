# org-address
> 组织密钥体系、自认证地址、地址记录与网关。
## 13. orgSecret（组织私有 DHT 派生密钥）

> 本节起各节（orgSecret / gateways 网关 / 组织根密钥对与 orgAddress）**已实现**（见 `development_plan`（wiki product/））；原「先行规格（实施计划 Phase 2–4，尚无线上实现）」标注作废。

- **生成**：组织创建时 `generate_org_secret()` = 32 随机字节 hex（64 hex 小写），
  仿 `generate_recovery_secret()`（code/core/src/org/types.rs）
- **存放**：组织记录 extra 动态键（键名 `orgSecret`），与 `recoverySecret` 同一 extra 模式（§3.1）；
  **不展示给用户**（UI 不渲染该 extra 键）
- **同步**：仅随组织同步链路（管理员推送/claim 落库后的快照广播、org-pull 拉取）在成员间流动——
  extra 非保留键经 `summary.metadata` 承载（§4.1）
- **用途**：派生组织级私有 DHT key `sha256hex(orgSecret + ":members")`
  （[p2p-dht](../../foundation/p2p/p2p-dht.md) §15），仅此
- **与 recoverySecret 的语义区别**：recoverySecret 派生的 token 进入半公开协议面（org-recovery
  查询，§10——任何持 token 者可向任意节点询问成员线索）；orgSecret 不进入任何面向非成员的
  协议面，仅在成员间同步链路内流动，用于计算私有 DHT key——非持密者无法计算 key、
  无法枚举组织成员（[p2p-dht](../../foundation/p2p/p2p-dht.md) §15）


## 14. gateways 字段（组织网关）

- `OrganizationRecord` 新增 `gateways: string[]`——rootId 列表（64 hex 小写，须为本组织成员），
  属**保留键**（不进 `summary.metadata`，§4.1）
- 管理员指定 **2–3 个**（`set_org_gateways`，code/core/src/org/service.rs），写入记录后走既有
  快照同步广播（§7/§9）扩散
- 职责：组织级私有 DHT 的 provider（[p2p-dht](../../foundation/p2p/p2p-dht.md) §15）、公开组织的地址记录
  发布（[p2p-dht](../../foundation/p2p/p2p-dht.md) §16）、未来的组织邮箱（本期不做——组织网关暂存转发
  离线消息已列入消息专项待办，见 code/app/TODO.md 消息章节）
- 网关角色是记录字段而非成员 role；role 仍仅 `admin` / `member`（§3.2）


## 15. 组织根密钥对与 orgAddress

- 组织创建时生成**组织根 Ed25519 密钥对**——与 root 身份、libp2p 节点密钥、pubsub 信封临时
  密钥均独立（第四条签名链，§19.1）；`orgId` 保持 `org_<16hex>` 不变，与 orgAddress 无派生关系
- **orgAddress = 公钥指纹（自认证地址）**：
  ```
  digest    = sha256(orgPublicKey 原始 32 字节)                        // 32 字节
  checksum  = sha256("spark:org-address:" 的 UTF-8 ‖ digest) 的前 2 字节
  orgAddress = base32(digest ‖ checksum)   // RFC 4648 字母表，小写，去 padding；34 字节 → 55 字符
  ```
  校验：base32 可解码为 34 字节 → checksum 段重算匹配 → digest 段即公钥指纹，
  与 `sha256(orgPublicKey)` 闭环比对（§16.3 第 4 步）
- `orgPublicKey` 在协议载荷中的编码同根公钥惯例：**base64（原始 32 字节，非 PEM）**（§1）
- **私钥存放**：由创建者内核本地保管——存组织记录 extra 的**加密字段**（键名与加密口径由实现期
  定案），不展示、**不进快照 metadata、不同步出本机**（§4.1 的例外）；本期单管理员持有，
  多管理员/多签共管是设计文档未决问题，不做


## 16. 组织地址记录（自认证）

### 16.1 线形

```json
{
  "orgAddress": "<§15，55 字符 base32>",
  "orgId": "org_<16hex>",
  "orgPublicKey": "<base64，原始 32 字节组织根公钥>",
  "displayName": "<可省，展示名>",
  "gateways": ["<64hex rootId>", "..."],
  "seq": 1,
  "publishedAt": 1720000000000,
  "ttl": 86400000,
  "signature": "<base64>"
}
```

- `seq`：同一根密钥下单调递增的发布序号；`publishedAt` 为毫秒
- `ttl`：毫秒，`0 < ttl ≤ 7 天`，发布方默认 24h；DHT 层记录 TTL 8h 独立，
  由网关周期重发续期（[p2p-dht](../../foundation/p2p/p2p-dht.md) §16）；本地缓存尊重 ttl，过期即失效

### 16.2 签名

- **待签名载荷** = 固定键序紧凑 JSON（不含 `signature`）：
  `{"orgAddress":...,"orgId":...,"orgPublicKey":...,"displayName":<缺省为 null>,"gateways":[...],"seq":...,"publishedAt":...,"ttl":...}`
  - `displayName` 缺省时载荷中序列化为 **`null`**（`?? null` 口径，同 §5.2 的 nodeInfoClaim）；
    线上记录对象缺 `displayName` 时**丢键**——验签统一经载荷构造函数归一
- 密钥：**组织根 Ed25519 私钥**（§15）；输入字节 = 载荷字符串 UTF-8；
  输出 = 64 字节签名的 **base64**

### 16.3 校验链（五步）

按序全部通过才接受，任一失败静默丢弃：

1. **结构**：字段类型/形状匹配；`gateways` 每项匹配 `^[0-9a-f]{64}$`
2. **ttl 窗口**：`ttl` 为正且不超 7 天上限；`now ≤ publishedAt + ttl`（未过期）且
   `publishedAt ≤ now + 10 min`（未来容忍与 claim/announce 同口径，§19.7）
3. **orgId 格式**：匹配 `^org_[0-9a-f]{16}$`
4. **自认证闭环**：`orgPublicKey` base64 解码恰 32 字节，且 `sha256(orgPublicKey)` ==
   orgAddress 内嵌 digest（§15 校验口径）——地址本身证明"记录主体持有对应私钥"
5. **Ed25519 验签**：§16.2 重建载荷 + `signature` + `orgPublicKey`，detached verify
   （公钥须 32 字节、签名须 64 字节）

### 16.4 发布与解析

- 承载（DHT key、gossip 信封、冲突裁决）见 [p2p-dht](../../foundation/p2p/p2p-dht.md) §16
- `resolve_org_address(orgAddress)`：本地缓存 → gossip 副本 → DHT；
  `search_known_orgs(keyword)`：本地缓存按 displayName/本地备注子串匹配，纯本地查询

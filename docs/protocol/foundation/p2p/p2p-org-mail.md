# p2p-org-mail

## 21. 直连协议 `/spark/org-mail/1.0.0`（跨组织网关邮箱）

> 状态：**已实现**（阶段四E 落地；设计依据
> `org-gateway-mailbox`（wiki architecture/p2p/），下称「设计」）。
> 场景与边界（可触达 ≠ 可进入、网关不可信、双层身份模型、私有组织不参与）
> 见设计 §1，本节只定义字节级协议面。
>
> 实施层注记（实测确认，不改变线上字节语义）：
> - §21.5 deliver 的处理顺序中**限流先于幂等**：同窗同 id 重投应答
>   `rate-limited`；幂等 `ok:true` 只在限流窗口外的重投出现。
> - 网关端点解析：DHT 节点存在记录按 libp2p peerId 定键，而地址记录
>   gateways 字段是 rootId——rootId→peerId 无公共索引，DHT 解析路径本期
>   未接，发送方端点解析走显式 hint + 朋友记录 peers。
> - golden vectors（`code/spec/vectors/org-mail.json`）由 Rust 例
>   `core/examples/gen_org_mail_vectors.rs` 自产（含 verify/unbox 自检），
>   无 TS 对侧生成器。

跨组织离线存信：发送方直连**收件方组织网关**投递密文信封，收件人向本组织
活跃网关挑战拉取。网关只见路由元数据，内容不可读。帧约定同
[p2p-netstack](p2p-netstack.md) §4（写一帧 JSON、读一帧 JSON、无长度前缀；
Rust 侧 `JsonFrameCodec` 模式，behaviour.rs 先例）。rootId 不出本协议的
任何线上字段（双层身份红线）——信封与授权一律用**域身份**。

### 21.1 域身份与密钥

- 新域串 **`org-mail:{orgId}`**（orgId = `org_<16hex>` 小写）：域身份派生
  同 [identity](../identity.md) §4（`sha256(utf8(domain))` → idxA/idxB → 强化路径
  两级 → Ed25519 fromSeed）；与 `org-access:{orgId}` 域分隔，互不可推导；
- 信封中的 `domainId` = 域身份 **Ed25519 公钥 32B 的 base64（标准表含
  padding）**——不是 rootId，不出 rootId 的任何派生指纹；
- 收件人域身份公钥的获取 = 带外通道（与组织地址分发同通道，设计 §1）；
  本协议不提供陌生人发现入口。

### 21.2 邮箱信封线形（字节级）

```json
{
  "id": "<24hex 随机>",
  "to":   { "orgAddress": "<组织地址记录>", "domainId": "<b64 32B>" },
  "from": { "domainId": "<b64 32B>", "orgAddress": "<可省>" },
  "ts": 1720000000000,
  "ttl": 604800000,
  "nonce": "<b64 12B 随机>",
  "ct": "<b64>",
  "sig": "<b64 64B>"
}
```

| 字段 | 类型 | 语义与约束 |
| --- | --- | --- |
| `id` | string | 24 位小写 hex（12B 随机）；投递幂等键（同 id 去重） |
| `to.orgAddress` | string | 收件方组织地址（[org-address](../../community/org/org-address.md) 自认证地址记录线形）；网关据此判定「是否本组织的信」 |
| `to.domainId` | string | 收件人域身份公钥 b64（路由键；拉取侧域名匹配依据） |
| `from.domainId` | string | 发送方域身份公钥 b64（验签键 + 投递限流键） |
| `from.orgAddress` | string，可省 | 发送方组织地址（展示/回信寻址提示，不参与授权） |
| `ts` | number | 发送方本地 Unix 毫秒；入站新鲜窗 `\|ts − now\| ≤ 10 min`（同 dm 信封口径） |
| `ttl` | number | 毫秒；缺省/缺省值 = 604800000（7 天，对齐 dm_offline `PENDING_TTL_MS`）；上限 2592000000（30 天），超限按上限截断（非拒收） |
| `nonce` | string | 12B 随机，base64 标准表 |
| `ct` | string | AES-256-GCM 输出（密文 ‖ 16B tag），base64 标准表（§21.3） |
| `sig` | string | 发送方域身份 Ed25519 签名 64B，base64（§21.4） |

**网关可见面**（既定口径）：`to.orgAddress`、`to.domainId`、`from.domainId`、
`id`、`ts`、`ttl`、信封字节大小——「谁能收到信」对网关可见，明文内容不可读。

### 21.3 加密原语（box/unbox）

复用 orgkey-deliver 的原语族（§20.6 / `sync/orgsync/access.rs`
`box_epoch_key`/`unbox_epoch_key` 同构，上下文串不同）：

1. **DH**：两侧域身份 Ed25519 → X25519 标准转换，`mul_clamped` 静态 DH；
   共享全零（低阶点特征）**拒绝**（H1a 同口径）；
2. **域分隔派生**：`boxKey = sha256(shared ‖ domain_info)`，
   `domain_info = "orgmail-box\x00{toOrgAddress}\x00{fromDomainId}\x00{toDomainId}"`
   （domainId 取 b64 字符串的 UTF-8 字节；防共享跨信封/跨对端复用，H1b
   同口径）；boxKey 全零拒绝（双保险）；无 PFS 属明示取舍（同 §20.6）；
3. **加密**：`AES-256-GCM(boxKey, nonce12, plaintext, aad)`；明文 = 应用层
   载荷 JSON 的 UTF-8（内容类型由载荷内 `kind` 区分，**邮箱层不解释**）；
   **AAD = `"{to.orgAddress}:{to.domainId}:{id}"`** 的 UTF-8 字节（防密文
   跨信封/跨收件人搬迁，同 §20.2.2 口径）。

### 21.4 发送方签名

`sig` = 发送方 `org-mail:{fromOrgId}` 域身份 Ed25519 私钥对**固定键序紧凑
JSON 载荷**（无空格、非 ASCII 不转义，同 canonical 口径）的签名：

```json
{"ct":...,"from":{...},"id":...,"nonce":...,"to":{...},"ts":...,"ttl":...}
```

- 顶层键序固定如上（字典序）；嵌套对象键序固定：`from` =
  `{"domainId":...,"orgAddress":...}`、`to` = `{"domainId":...,"orgAddress":...}`；
- **`from.orgAddress` 缺省时整个键不出现**在签名载荷中（与信封线上形态
  一致——线上省略则载荷省略，杜绝「省/null」两义）；
- 验签键 = `from.domainId`（自包含，同 nodeInfoClaim 口径）；验签失败 =
  形状非法（§21.5 错误码 `invalid-envelope`）。

### 21.5 两个 op

#### 投递（发送方 → 目标网关）

```json
// 请求
{ "op": "deliver", "envelope": { §21.2 信封 } }
// 响应
{ "ok": true }
{ "ok": false, "reason": "<见下>" }
```

- 网关处理顺序：**形状 → ts 新鲜窗 → 签名 → `to.orgAddress` 归属本组织
  （不符 `wrong-org`）→ 限流（按 `from.domainId`，同一来源最小间隔 1s，
  沿用覆盖网节制原则，命中 `rate-limited`）→ 幂等去重（同 `id` 已有 →
  `ok:true` 不重复落库）→ 配额（§21.6，超限 `quota`）→ 落库**；
- `reason` 枚举：`invalid-envelope`（形状/签名/freshness）、`wrong-org`、
  `rate-limited`、`quota`；内部异常文本原样（对齐 §9.1 既有口径）；
- **网关不校验**收发双方的组织成员资格（网关无法也无须验证对方组织
  成员表——内容真伪由收件人解密 + 验签把关，与 encrypted 集合 AEAD
  把关同构，设计 §2.4）；
- 存储键：`orgmail:box:{orgId}:{envelopeId}`（值 = 信封 JSON 原样）；
- 无投递回执（最小形态）：`ok:true` = 「已入对方组织邮箱」，**不等于**
  对方已读/已取；丢信语义如实呈现（设计 §5）。

#### 拉取（收件人 → 本组织活跃网关，两轮握手）

取信授权 = **域身份持有证明**（挑战签名）+ **域名匹配**——域身份从 root
种子派生，能签出挑战即身份本体，网关无需也无法另查对方组织成员表
（设计 §2.4 最小形态）。挑战 nonce 由网关签发（防重放/预计算），单轮
请求-响应无法完成，故拉取为**同一网关上的两轮请求**：

```json
// 第一轮：取挑战
// 请求
{ "op": "fetch", "recipientDomainId": "<b64 32B>" }
// 响应（挑战）
{ "ok": true, "phase": "challenge", "nonce": "<b64 16B 随机>", "ts": 1720000000000 }

// 第二轮：带挑战应答取信（可在新流/新连接上进行——nonce 按 (网关, nonce)
// 记一次性，TTL 60s，用后即焚）
// 请求
{ "op": "fetch", "recipientDomainId": "<b64 32B>", "nonce": "<回显>",
  "challengeTs": 1720000000000, "challenge": "<b64 64B sig>" }
// 响应
{ "ok": true, "envelopes": [ { §21.2 信封 }, ... ] }
```

- **挑战签名**：`challenge = sign(域身份私钥, utf8(payload))`，
  `payload = "orgmail-fetch\x00{nonce}\x00{gatewayPeerId}\x00{challengeTs}"`
  （nonce 为 b64 字符串原样；gatewayPeerId = 网关连接层 peerId base58；
  challengeTs 为十进制 ASCII）——验签键 = `recipientDomainId` 解码出的
  域身份公钥；
- 网关校验顺序：nonce 在册且未过期未用 → challenge 验签 →
  **域名匹配**：只返回 `to.domainId == recipientDomainId` 的信封；
- **取信即删**：成功响应的信封随响应同事务删除；拉取失败/部分投递
  由下一轮拉取补齐（拉取触发时机见设计 §2.6：orgsync hello 收敛后 +
  keepalive tick + start_p2p 上线拉一次，逐活跃网关合并）；
- 失败响应：`{ok:false, reason}` —— `invalid-request`（形状/缺字段）、
  `invalid-challenge`（nonce 不在册/过期/已用、验签失败、gatewayPeerId
  或 ts 不吻合）、`rate-limited`（按 recipientDomainId 最小间隔 1s）；
- 收件人侧收信落 `orgmail:in:` 键族（本地键，不进任何同步流量）；呈现层
  （系统会话/收件箱）归 UI 批次，不在本协议面。

### 21.6 配额、TTL 与幂等（组织自治理）

| 参数 | 值 |
| --- | --- |
| 信封 TTL 缺省 | 604800000 ms（7 天） |
| 信封 TTL 上限 | 2592000000 ms（30 天），超限截断 |
| 每组织邮箱容量 | ≤ 1000 条 **或** ≤ 10 MB（先到为准；超限投递 `quota`） |
| 每收件人条数 | ≤ 100（按 `to.domainId` 计；超限 `quota`） |
| 投递幂等 | 同 `id` 去重（重试/多网关投递无害） |
| 过期清扫 | 惰性——随网关读写时点执行，不设独立定时器（orgkey stash 老化先例） |
| 投递/拉取限流 | 按来源/收件人 domainId 最小间隔 1 s |
| 挑战 nonce | 16B 随机 b64；TTL 60 s；一次性 |

### 21.7 与既有通道的边界

- **不进邮箱**：组织内（同组织成员间）消息一律走既有 dm/dm_offline；
  dm_offline 是同信任域暂存（验签绑 rootId），邮箱是跨信任域经第三方
  暂存（域身份签名）——存储形态同构（pending 键 + TTL + cap），键族
  各自独立（`orgmail:box:` / `orgmail:in:`），正交不合并（设计 §2.7）；
- **私有组织零变化**：不发布地址记录即无邮箱入口；
- 与 orgsync/orgsync-data 无交集（邮箱信封不进任何同步集合的键域）。

### 21.8 golden vectors（实施时落 `code/spec/vectors/org-mail.json`）

向量组（协议先行，实施以本清单验收；消费测试 `core/tests/org_mail_vectors.rs`）：

1. **信封构造**：固定两侧域身份种子（固定 orgId 对）、固定 `id`/`ts`/
   `ttl`/`nonce`/明文载荷 → 逐字节 `ct` 与 `sig`；
2. **box 往返**：固定 DH 输入 → 固定 boxKey/密文；低阶点（全零共享）
   拒绝用例；
3. **AAD 搬迁拒收**：把 `ct` 搬到不同 `id`/`to.domainId` 的信封 →
   解密必败；
4. **签名载荷**：固定信封（含/不含 `from.orgAddress` 两形态）→ 固定键序
   载荷串逐字节 + sig 固定值；篡改任一字段验签必败；
5. **挑战**：固定 nonce/gatewayPeerId/challengeTs/域身份种子 →
   `orgmail-fetch` 载荷串与 challenge sig 固定值；错 peerId/错 ts/
   重放 nonce 验签必败；
6. **配额/TTL 判定**：边界值（ttl=上限、容量 999/1000、每收件人
   99/100）的接受/拒绝判定表。

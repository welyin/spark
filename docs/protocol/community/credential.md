# credential（资格凭证 / 注销列表 / 验证人信任声明）

> 状态：**C2 纯逻辑已落地**（`core/src/credential/`：§2–§6 线形与验证链、holderProof；签发流程归插件不在内核，OrgSigSet 五步验证链归 C3 经注入接口接入）。依据：`总体方案`（wiki architecture/community-affairs.md） §7.1（内核管格式与验证、签发归插件）；产品语义：`共同体模型`（wiki product/community-model.md） 第十节。
> 通用编码约定见 `community 总约`（wiki protocol/community/README.md）。
>
> ⚠️ 决策变更（2026-09-10）：组织与共同体已合并为统一概念（成员分个人/组织两类，加入类型由创世策略声明；副本按全副本设备计数）。本篇涉及"叶组织/共同体域"二分的规则以 [docs/product/community/model.md](../../product/community/model.md) 修订为准，全量改写见架构任务 A53。

## 1. 边界

内核：凭证 schema、签名验证、注销列表（注销 = 验证人签名的注销记录，append-only）。**签发流程不在内核**——验证插件是方法，验证人才是信任；证据最小披露（原始材料只给验证人看，不进公共数据、不上链）。

## 2. 资格凭证线形

```json
{
  "credV": 1,
  "credType": "household-owner",
  "issuer": { "identity": "<64hex>", "publicKey": "<b64 32B>" },
  "holder": { "kind": "org", "identity": "<64hex>", "publicKey": "<b64 32B>" },
  "subjectDomain": "org_<…>",
  "claims": { "household": "3-502" },
  "method": "plugin:hoa-verify:manual-property-cert",
  "linkRef": null,
  "issuedAt": 1720000000000,
  "sig": "<b64 64B>"
}
```

| 字段 | 约束 |
| --- | --- |
| `credV` | 恒 1 |
| `credType` | 凭证类型（`^[A-Za-z0-9_-]+(:[A-Za-z0-9_-]+)*$`，≤64）——如 `household-owner`（业主）/`resident`（居住）；类型决定角色与权利边界，**资格跟着凭证走，不跟层级走**（产品第六节） |
| `issuer` | 验证人（个人域身份）；`identity == sha256hex(publicKey)` 前置校验 |
| `holder` | 持有者；`kind: 'person' \| 'org'`——持有者可以是个人或组织，取决于目标域创世策略声明的成员类型门槛（2026-09-10 决策；原"加入上层域的实体必须是组织、典型 holder.kind = org"的旧口径待 A53 改写）。kind = org 时 identity 为组织在本域的域身份（`org-genesis`（待 A53 改写，wiki protocol/community/） §4） |
| `subjectDomain` | 对象域（凭证声明资格所对的域 orgId；双形态） |
| `claims` | **结论字段**：如 `household`（户号）；**禁止**姓名、证件号等身份标识（最小披露红线；字段级可见性归策略引擎，不进凭证） |
| `method` | 核验方式标识（验证插件 id + 方法名）；验证插件强制 L1 开源（产品规则，协议层只记录标识） |
| `linkRef` | 同人关联声明引用（§5），缺省 `null` |
| `issuedAt` | 签发者声明时刻；±10 min 新鲜度门槛同总约 |
| `sig` | issuer 私钥对 `canonical(剔除 sig 的全部字段)` 的签名 |

```
credId = sha256hex(normalizeObject(凭证剔除 sig))   // 64 hex；注销/呈现/关联声明均以此引用
```

- **凭证不设有效期**（产品）：资格变更由验证人主动发起注销（§3）；资格失效不抹除历史——过去的发言投票原样保留，仅失去新操作资格；
- 存储：凭证由持有者本地保管（键 `cred:held:{credId}`，**本地键**），呈现时才进协议面（[read-gate](read-gate.md)）；凭证不随公共同步面扩散（最小披露）。

## 3. 注销列表（验证人 append-only 签名日志）

### 3.1 注销条目

```json
{
  "revV": 1,
  "issuer": "<64hex>",
  "seq": 7,
  "prevHash": "<64hex>",
  "credId": "<64hex>",
  "revokedAt": 1720000000000,
  "reason": null,
  "sig": "<b64 64B>"
}
```

- 链规则：`seq` 从 1 递增，`prevHash` 指向前一条目 `entryHash`，首条 `null`；`entryHash = sha256hex(normalizeObject(条目剔除 sig))`——每验证人一条独立链（语义同 [sync-evidence](../foundation/sync-evidence.md) §2 的迷你版，作用域 = issuer）；
- `reason` 可空（展示用，无协议语义）；
- 注销列表**公开可复制**（验证人经 gossip/indexer 渠道公布；承载面随 C10 定，线形先行）。

### 3.2 注销列表头承诺（「未注销」证明）

```json
{
  "revHeadV": 1,
  "issuer": "<64hex>",
  "headSeq": 7,
  "headHash": "<64hex>",
  "asOf": 1720000000000,
  "sig": "<b64 64B>"
}
```

- 验证「credId 截至某时刻未注销」= 验证人签发的头承诺 + 列表 `seq = 1..=headSeq` 全量（链式校验逐条重算）中不含该 credId；
- 头承诺是**验证人的陈述**（验证人是信任本体，产品第十节）；持旧头声称未注销属正常滞后，消费方按 `asOf` 新鲜度自行取舍；
- **既往不咎**：组织撤销对某验证人的信任，不影响其已签发凭证的效力（§4 trustDecl 带 `effectiveFrom`，凭证按签发时刻信任集判定）。

## 4. 验证人信任声明（trustDecl）

组织（对象域）声明信任哪些验证人、对哪些凭证类型/方法：

```json
{
  "trustV": 1,
  "orgId": "org_<…>",
  "verifiers": [
    { "identity": "<64hex>", "publicKey": "<b64 32B>",
      "credTypes": ["household-owner", "resident"],
      "methods": ["plugin:hoa-verify:*"] }
  ],
  "effectiveFrom": 1720000000000,
  "seq": 3,
  "updatedAt": 1720000000000,
  "sigSet": { "…": "OrgSigSet，组织签名包 `org-signature`（待 C3，wiki protocol/community/）" }
}
```

- 存放：`org:verifiers:{orgId}` 单记录，逐版 LWW（`seq` 大者胜——原对标 `org:acl` updatedAt 口径，acl 已随 C7 退役），并入 org:structure@v1 键域全员流动（**既有线形变更注记**落 `org-orgsync`（待 A53 改写，wiki protocol/org/） §20.2.3；与 `org:evi:anchor:` 同列——全员可见的审计面；`org:acl:` 原亦同列，已随 C7 退役移出该键域）；
- 合入校验：`sigSet` 按 `org-signature`（待 C3，wiki protocol/community/） §5 五步验证链通过才接受（验签失败保留本地现状——原同 acl 口径，acl 已随 C7 退役）；
- 撤销信任 = 新版记录移除该 verifier，`effectiveFrom` 之后的签发不再被本域信任；之前签发的凭证效力不受影响；
- **验证人失信处置**的产品语义（既往不咎、逐条注销变更）完全由 effectiveFrom + §3 注销流程承载，协议不加额外机制。

## 5. 同人关联声明（same-person linkage，opt-in）

一人多户（一人控制多个组织持多户凭证）：默认「一户一票」两户两票；共同体规则要求「一人一票」时，验证人在凭证体系上附加关联声明，计票时合并。**关联声明打破两户间不可关联性，属 opt-in 披露，仅在规则明确要求时启用。**

```json
{
  "linkV": 1,
  "statement": "same-person",
  "members": [ { "holderIdentity": "<64hex>", "credId": "<64hex>" } ],
  "issuer": { "identity": "<64hex>", "publicKey": "<b64 32B>" },
  "subjectDomain": "org_<…>",
  "issuedAt": 1720000000000,
  "sig": "<b64 64B>"
}
```

- `members` ≥ 2；每个 credId 必须是该 issuer 已签发、未注销的凭证（验证链回查 §3）；
- 凭证侧经 `linkRef = <本声明哈希 linkId = sha256hex(canonical 剔除 sig)>` 回指；声明与凭证双向可索引；
- 声明与凭证同口径**不设有效期**：验证路径（结构 + 签名）不查 `issuedAt`，±10 min 新鲜度门槛只在签发受理时校验；声明的失效路径 = 成员凭证注销（§3）；
- 计票合并语义在事务规则/插件侧；协议只保证声明可验证、留痕、opt-in。

## 6. 验证链（凭证呈现时，内核纯逻辑）

1. **结构**：字段类型/形状；issuer/holder 的 identity == sha256hex(publicKey)；
2. **credId 复算**：`sha256hex(canonical 剔除 sig)` 匹配引用值；
3. **验签**：issuer 私钥签名有效；
4. **信任匹配**：issuer ∈ subjectDomain 的 trustDecl，且 credType/method 在其声明范围内；信任集按 **issuedAt 时刻**判定（既往不咎）；
5. **注销检查**：credId 不在 issuer 注销列表（头承诺 + 全量链复算，§3.2）；
6. **持有者绑定**：呈现场景由 [read-gate](read-gate.md) §3 的 holderProof 闭合（证明持有 holder 私钥）。

任一失败 → 凭证视为无效（fail-closed），消费方不得放行。

## 7. 验收向量（登记：`code/spec/vectors/community.json`）

| case 组 | 内容 | 状态 |
| --- | --- | --- |
| `credential.issue` | 固定凭证输入 → canonical 载荷逐字节 + credId + sig 固定值；篡改字段必败 | 已产出（生成器自产） |
| `credential.revokeChain` | 3 条注销条目 → entryHash/prevHash 链逐字节；断链必败 | 已产出 |
| `credential.revHead` | 固定头承诺 → 签名载荷逐字节 + sig 固定值 | 已产出 |
| `trustDecl` | 固定信任声明 → canonical + sigSet 验证通过；无效 sigSet 拒绝合入 | 已产出（sigSet 复用 orgSigSet 组密钥） |
| `samePersonLink` | 固定关联声明 → linkId + sig 固定值；members<2 拒绝 | 已产出 |
| `credential.trustTimeline` | 信任撤销前后签发的凭证判定（既往不咎时间线） | 已产出（C2 core/examples/gen_credential_vectors.rs） |

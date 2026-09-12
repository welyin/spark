# 同步与存证规格（schema / sync / evidence）

> 来源：反向提取自 `desktop/src/main/db/{schema,sync,evidence}.ts`。
> Rust 实现必须逐字节对齐 canonical JSON 与哈希链规则；以 golden vectors 验收
> （向量文件在 code 仓库 `spec/vectors/sync-evidence.json`）。

## 1. Canonical JSON（最高优先级，所有哈希的根基）

`normalizeObject(value)` 规则（evidence.ts:22-31）：

- `undefined` → 字符串 `"undefined"`
- `null` → 字符串 `"null"`
- 非 object（number/string/boolean）→ `JSON.stringify(value)`
- object → 新对象：key 按 `Object.keys(value).sort()`（JS 字符串排序=UTF-16 code unit 序），
  每个 value **先递归 normalize 再作为值**放入，最后 `JSON.stringify(ordered)`

⚠️ 关键语义：object 的递归值在 JS 里是 normalize 后的**字符串**（嵌套对象被序列化成字符串嵌入）。
即 `normalizeObject({a:{b:1}})` = `{"a":"{\"b\":1}"}` —— 内层对象变成 JSON 字符串值。
Rust 实现必须复刻这个"嵌套字符串化"行为，不能做成常规 canonical JSON。

⚠️ JS `JSON.stringify` 细节需对齐：数字格式（整数无小数点、浮点按 JS Number→String 规则）、
字符串转义（非 ASCII 不转义、控制字符短转义）、无空格。数组会落入 object 分支
（`typeof [] === 'object'`，key 为 "0","1",...）。
⚠️ key 序的真实行为（已由向量固化，以此为准）：normalizeObject 按字典序 sort，但
`JSON.stringify` 对**整数型 key**（canonical array index，< 2^32-1）恒按**数值升序**输出，
与插入/排列顺序无关——所以 `[0..10]` 输出为 `0,1,2,…,9,10` 数值序，而非字典序 `"10"<"2"`。
字典序仅对非整数型 key（如 `"b","a","A"`）与 ≥2^32-1 的数字字符串 key 体现
（参见 vectors 用例 `array-indices-numeric-order`、`integer-like-keys-ordering`）。
Rust 侧序列化器必须复刻：先分离整数型 key（数值升序）再排其余 key（字典序）。

## 2. 存证链（evidence）

条目字段：`seq, prevHash, domain, collection, id, op('put'|'delete'), dataHash, payloadHash, metaHash, hash, timestamp, nodeId`

- `payloadHash` = sha256hex(normalizeObject(payload))；payload 为 null/undefined → null
- `metaHash` = sha256hex(normalizeObject(meta))；同上
- `dataHash` = sha256hex(normalizeObject({domain, collection, id, op, payloadHash, metaHash}))
- `entry.hash` = sha256hex(normalizeObject({seq, prevHash, domain, collection, id, op, dataHash, payloadHash, metaHash, timestamp, nodeId}))
- 链：`seq` 从 1 递增；`prevHash` 指向前一条目 hash，首条为 null
- 存储 key：`doc:evidence:proof:{seq 左补零至 12 位}`；头指针 `doc:evidence:head` = `{seq, hash}`
- 校验：从 1 遍历到 head.seq，逐条验 prevHash 与重算 hash

## 3. 集合策略注册表（schema）

- 策略：`append-only`（默认/治理强制）| `lww`
- 归一化（resolveSchemaDeclaration）：
  - syncStrategy 必须是两值之一，否则抛错
  - `governance=true` 且非 append-only → 抛错（禁止降级）
  - append-only → `enableEvidence` 强制 true；lww → 取声明值（默认 false）
- 未声明集合默认策略：`{append-only, governance:false, enableEvidence:true}`
- 存储 key：`doc:system:collection-schema:{encodeURIComponent(domain + "/" + collection)}`
- 集合名正则：`^[A-Za-z0-9_-]+$`
- 声明幂等：同策略重复声明返回既有记录；冲突声明抛错；**一旦声明不可变更**
- 同步消息携带的 schema 仅经 sanitizeSchemaHint 合法化后作**瞬时兜底**（本地未声明时），
  **永不写入注册表**（防远端锁死/降级本地策略）
- sanitizeSchemaHint：syncStrategy 非法 → undefined；governance=true 且非 append-only → undefined

## 4. 版本向量与 LWW（sync）

- meta：`{vv: {nodeId: counter}, ts: number, nodeId?}`，key `meta:{domain}:{collection}:{id}`
- `compareVersionVectors(local, remote)` → 'local'|'remote'|'concurrent'|'equal'
  （逐 key 取大比较；双null → 'equal'）
- `resolveConflictByLWW(localTs, remoteTs)`：null 按 0；`>` 严格比较，相等 → 'equal'
- `mergeVersionVectors`：逐 nodeId 取 max（append-only 幂等去重后促进收敛）

### applyRemoteUpdate 流程

1. **purge 水位线拦截**：`remoteMeta.ts < 水位线` → 拒绝落地（防已清理数据回灌）
2. schema hint 仅 sanitize 后作兜底；解析生效策略
3. append-only 分支：
   - 远端删除 → 拒绝（告警）
   - 本地无此 doc → 接受写入（doc + meta + 索引 + evidence[op=put]）
   - 本地已有且 `payloadHash(local) === payloadHash(remote)` → 幂等去重，合并 vv（取大）与 ts（取大），有变化才写 meta
   - 本地已有且载荷冲突 → 拒绝保留本地（告警）
4. lww 分支：
   - `cmp==='remote'`：落地远端（put：写 doc+索引 diff+meta+evidence；delete：删 doc+索引、写 tombstone meta `{vv,ts,tombstone:true}`+evidence[op=delete]）
   - `cmp==='local'` / `'equal'`：不动
   - `cmp==='concurrent'`：按 ts 裁决 LWW；remote 胜出走同上落地（含 evidence），否则不动
   - 注意：enableEvidence 的 lww 集合，cmp=remote 与 concurrent-remote 两分支都写 evidence（已修齐）

## 5. 验收向量（code 仓库 `spec/vectors/sync-evidence.json`）

1. normalizeObject：嵌套对象/数组/null/undefined/数字/中文串 → 精确输出字符串
2. payloadHash/dataHash/entryHash 固定输入 → 固定 hash（≥3 组）
3. 三条目链式构建 → seq/prevHash/hash 链精确匹配
4. compareVersionVectors 全分支（local/remote/concurrent/equal/双null）
5. resolveSchemaDeclaration 全分支（含抛错用例）

---

## 6. 节点锚记录（阶段四F，已实现）

> 依据：`evidence-anchoring-export`（wiki architecture/sync/）
> §1。存证链是每节点本地全局单链（跨节点无全局全序，无 PBFT 是既定决策）；
> 跨节点完整性互证 = 本节锚定机制。前提：链/哈希/canonical 规则（§1–§2）
> 不变，锚是链头承诺的**可引用声明**。
>
> **验签材料内嵌（协议守护已会签——见 `evidence-anchor-cosign`（联签记录，wiki protocol/org/））**：
> 锚记录内嵌 `rootId` + `publicKey`（b64 32B）——rootId = sha256hex(公钥)
> 不可逆推，不带公钥则消费点验签（§「同步面不验签」的既定分工）无从执行；
> 两字段**入签名载荷**（防公钥替换，nodeInfoClaim 既有先例），签名载荷 =
> 剔除 `sig` 的**全部**字段 canonical。验收以 golden vectors
> （`code/spec/vectors/evidence-anchor.json`）为准。

每节点一条 **LWW 自覆盖**记录，声明本地链头承诺：

```json
{
  "anchorV": 1,
  "orgId": "org_<16hex>",
  "nodeId": "<设备 peerId>",
  "rootId": "<64hex>",
  "publicKey": "<b64 32B>",
  "headSeq": 142,
  "headHash": "<sha256hex 链头>",
  "ts": 1720000000000,
  "sig": "<b64 64B>"
}
```

| 字段 | 约束 |
| --- | --- |
| `anchorV` | 恒 1（版本字段，后续演进用） |
| `orgId` / `nodeId` | nodeId = 设备 peerId（与 vv 分量键同口径）；锚是节点级声明，成员资格由既有成员表面确认 |
| `rootId` / `publicKey` | 签名方 root 身份 id 与公钥（b64 32B）——内嵌验签材料；消费方验签前置校验 `sha256hex(base64decode(publicKey)) == rootId`（公钥-rootId 绑定，同 nodeInfoClaim 口径） |
| `headSeq` / `headHash` | 声明时刻本地链头（`doc:evidence:head`）的 seq/hash |
| `ts` | 声明方本地 Unix 毫秒 |
| `sig` | 节点 **root 身份** Ed25519 签名；载荷 = **剔除 `sig` 的全部字段**（`anchorV/orgId/nodeId/rootId/publicKey/headSeq/headHash/ts`）的 **canonical JSON**（§1 `normalizeObject` 规则）UTF-8 字节 |

- **存储与同步面**：键 `org:evi:anchor:{orgId}:{nodeId}`，并入 orgsync
  `org:structure@v1` 键域（all-members，与 `org:coll:` 同列——
  见 `org-orgsync`（待 A53 改写，wiki protocol/org/） §20.2.3；`org:acl:` 原亦同列，已随 C7
  encrypted 轴退役移出该键域），零新通道；逐键 lww-record
  （同 `org:member:` 口径：ts/vv 裁决，每节点只写自己的键）；
- **同步面不验签**：锚按既有复制组/白名单规则流动；密码学校验（sig）
  在**消费点**执行（治理面采纳、导出、独立核验）——同步层不替消费方
  做信任裁决；
- **触发（实施口径，已会签）**：链头变化挂钩（doc 写/删后幂等刷新——链头
  未变不写锚）+ p2p 启动兜底（停机期间的链头变化收敛）+ 治理事件驱动的
  显式 API；三条触发路径幂等覆盖原「每日周期兜底」（锚随链头即时收敛，
  周期兜底无独立必要）；频率节制由幂等性 + 既有按 from 限流承担；
- 个人域（pdsync 自设备）锚本期不做（设计 §1.2）。

## 7. 组织锚根（默克尔树）

纯派生、**不落库为独立记录**：任一节点对「当前已知全部成员锚记录」计算
默克尔根，作为组织存证状态的单哈希引用点（治理签名包/法定人数快照引用
`(orgId, anchorRoot, ts)`）。树的输入是**锚记录集合**（不是逐条目——链头
hash 本身已是单链承诺，逐条目进树无增益）。

- **叶**：`leaf(nodeId) = sha256(utf8("evi-anchor-leaf\x00") ‖ canonical(锚记录全文含 sig))`
  ——锚记录全文经 §1 normalizeObject 后的 UTF-8 字节；
- **排序**：叶按 nodeId **UTF-8 字节序**（字典序）排列；
- **内部节点**：`node = sha256(utf8("evi-anchor-node\x00") ‖ left ‖ right)`
  （left/right 为 32B 原始摘要，非 hex 字符串）；
- **奇数叶**：末叶复制补齐（标准二叉）；**单叶**：root = 该叶；
- 根输出 = 32B 摘要的小写 hex（64 字符）；
- **inclusion proof**（nodeId → 兄弟哈希路径）：`proof = [sibling₀,
  sibling₁, ...]`，**自叶向根**逐层排列，元素为 hex；左右位置不随证明
  携带——可推导：核验方持全部锚记录 → 按 nodeId 排序得叶索引 idx，
  逐层 `idx` 偶 = 兄弟在右、奇 = 兄弟在左，`idx ⌊÷2⌋` 上折；末层奇数
  复制规则同上。验证 = 从叶重算至根与 anchorRoot 比对。

## 8. 分叉检测语义（可举证告警，非共识裁决）

对同一 nodeId 的先后两份**签名**锚记录：

| 情形 | 判定 |
| --- | --- |
| `headSeq` 回退（后锚 < 先锚） | **分叉证据** |
| 同 `headSeq` 不同 `headHash` | **分叉证据** |
| 锚旧于本地链头（seq 小、hash 与本地该 seq 一致） | 正常滞后（锚周期滞后，下次锚定收敛） |
| 锚 `headSeq` 大于本地链高 | 本地同步未收敛（提示先同步再引锚，不作分叉） |

- 处置：告警 + 治理面**拒绝采纳该节点锚**（本地标记，不网络隔离）——
  本机制检测分歧、不做裁决，裁决归治理层（与无 PBFT 决策一致）；
- **证据保全**：同步面 LWW 自覆盖会盖掉旧锚——分叉判定点的两份签名锚
  必须各自留档（告警负载携带双份锚全文）后方可让覆盖发生，否则证据
  灭失；
- 可举证性：两份同 nodeId、互斥（回退或同 seq 异 hash）的签名锚并排即
  密码学证据，任何核验方可独立复算确认。

## 9. 存证导出包（自包含单文件 JSON）

canonical 序列化（§1 规则），全部哈希可离线复算；可打印（线下提交场景）：

```json
{
  "formatVersion": 1,
  "scope": { "domain": "plugin:vote", "collection": "ballots", "orgId": "org_..." },
  "exporter": { "rootId": "<64hex>", "publicKey": "<b64 32B>", "ts": 1720000000000, "sig": "<b64 64B>" },
  "head": { "seq": 142, "hash": "<sha256hex>" },
  "entries": [ /* 存证链条目，见下 */ ],
  "anchors": [ /* 导出时刻已知的全部成员锚记录（§6 全文含 sig） */ ],
  "anchorRoot": "<hex>",
  "anchorProofs": { "<nodeId>": ["<hex 自叶向根>", ...] },
  "spec": { "canonicalJson": "<规则摘要>", "specRef": "sync-evidence §1–§2、§6–§9" }
}
```

| 字段 | 约束 |
| --- | --- |
| `formatVersion` | 恒 1 |
| `scope` | `domain`/`collection`/`orgId` 各可省（全省 = 全链导出）；**scope 是声明性关注范围，不是 entries 的过滤条件** |
| `entries` | **全量连续链** `seq = 1..=head.seq`（§2 条目线形原样）——链式校验（逐条重算 hash + prevHash 连续）以连续性为前提，故 entries 不按 scope 过滤；范围核验 = 覆盖性断言（scope 声明的 domain/collection 条目在链内存在且完整） |
| `exporter.sig` | 导出者 root 身份 Ed25519 签名；载荷 = canonical(包全文**剔除 `exporter.sig` 字段**) 的 UTF-8 字节 |
| `exporter.publicKey` | 导出者公钥 b64 32B（内嵌验签材料，已会签：rootId 不可逆推公钥，离线核验必需；入签名载荷防替换，同 §6 锚记录口径） |
| `anchors`/`anchorRoot`/`anchorProofs` | §6/§7 口径；proofs 键 = nodeId；**无任何已知锚时 `anchorRoot` 为 `null`、`anchorProofs` 为空对象**（anchors 空数组） |
| `spec` | 自描述指针：`canonicalJson` = §1 规则人读摘要，`specRef` 恒 `"sync-evidence §1–§2、§6–§9"` |

- 核验方零外部依赖（包内含验证全部材料）；**分层诚实标注**：无组织
  成员表的外部核验方能验证「密码学完整性与签名有效」，不能验证
  「签名者当时是成员/角色」——成员资格核验增强（包内附成员表存证锚
  快照）列后续；
- 与 data-mgmt 全库导出正交：本包是**存证专用**面，不含文档明文。

## 10. 验收向量（锚定与导出包；实施落 `code/spec/vectors/evidence-anchor.json`）

向量组（协议先行，实施以本清单验收；消费测试
`core/tests/evidence_anchor_vectors.rs`）：

1. **锚记录**：固定 orgId/nodeId/root 密钥/headSeq/headHash/ts →
   canonical 签名载荷串逐字节 + `sig` 固定值；篡改任一字段验签必败；
2. **默克尔根**：同一锚记录集合 → 固定 anchorRoot；覆盖单叶 / 偶数叶 /
   奇数叶（末叶复制）三形态；
3. **inclusion proof**：固定树 → 每叶 proof（自叶向根兄弟 hex 序列）逐字节；
   验证通过 + 篡改 sibling/换叶索引失败用例；
4. **导出包**：固定链（≥3 条目）+ 固定锚集 + 固定导出者密钥/ts →
   canonical 包字节 + `exporter.sig` 固定值；逐字段篡改（条目/hash/锚/
   proof/签名）各项必败；断链（删中间条目）必败。


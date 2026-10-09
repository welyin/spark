# affair（事务容器）

> 状态：**容器纯逻辑已落地（C1，code/core/src/affair/）；复制面已落地（C4，[affair-sync](affair-sync.md)）；账龄/阶梯推导与决议组织效力钩子已落地（C6，§13）；决议结论存证条目已落地（A21，§6.3）**。依据：`总体方案`（wiki architecture/community-affairs.md） §3（含 §3.3 可判定性纪律、§3.4 时间权威修正）、§6 方案 A1。
> 通用编码约定（canonical JSON / 签名 / 身份 id / 时间）见 `community 总约`（wiki protocol/community/README.md）；命名红线：实现层一律 `affair`，与组织本地审计日志 `org:tx:` 严格分域，禁止混用。

## 1. 定位与验证分层

事务容器 = 以 affairId 为作用域的 **append-only 签名操作日志 + 创世规则**。内核保证：容器结构、签名验证、日志完整性、关闭条件的确定性求值、法定人数快照；内容语义（选票/帖子/文件）由插件定义为签名操作的载荷，**无协议地位**。

- **非装机持有者可验证**签名链与完整性，**不能验证类型语义**——有意诚实边界（总体方案 §3.2）；
- 集合策略：append-only、签名强制、逐条存证（`governance: true` 铁则，[sync-evidence](../foundation/sync-evidence.md) §3）；
- 事务**没有托管域**（产品第七节）：由关注者复制保存，复制面（动态复制组 hello/need/data）归 C4，本文只定容器线形与本地存储键。

## 2. affairId 与创世记录

### 2.1 线形

```json
{
  "affairV": 1,
  "type": "forum",
  "title": "第二届业委会选举",
  "summary": "……",
  "tags": ["region:110105", "hoa"],
  "initiator": { "kind": "person", "identity": "<64hex>", "publicKey": "<b64 32B>" },
  "rules": { "…": "见 §5 规则文档" },
  "initialVoters": ["<64hex>"],
  "refs": [ { "target": "<64hex affairId>", "rel": "inherit" } ],
  "createdAt": 1720000000000,
  "sig": "<b64 64B>"
}
```

| 字段 | 约束 |
| --- | --- |
| `affairV` | 恒 1（版本字段） |
| `type` | 事务类型标识，插件命名空间字符串（`^[A-Za-z0-9_-]+(:[A-Za-z0-9_-]+)*$`，≤64 字符）；内核不解释 |
| `title` / `summary` / `tags` | 议题元数据（**全网索引只收录这三样**，产品级承诺）；title trim 后 1–120 字符，summary ≤1024 字符，tags ≤16 个、每个 ≤32 字符（**字符数按 UTF-16 code unit 计**，对齐 JS `String.length` 口径——含 BMP 外字符时与码点口径差 1）；区域标签约定形式 `region:<GB/T 2260 代码>`，仅用于检索聚合（不代表任何机构认可）。修订不走创世，走 `meta-revise` 操作（§11） |
| `initiator` | 发起人 `Actor`（§2.2）；创世签名主体 |
| `rules` | 规则文档（§5），含参与门槛声明与关闭条件 |
| `initialVoters` | 初始投票者集合（身份 id 列表，元素匹配 `^[0-9a-f]{64}$`，去重）；**缺省（字段缺席或 null）= 仅发起人**（冷启动，产品「参与阶梯详设」）；**显式空数组 = 零初始投票者**（创世事务可以没有任何初始投票者，投票者此后只能经 §13 阶梯口径爬升产生）；权力随贡献者爬升稀释 |
| `refs` | 事务间引用（§10），可空数组；创世即声明的谱系（如继承事务引用前任） |
| `publish` | 可选，**公开发布声明位**（A55，总体方案档二-2 拍板的参数化扩展）：严格布尔 fail-closed——仅显式 `true` 生效，`"true"`/`1`/`{}`/`null` 等其余形态被 affairId 承诺但不触发任何行为；**缺省不携带**（显式 `false` 同样不写入，canonical 保持最小，存量创世逐字节兼容）。声明公开的事务在关注/创世入站后触发元数据公告（[affair-metadata](affair-metadata.md) §4，复用 C10 `indexer_publish_meta` 通路，公告失败不阻断关注与落账）。本字段是内核首个**解释**的创世顶层字段（此前顶层字段一律「不解释但承诺」），属向后兼容的行为扩展 |
| `createdAt` | 发起人本地毫秒，仅展示用；**权威时间见 §7** |
| `sig` | initiator 私钥对 `canonical(剔除 sig 的全部字段)` 的签名 |

### 2.2 Actor（操作者/签名主体）

```
{ kind: 'person' | 'org', identity: '<64hex>', publicKey: '<b64 32B>', orgSig?: <OrgSigSet> }
```

- `identity` 必须满足 `identity == sha256hex(base64decode(publicKey))`（验签前置，公钥-身份绑定，同 nodeInfoClaim 口径）；
- `kind: 'person'`：个人参与者。身份两种（用户自选，产品第六节）：
  - **上下文身份**（默认）：域身份派生（[identity](../foundation/identity.md) §4），域串 = `affair:{affairId}`，跨事务不可关联；
  - **公共身份**：一个稳定的公开账号（opt-in），其全部参与构成公开履历；
- `kind: 'org'`：组织**表态**（展示级立场，不爬阶梯、不表决、无票权——产品第六节）；必须携带 `orgSig` = 满足该组织策略的签名集合（`org-signature`（待 C3，wiki protocol/community/）），`identity`/`publicKey` = 该组织在本事务上下文的展示身份（可为 `affair:{affairId}` 域串按 `org-genesis`（待 A53 改写，wiki protocol/community/） §4 派生的组织域身份）；
- **创世自举例外**：创世记录的 initiator 派生域由实现自选（affairId 未定，`affair:{affairId}` 域串不可用——sha256 不动点不可计算）；协议只约束 §2.1 验签与 affairId 计算。创世之后的一切操作可用 `affair:{affairId}` 上下文身份。

### 2.3 affairId

```
affairId = sha256hex(normalizeObject(创世记录剔除 sig))   // 64 hex 小写
```

- 自认证：持创世记录即可复算 affairId，无需外部锚；
- 创世记录内任何字段（含 type/rules/initialVoters/refs）都被 affairId 承诺，事后不可改——规则修改走 `rule-change` 操作（§5.3），不产生新 affairId。

## 3. 操作日志条目

### 3.1 线形

```json
{
  "opV": 1,
  "affairId": "<64hex>",
  "prevOpHash": "<64hex>",
  "opType": "content",
  "payload": { "…": "按 opType，见 §4/§5" },
  "actor": { "kind": "person", "identity": "<64hex>", "publicKey": "<b64 32B>" },
  "declaredAt": 1720000000000,
  "sig": "<b64 64B>"
}
```

| 字段 | 约束 |
| --- | --- |
| `opV` | 恒 1 |
| `affairId` | 必须等于所属事务（入站校验，防跨事务搬迁） |
| `prevOpHash` | 操作者观察到的日志头 opHash；**首条操作 = affairId**（创世承诺即链根） |
| `opType` | §4 枚举 |
| `payload` | 按 opType；`content` 由插件定义（内核不解释） |
| `actor` | §2.2 Actor |
| `declaredAt` | 签名者声明时间（毫秒）；**实时提交**入站新鲜度校验：`|declaredAt − now| ≤ 10 min` 超窗拒收（nodeInfoClaim 先例）；**复制入站（affairsync-data 与 drain 复检）豁免本门槛**——复制保真由签名 + opHash 链承担，时间语义各副本吃本地存证锚定时刻（§7.2），历史补齐不被时间窗锁死。declaredAt 为自报文本，仅作展示，永不进入判定（§7.2 第 4 条） |
| `sig` | actor 私钥对 `canonical(剔除 sig 的全部字段)` 的签名 |

### 3.2 opHash 与日志拓扑

```
opHash = sha256hex(normalizeObject(操作条目全文含 sig))   // 含签名：链承诺覆盖签名本身
```

- **日志是 DAG，不是单链**：并发操作引用同一 prevOpHash 即合法分支。多写者分布式环境下不存在全序单链，prevOpHash 的语义是**因果见证**（操作者声明其观察到的日志头，防凭空插入伪造历史）+ 逐条存证锚定；
- **状态推导不依赖链拓扑**，只依赖操作集合的内容（签名、快照、计数、内容哈希）——§3.3 纪律（总体方案）的线形落地；
- 入站校验：结构 → affairId 匹配 → declaredAt 新鲜度（仅实时提交；复制入站豁免，§3.1）→ actor 绑定 → 验签 → prevOpHash 指向已知条目（未知则暂存待补，不因乱序拒收）。

### 3.3 存储键（本地，关注者副本）

| 键 | 值 | 说明 |
| --- | --- | --- |
| `affair:rec:{affairId}` | 创世记录 JSON | 每事务一条 |
| `affair:op:{affairId}:{opHash}` | 操作条目 JSON | append-only |
| `affair:head:{affairId}` | `{ heads: ["<opHash>", …] }` | 本地观察到的 DAG 头集合（无后继的条目） |
| `affair:follow:{affairId}` | 本地关注状态 | **本地键，不进任何同步流量** |

- 键前缀 `affair:` 与 `org:tx:`/`org:` 分域；orgsync-data 白名单（`org-orgsync`（待 A53 改写，wiki protocol/org/） §20.1 第 3 条）**拒收** affair 键，affair 复制面（[affair-sync](affair-sync.md) §3.3）likewise 拒收 `org:` 键；复制面线形（affairsync-hello/need/data）与关注者发现归 [affair-sync](affair-sync.md)（C4）。
- 逐条存证：每条操作入本机存证链，条目字段 `domain = "affair:{affairId}"`、`collection = "ops"`、`id = opHash`、`op = 'put'`；创世记录 `collection = "genesis"`、`id = affairId`。链规则同 [sync-evidence](../foundation/sync-evidence.md) §2（每节点本地链，无全局全序——见 §7）。

## 4. opType 枚举（内核识别面）

| opType | 内核语义 | payload |
| --- | --- | --- |
| `content` | **插件语义**（帖子/选票/文件引用/执行回报……），内核只验签名与门槛资格 | 插件定义 |
| `rule-change` | 规则修改提议；有效性由 §5.3 集体决策机制判定 | §5.4 |
| `vote` | **内核级表决票**（仅服务 rule-change / meta-revise 的 vote 变体；插件业务投票走 content） | `{ "proposal": "<opHash>", "choice": "yes"\|"no" }` |
| `objection` | 对延迟生效类提议（rule-change / meta-revise / resolution 公示）的否决异议 | `{ "target": "<opHash>", "reason"?: string ≤256 }` |
| `meta-revise` | 主持人元数据修订提议（§11.2） | §11.2 |
| `moderate` | 主持人展示层操作（§11.1，评论折叠） | §11.1 |
| `ref` | 追加事务间引用（§10） | `{ "target": "<affairId>", "rel": <§10 枚举> }` |
| `snapshot` | 法定人数/阶梯名册快照载入（§9） | §9 |
| `resolution` | 决议产物（§6） | §6.1 |

- 未知 opType：**整条拒收**（fail-closed；插件语义请走 `content`，不得发明内核 opType）；
- `vote`/`objection` 的 `proposal`/`target` 必须指向同事务已知条目，否则暂存待补（同 prevOpHash 乱序规则）。

## 5. 规则文档（rules）

### 5.1 线形

```json
{
  "engine": "b1",
  "closeConditions": [ { "…": "§5.2 可判定条件枚举" } ],
  "pubPeriod": { "delayMs": 86400000 },
  "participation": { "…": "§5.5 参与门槛声明" },
  "ruleChange": { "…": "§5.3 集体决策机制" },
  "exec": null
}
```

| 字段 | 约束 |
| --- | --- |
| `engine` | 恒 `"b1"`（预留字段，策略引擎升级路径见总体方案 §6 方案 B） |
| `closeConditions` | 关闭条件数组（任一满足即关闭）；**只许 §5.2 可判定形式**，其余内核静态检查拒绝（§5.6） |
| `pubPeriod.delayMs` | 决议/规则修改公示期毫秒；**缺省 86400000（24h），下限 24h**（§7 公示期吸收时钟偏差与副本滞后） |
| `participation` | 参与门槛声明（§5.5） |
| `ruleChange` | 规则修改的集体决策机制（§5.3）；**发起后修改规则只能走它，发起人单方不可改**（产品铁律） |
| `exec` | 执行型事务声明：`null`（纯讨论/决议型，决议即终态）或 `{ "executor": <Actor 摘要>, "verify": { "kind": "delayed-veto", "delayMs": … } \| { "kind": "verifier-sign", "verifiers": ["<64hex>"] } \| { "kind": "vote", … } }`（核查方式由事务规则声明，产品第六节「执行与核查」） |

### 5.2 关闭条件：可判定形式枚举（§3.3 纪律落地）

`closeConditions[]` 每项必须是下列形式之一：

| type | 字段 | 语义 |
| --- | --- | --- |
| `op-count` | `{ "type":"op-count", "opType":<opType>, "filter"?:<插件命名串>, "count": N }` | 按 §8 确定性排序键排序后的第 N 个匹配有效操作出现时关闭 |
| `threshold` | `{ "type":"threshold", "base": "snapshot:<opHash>"\|"ladder:voters", "num": a, "den": b }` | 满足条件的有效操作数 ≥ 基数 × a/b 时关闭（基数 = §9 快照名册） |

**threshold 计入口径（§12 登记，实现现状合法化）**：计入集合 = 有效集中 opType 为 `content` 且 filter 匹配的操作——**按操作者身份去重（一人只计一次，防单人刷屏满足比例），每人计入其 opHash 字典序（§8）最小的一条**（确定性，与到达顺序无关）；`vote` 类操作不被 threshold 计数（插件「N 票关闭」请用 op-count 表达）。
| `wall-clock` | `{ "type":"wall-clock", "notBefore": <ms> }` | 锚定时间不早于 notBefore（**近似时间，语义见 §7.2**；决议仍须公示期） |

**禁止形式**（内核静态检查拒绝，§5.6）：依赖到达顺序的条件（"本地收到的第 N 个"）、依赖本地时钟即时值的条件、无公示期的 wall-clock 立即生效。

### 5.3 集体决策机制（ruleChange / 组织级效力共用三形态）

```json
{ "kind": "vote",        "voterSet": "ladder:voters", "threshold": { "num": 1, "den": 2 }, "quorum": { "num": 1, "den": 2 }, "snapshot": "required" }
{ "kind": "multisig",    "m": 2, "n": 3, "signers": ["<64hex>", "…"] }
{ "kind": "delayed-veto","delayMs": 259200000, "vetoThreshold": { "count": 3 } }
```

| kind | 语义 | 有效性判定（内核纯逻辑） |
| --- | --- | --- |
| `vote` | 正式投票 | 法定人数快照（§9）内有效 `vote` 操作计数：同意票/快照基数 ≥ threshold 且参与数 ≥ quorum；逐票验签 + 投票者资格按快照名册 |
| `multisig` | m/n 多签 | rule-change 条目携带 `approvals`（§5.4）：≥m 个 signers 内不同身份的签名，逐一验签 |
| `delayed-veto` | 延迟生效 + 阈值否决 | 提议锚定时刻 + delayMs 内，有效 `objection` 数 < vetoThreshold → 生效；达到阈值 → 否决。锚定时刻语义见 §7.2 |

- **单点禁令（内核硬编码不可变校验，任何规则文档不能关闭）**：组织级效力（策略修改、换届、创设、预算）必须经上述三形态之一的集体决策；任何单一密钥直接产生组织级效力的规则表达 → 静态检查拒绝；
- **失联经延迟否决解锁**同为内核硬编码：multisig/vote 机制的签名者失联恢复路径 = 经 delayed-veto 修改机制本身；
- 小组织默认值用 delayed-veto 而非 multisig（多签失联死锁，产品第六节）。

### 5.4 rule-change payload

```json
{
  "mechanism": { "…": "§5.3 三形态之一" },
  "change": { "…": "新规则文档片段（patch 语义：顶层键覆盖，null = 删除该键）" },
  "approvals": [ { "identity": "<64hex>", "publicKey": "<b64>", "sig": "<b64>" } ],
  "proposedAt": 1720000000000
}
```

- `approvals` 仅 multisig 形态携带；每个 approval 的签名载荷 = `canonical({ "affairId":…, "change":…, "mechanism":…, "proposalPrevOpHash":…, "proposedAt":… })`（即 rule-change 条目剔除 actor/sig/approvals 的提议本体）；
- 生效判定满足后，新规则文档 = 旧文档应用 patch；**规则的每一版本由规则修改链确定性可溯**；
- `ruleChange` 键自身的修改同样走现行机制（含硬编码校验：不得关闭单点禁令/延迟否决解锁）。

### 5.5 参与门槛声明（participation）

```json
{
  "contribute": { "ladder": "contributor" },
  "vote":       { "ladder": "voter" },
  "credentials": [ { "credType": "household-owner", "verifierDomain": "org_<…>" } ],
  "combine": "all"
}
```

- 门槛必须**可验证**（产品铁律）；基础件两类：**账龄/阶梯**（内核从日志+存证链确定性推导，插件伪造不了链上时间）与**凭证**（[credential](../community/credential.md)，插件签发、内核验证产物）；
- `ladder` 取值 `observer`/`contributor`/`voter`（观察/评论层永远零门槛——`observer` 为缺省，门槛只卡贡献与表决）；
- 阶梯参数（在级天数、采纳次数、活跃窗口）可在 participation 内以 `ladderParams: { "contributorAccepts": 1, "voterDays": 30, "voterAccepts": 3, "activeWindowMs": 7776000000, "decayMs": 7776000000 }` 覆盖默认值（缺省值即产品默认值：1 次采纳 / 在级 30 天 + 累计 3 次采纳 + 近 90 天活跃 / 90 天衰减）；
- `combine`：`all`（缺省）| `any`；平台不设任何层级默认门槛（原则 1）。

### 5.6 静态检查（规则校验期）

规则文档载入（创世/rule-change 合入）时内核执行：

1. closeConditions 每项 ∈ §5.2 枚举，参数范围合法（count ≥ 1、0 < num ≤ den、wall-clock 必须搭配 pubPeriod ≥ 24h）；
2. ruleChange 机制 ∈ §5.3 三形态，multisig 满足 1 ≤ m ≤ n ≤ signers 长度；
3. 不出现禁用表达（到达顺序依赖、无公示期立即生效、单密钥组织级效力）；
4. `pubPeriod` 形状检查 fail-closed：字段**整体缺席**才应用缺省值（delayMs 缺省 24h、vetoThreshold.count 缺省 1）；字段 present 但形状/类型非法（pubPeriod 非对象、delayMs 非整数、vetoThreshold 非对象、count 非无符号整数）→ **malformed 拒绝**，不得静默回退缺省值让坏形状规则按缺省生效；
5. `participation.ladderParams` 参数范围合法（天数/窗口 > 0、采纳次数 ≥ 1、形状为对象）——越界在创世与 rule-change patch 应用后的校验期即拒（与阶梯读路径同口径）；
6. 任一不满足 → **拒绝合入**（创世拒绝创建；rule-change 视为无效操作，不入有效集）。

## 6. 决议产物（resolution）

### 6.1 payload 线形

```json
{
  "result": "passed",
  "condition": { "…": "被满足的 §5.2 关闭条件原文" },
  "countedOps": ["<opHash>", "…"],
  "tally": { "…": "计票结果，插件结构；内核只承诺其字节" },
  "quorumSnapshot": "<opHash of snapshot 操作>",
  "rulesHash": "<64hex>",
  "pubPeriod": { "delayMs": 86400000 }
}
```

- `countedOps`：计入关闭判定的有效操作 opHash 列表，**按 §8 排序键升序**；
- `rulesHash` = `sha256hex(normalizeObject(判定所用规则文档版本))`；
- `pubPeriod.delayMs`：**自声明值在入站解析期即须 ≥ 24h（§5.1 下限），低于下限的 resolution 操作整条拒收**；复算时还要求与判定所用 rules 版本的 `pubPeriod.delayMs` 一致（整体缺席按缺省 24h 复算），不符 → resolution 无效（`pub-period-mismatch`）——公示期是规则参数，不得由决议方自声明即时化；
- resolution 是**显式钉入日志的操作**（actor = 首先观察到关闭条件满足的节点身份）：其内容全体副本可对同一操作集合**确定性复算**；复算不符 → 该 resolution 无效（入无效集，可告警）；
- **决议 id = 该 resolution 操作的 opHash**（组织效力声明、执行回报、申诉引用之）。

### 6.2 生效语义

1. resolution 落日志 → 状态「**待确认决议**」（任何副本在公示期结束前不得呈现为已生效）；
2. 公示期 = **规则文档版本的** `pubPeriod.delayMs`（≥24h；生效态求值取规则版本公示期，不取决议 payload 自声明值——后者须与规则版本一致，§6.1）内无有效 objection（阈值同 delayed-veto 语义，默认任一有效异议即打回复核——阈值可由 rules 配置）→「**生效决议**」；
3. 执行型事务（rules.exec ≠ null）：生效决议 →「待执行」→ 执行方签名回报（`content` 操作 + orgSig 表态）→ 按 exec.verify 核查通过 →「**关闭**」（终态）；核查不通过 → 打回执行或转申诉（新事务，§10 rel=appeal）；
4. 组织效力钩子（C6）：决议对组织的效力 = 组织**事先声明**匹配 + 决议有效 + **事先性**（声明存证时刻早于决议锚定时刻）——三线形要素，声明记录线形见 `org-genesis`（待 A53 改写，wiki protocol/community/） §6。

### 6.3 决议结论存证条目（evi:resolution，A21）

**生效决议**（§6.2 两态之「生效」——公示窗无阈值异议后生效判定通过，**非**关闭条件初通过的「待确认」态）由**每个求值到该生效判定的节点**在**本机存证链**确定性自写自锚（写入点 = 组织效力钩子消费编排，与 §6.2-4 治理钩子同点；条目不流动、锚经 orgsync 全员流动——存证链本地性见 [sync-evidence](../foundation/sync-evidence.md) §6）：

```json
{ "kind": "evi:resolution", "affairId": "<64hex>", "subject": "<决议 id = resolution opHash>",
  "conclusionHash": "<64hex>", "sigSet": <OrgSigSet 原样 | null>, "effectiveTs": 1720000000000 }
```

- **存证条目定位**：domain = orgId（效力相关组织）、collection = `"resolution"`、id = 决议 opHash、op = put（与 effectrcpt / roster 承诺条目同族口径）；
- `subject` = **决议 id**（§6.1：resolution 操作的 opHash）——效力声明 / 回执 / `bornOf` 引用之的同一键；
- `conclusionHash` = `sha256hex(normalizeObject(决议 §6.1 payload))`——本体消亡后凭此哈希 + 签名可证明「该决议存在过」，持有决议原文时证内容绑定；
- `sigSet` = 决议操作 `actor.orgSig` **原样内嵌**（kind=org 的组织决议）；kind=person 的决议 → `null`（如实标注：本决议未携带组织签名集合；签名包五步验证链属 org-signature，本条目不重复校验）；
- `effectiveTs` = 生效判定所依据的存证锚时刻 = **决议在本副本链上的锚定时刻**（§7.2 时间源）。条目输入全确定性（决议记录 + 声明面 + 锚时刻）：各副本对同一决议算出逐字节相同条目的前提 = 锚定输入一致；跨副本锚时刻互异是 §7.1 既定诚实边界（无全局权威时钟），`affairId`/`subject`/`conclusionHash`/`sigSet` 四字段全网逐字节一致；
- **幂等**：本机链已存在同 (domain, collection, id) 条目 → 不重写；
- **锚定接线**：先写条目 → 再触发锚（`evidence_anchor` 幂等，链头承诺自然覆盖新条目）；
- **效力相关组织反查**（声明面）：扫 `org:effectgrant:` 键域（`org-genesis`（待 A53 改写，wiki protocol/community/） §6），取现行非撤销且键-文一致的声明记录的组织集合；**无声明组织效力的事务（纯讨论）不写此条目**；
- **迁移**：新增条目类型 append-only 自然兼容；旧版本节点读到未知条目按「忽略未知键」既有行为跳过。

向量：`code/spec/vectors/evidence-resolution.json`（条目逐字节含签名包内嵌、conclusionHash 计算；消费测试 `core/tests/evidence_resolution_vectors.rs`）。

## 7. 时间语义（§3.4 修正的协议落地）

### 7.1 无全局权威时钟

存证链是**每节点本地链**（条目时间戳 = 写入节点本地时钟 + nodeId），跨节点只有默克尔根锚定做完整性互证（[sync-evidence](../foundation/sync-evidence.md) §2/§6–§8）。**本协议不假设全局全序与权威墙钟。**

### 7.2 三条可实现语义

1. **顺序条件优先**：能写成集合/计数条件的一律写 §5.2 `op-count`/`threshold`（各节点判定一致，无时钟依赖）；
2. **墙钟条件 = 近似时间 + 公示期吸收**：`wall-clock` 条件的时间源 = 该事务操作在**关注者各副本存证链上的锚定时间**；入站侧 declaredAt 只做 ±10 分钟新鲜度门槛且仅压实时提交（§3.1）；关闭判定后必须经公示期（≥24h）——公示期吸收时钟偏差与副本滞后，公示期内任何副本可持有效 objection 异议；
   - **登记（§12）**：实现采用保守近似——锚定时间越过 `notBefore + 10 min` 容忍带才算满足；±10 min 带内一律判未满足（时钟不确定区，等更晚锚定），方向 fail-closed（更晚关闭，公示期兜底）。第二实现须按本口径判定，不得按字面「不早于 notBefore 即满足」；
3. **诚实标注**：秒级精确「到点自动关闭且全网同时一致」**做不到**；公示期结束前的决议一律是「待确认决议」。本语义对 UI/插件如实呈现，不得宣称更强。
4. **declaredAt 护栏**：`declaredAt` 是签名者自报文本，**永不进入任何判定**（关闭条件、公示期、账龄、阶梯、排序一概不消费），仅作展示且须如实标注为自报时间；任何新语义要消费 declaredAt 须协议守护会签后方可落地。

## 8. 确定性排序键

一切「第 N 个」「前 N 个」语义统一按：

```
sortKey(op) = opHash 的 hex 字符串字典序（UTF-8 字节序）
```

- 与到达顺序、链拓扑无关；同一操作集合任何副本排出同一序列；
- 计数时先过滤无效操作（验签失败/资格不符/静态检查不过），再按 sortKey 排序取前 N。

## 9. 法定人数快照（snapshot 操作）

防止窗口期拉人头/踢人操纵计票：参与基数以**投票/决议开始前的有效名册快照**为准。

```json
// payload（两形态）
{ "basis": "ladder",     "asOf": "<opHash>", "rosterHash": "<64hex>" }
{ "basis": "org-roster", "orgId": "org_<…>", "memberSetHash": "<64hex>",
  "anchor": { "orgId": "org_<…>", "anchorRoot": "<64hex>", "ts": 1720000000000 } }
```

- `ladder` 形态：阶梯名册 = 从日志确定性推导至 `asOf` 操作（含）为止的投票者集合；`rosterHash = sha256hex(normalizeObject(按 identity 字典序排序的名册数组))`，名册条目 = `{ "identity": "<64hex>" }`（一人一票，不加权）；
- `org-roster` 形态：域名册快照 = 指定存证锚点处的成员集；`memberSetHash` 口径同 `org-signature`（待 C3，wiki protocol/community/） §3；`anchor` 引用该组织锚根（sync-evidence §7），快照内容随操作携带或由副本本地提供，哈希承诺防编造；
- 快照是**操作载入日志**（签名、存证、可复算），不是本地推导缓存。

## 10. 事务间引用

统一一种机制：携带目标事务 ID + 关系类型。

| rel | 语义 |
| --- | --- |
| `inherit` | 继承（换代/分叉继承——任何人可基于同一历史副本发起继承事务，谱系公开） |
| `appeal` | 申诉（对决议的异议 = 引用被争议事务的新事务，走同一套集体决策与存证，不设平行机制） |
| `parent` | 父子分解（本事务是 target 的分解，target 为父；**只做引用与展示，不做状态耦合**——父决议不由子决议聚合，聚合由人做、索引层辅助呈现） |
| `related` | 关联（导航） |

- 创世 `refs` 与后续 `ref` 操作两路均可声明；引用 **append-only，不可撤销**（历史不可改；谱系由索引层重建展示）；
- 自指禁令：`target == 本事务 affairId` 拒绝。

## 11. 主持人展示层操作

发起人自动成为事务主持人；权限**仅限展示层**，不拥有规则修改权（规则修改走 §5.3 集体决策）。主持人失能/滥权由申诉事务处置（§10 appeal）。

### 11.1 moderate（评论折叠）

```json
{ "action": "fold", "target": "<opHash>", "reason"?: string ≤256 }
```

- 仅 `actor.identity == 创世 initiator.identity` 合法（其余拒入有效集）；
- 折叠是**展示层建议**：数据不删、不参与计数排除——客户端/indexer 自行采纳或忽略（评论垃圾防线在客户端，不进协议，产品「参与阶梯详设」）。

### 11.2 meta-revise（元数据修订提议）

```json
{ "title"?: string, "summary"?: string, "tags"?: string[],
  "mechanism": { "kind": "delayed-veto", "delayMs": …, "vetoThreshold": { "count": … } } }
```

- 仅主持人可提议；生效机制固定为 delayed-veto（防操纵发现面；阈值可配）；
- 缺省字段 = 不变（三态口径同 [identity](../foundation/identity.md) §6 updateProfile）；**title 不允许清除：显式 `title: null` 在解析期即整条拒收**（`bad-meta-revise-title`）——标题是必备元数据，「清除标题」语义不成立，不得应用期静默吞掉；
- 生效后元数据代际 metaSeq+1，经元数据面公告扩散（[affair-metadata](affair-metadata.md)）；修订历史在日志内留痕可验。

## 12. 验收向量（登记：`code/spec/vectors/community.json`）

| case 组 | 内容 | 状态 |
| --- | --- | --- |
| `affairGenesis` | 固定创世输入 → canonical 载荷逐字节 + affairId + sig 固定值；篡改任一字段 affairId/验签必败 | 已产出（生成器自产） |
| `opChain` | 固定密钥/时间戳的 3 条操作 → opHash 链（首条 prevOpHash = affairId）逐字节；sortKey 排序输出 | 已产出 |
| `ruleMechanisms` | §5.3 三形态机制文档的 canonical 逐字节（vote/multisig/delayed-veto 各一） | 已产出 |
| `staticCheck` | §5.6 拒绝用例集（到达顺序依赖、无公示期 wall-clock、m>n、单点效力、畸形 pubPeriod 形状、越界 ladderParams） | 已产出（C1 `core/examples/gen_community_affair_vectors.rs` 自产回填） |
| `resolutionReplay` | 固定操作集合 → resolution 复算一致性（countedOps 排序/rulesHash/condition/pubPeriod；含 op-count / wall-clock / threshold 三变体，threshold 含按人去重负例） | 已产出（C1 生成器自产回填） |
| `ladderDerive` | 固定日志 → 阶梯名册推导（含在级天数/衰减） | 已产出（C6 `core/examples/gen_community_ladder_vectors.rs` 自产回填；消费 `core/tests/community_ladder_vectors.rs`） |
| `snapshot` | 固定名册 → rosterHash / memberSetHash 固定值 | 已产出（纯函数） |
| `affairSync` | 事务复制面三信封 body 逐字节 + key 白名单拒收用例 | 已产出（C4 `core/examples/gen_community_affair_sync_vectors.rs` 自产回填；规格 [affair-sync](affair-sync.md)） |

提取计划（占位组）：无 affair 侧占位组。C1 已落地：实现 `code/core/src/affair/`，生成器 `code/core/examples/gen_community_affair_vectors.rs`（生成前对 C0 affair 组逐字节复核），消费测试 `code/core/tests/community_affair_vectors.rs`；C6 已落地：账龄/阶梯推导 `code/core/src/affair/ladder.rs`、决议组织效力钩子 `effect.rs` + kernel 门面 `affair_org_effects`，生成器 `code/core/examples/gen_community_ladder_vectors.rs`，消费测试 `code/core/tests/community_ladder_vectors.rs`——先向量后实现的纪律不变（生成器即规格的可执行重述）。

## 13. 账龄与阶梯推导口径（C6 落地登记）

账龄与阶梯由内核从操作日志 + 本副本存证链锚定时刻**确定性推导**——与到达顺序、链拓扑无关；时间权威见 §7.2：只认锚定时刻，`declaredAt` 声明时间不参与，未锚定操作不参与一切时间推导。实现 `code/core/src/affair/ladder.rs`（纯逻辑）与 kernel 门面 `affair_ladder_status`；验收向量 `ladderDerive`（§12）。

- **账龄** = 求值时刻 − 身份最早链上活跃时刻（其署名的 person 操作中锚定时刻最小者）；无链上活动 → 无账龄；
- **采纳**：经 delayed-veto 生效（锚定 + delayMs 已过且有效异议 < 否决阈值）的 meta-revise / rule-change 操作，计入提议者（actor）一次采纳，生效时刻 = 锚定时刻 + delayMs；vote/multisig 形态的规则修改同样产生效力但**不计采纳**（采纳口径取最小集）；
- **贡献者**：累计采纳 ≥ `contributorAccepts`（缺省 1）；在级起点 = 第 N 次采纳的生效时刻；
- **投票者**：贡献者 且 在级满 `voterDays`（缺省 30 天）且 累计采纳 ≥ `voterAccepts`（缺省 3）且 近 `activeWindowMs`（缺省 90 天）内有链上活动；
- **在级衰减**：投票者近 `decayMs`（缺省 90 天）无链上活动 → 降回（有采纳记录者降回贡献者；零采纳的初始投票者降为观察者）；采纳与在级起点保留累计，恢复活动并重新满足条件即回级；
- **初始投票者**（§2.1 `initialVoters`，冷启动）自创世锚定时刻起为投票者；无任何链上活动则无衰减时刻可判，保留投票者；
- **组织表态**（actor kind = org）不爬阶梯、不进名册、无票权（产品第六节）；
- **作用域**：推导只在单个 affair 的操作集合上进行（逐治理上下文），不产出任何全局/跨事务分值（防老号交易）；
- 名册条目按 identity 字典序，一人一票不加权；投票者集合即 §9 快照 `rosterHash` 的名册来源（`snapshot` 操作载入的名单须与该集合复算一致）。

决议组织效力钩子（org-genesis §6 的判定侧）同本节约径：声明记录（`org:effectgrant:` 键域）逐条 × 本事务决议逐条求值三线——事先声明存在、决议生效（§6.2）、声明存证锚定**严格早于**决议锚定；三线齐备产出待应用事件（kernel 门面 `affair_org_effects`），名册/策略的实际应用与回执归编排层。声明锚定时刻 = 存证链上 payloadHash 与现行声明记录逐字节一致的最早 put 条目时刻（声明键 append 语义：历史覆盖在链上留痕）。

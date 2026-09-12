# 架构设计：治理（公示延迟禁令、决策结构下放与签名三形式）

> 模板：产品目标 → 现状 → 差距 → 目标设计 → 迁移路径 → 验收。对应产品文档：[../../product/affairs/governance.md](../../product/affairs/governance.md)（定稿）。本篇是 affairs 层重构重心（product/todo #20、#21、#19）。

## 一、产品目标（来自 product/affairs/governance.md）

1. **透明与延迟禁令（唯一硬约束，底层不可变）**：章程级效力（策略修改 / 换届 / 创设 / 预算 / 改组）必须**公示 N 天后才生效**、全程留痕（时间以存证链为准）；
2. **决策结构下放**：谁能提议、公示期内谁能否决、需要多少人批准——独断 / 多签 / 投票 / 延迟否决皆可，由组织策略声明，内核只做确定性求值、不干预；小组织推荐默认"延迟 + 否决"（模板层）；
3. 效力来源必须事先声明（防治理偷袭）；决议能做的事不限类型；
4. **组织签名三形式**：anyAdmin / m-of-n / **门限签名**（可识别门限，**签名者可溯 = 硬前提**，不满足宁可不做）；签名验证回查历史名册；
5. **中心化提醒**（product/todo #19）：配置中心化决策结构时明确提示密钥保管纪律（权力越集中，钥匙越要看好）；
6. 策略可引用插件**契约发布物**（股权等快照）作资格 / 权重输入（product/todo #23，设计在 plugins/plugin-types 篇，本篇只列接口约束）。

## 二、现状（核实日期：2026-09-08）

**wiki**：`org-signature.md`（OrgSigSet 线形、五步验证链、anyAdmin / m-of-n、sigSetV=1 预留门限、legacy 降级标注）；`affair` §5.3（集体决策机制 + **单点禁令硬编码** + delayed-veto 失联解锁路径）；`community-affairs.md` §7.1。

**代码**：

- `core/src/affair/rules.rs`：**单点禁令硬校验**——`multisig m < 2` 拒绝（:143 注释、:324 "m == 1 时任一单一密钥即可产生效力"判非法）、`delayed-veto` 参数校验（delayMs ≤ 0 或否决阈值 < 1 拒绝）；
- `core/src/affair/decide.rs`：`evaluate_vote` / `evaluate_multisig` / `evaluate_delayed_veto` 三形态求值；`rulechain.rs`（`evaluate_rule_change`：delayed-veto = 锚定 + delayMs 窗口 + 异议计数；multisig = 提议锚定时刻签名集）；
- `core/src/org/sigset.rs`：OrgSigSet 五步验证链（`OrgSigSetVerifyContext` 纯逻辑 + `verify_detailed` 含 degraded 标注），向量组 anyAdmin / mOfN / tamper / legacyDegraded 已产出；
- meta.rs（元数据修订固定 delayed-veto）、ladder.rs / profile.rs（阶梯与履历，采纳复用 delayed-veto 口径）。

**核心冲突**：rules.rs 的 m<2 拒绝 = 旧单点禁令——它使"独断"（m=1）在**任何机制下都不可表达**，与决策结构下放直接冲突；且 vote / multisig 机制**没有时间窗字段**（仅 delayed-veto 有 delayMs），"公示 N 天后生效"对多签 / 投票在**机制 schema 层**无承载（rulechain.rs 实现判定 multisig = 提议锚定时刻）。

**既有 pubPeriod 如实登记**：规则文档级 `pubPeriod` 已存在——缺省/下限 24h 硬校验（`rules.rs:137` `InvalidPubPeriod`），wiki affair §5.1 名义覆盖"决议/规则修改公示期"，§6.2 决议生效必经公示期，`resolution.rs` 有 `PubPeriodMismatch` 拒绝。它与本设计机制级 `delayMs` 的关系在 §4.1 钉清。

## 三、差距

| # | 差距 | 出处 |
| --- | --- | --- |
| G1 | rules.rs 单点禁令硬校验（m<2 拒绝）须改为**公示窗硬校验**（一切章程级机制 delayMs > 0 强制），m=1 合法化（独断 + 公示窗） | product/todo #20 |
| G2 | vote / multisig 机制无公示窗语义，须统一进"锚定 → 公示窗 → 生效"模型 | product/todo #20 |
| G3 | 门限签名（可识别、签名者可溯）未做，sigSetV=1 预留待启用 | product/todo #21 |
| G4 | 中心化配置的密钥保管提示（策略配置 UI） | product/todo #19 |
| G5 | 策略求值输入扩展（契约发布物快照）——接口约束 | product/todo #23（plugin-types 篇） |

## 四、目标设计

### 4.1 机制模型统一：「锚定 → 公示窗 → 生效」（#20 核心）

一切章程级 ruleChange 统一为三段式，**决策结构只决定"公示期内满足什么条件"**：

```
提议锚定（存证链时刻 T0）
  → 公示窗 delayMs（内核硬强制：delayMs > 0，时间以存证链为准）
    → 生效判定（按策略声明的决策结构求值，见下表）
```

| 决策结构（策略声明） | 公示期内生效条件 | 对应现状 |
| --- | --- | --- |
| `delayed-veto { delayMs, vetoThreshold }` | 窗末判定：窗内有效异议 < 否决阈值即生效 | 已有，零改动 |
| `vote { delayMs(=投票期), threshold, quorum?, snapshot? }` | 窗末判定：计票达通过阈值即生效（窗即投票期；法定人数 quorum 与名册快照 snapshot 字段保留、不取消） | vote 补 delayMs 字段（投票期复用为公示窗） |
| `multisig { delayMs, m, n }` | 窗末判定：窗内集齐 m 个有效分量则生效（纯延迟语义）；**m ≥ 1 合法**（m=1 = 独断 + 公示窗） | 补 delayMs 字段；m<2 拒绝**删除** |
| ~~`single-admin`~~ | 不新增 kind——独断直接表达为 multisig m=1（文档与 UI 用"独断"表述，schema 面不膨胀） | 无（收敛裁定） |

**生效判定时点一律 = 窗末**（本条是防架空的关键）：窗内只收集条件（票数 / 分量 / 异议），判定在窗末执行——否则 m=1 的 multisig 提议锚定即"集齐"，公示窗被架空，恰是本重构要封死的旁路。

**与既有 pubPeriod 的关系（本篇裁定）**：章程级 ruleChange 由机制级 `delayMs` 承载公示窗，`pubPeriod` 字段对章程级动作**退役**（其 24h 下限校验随之不适用于新模型——内核唯一硬约束是 `delayMs > 0`，下限数值属组织自治权衡，UI 模板推荐 ≥24h）；普通决议的 pubPeriod 语义（§6.2 决议生效必经公示期）保留不变。

**rules.rs 校验改写**（唯一硬约束的全部内容）：
1. 一切章程级机制 `delayMs > 0` 强制（≤0 / 缺失 → 拒绝）——**没有反应窗口的透明等于没有透明**；
2. 删除 m<2 拒绝（单点禁令退役）；m=1 合法；
3. delayed-veto 既有参数校验保留；
4. **留痕**：生效决议写 `evi:resolution` 条目（affair-model 篇 §4.1）——公示 + 延迟 + 留痕三件套闭环。

**公示延迟禁令的适用范围（回写 membership 篇裁定）**：除章程级 ruleChange 外，下列**组织声明记录**（非 affair ruleChange 的组织级动作）同受约束——开放面扩大方向公示 + 延迟生效、收窄即时：名册开放声明（`org:disclosure:`，membership §4.3）、准入策略声明（`acceptCredentials`，membership §4.5）、组织域内容处置（public-topics §4.5）、org-pinned 接受声明（affair-model §4.2）。理由同一：防管理员瞬间动作造成不可逆后果。

时间语义不变：一切窗口以存证链锚定时刻为准，不采信本地时钟（既有纪律）。

### 4.2 策略文档与签名策略（org policy）

`signingPolicy`（日常代表行为）扩展同族字段：`{ kind: any-admin | m-of-n | threshold, ... }`——anyAdmin / m-of-n 已有；**threshold 见 §4.3**。策略修改本身属章程级 → 必须经 §4.1 的 ruleChange 事务（公示延迟）——签名策略修订链（`org:policy:` policyHash 链）与 ruleChange 联动在迁移节说明。

### 4.3 门限签名（#21，sigSetV=2）

- **硬前提（不可妥协）**：**签名者可溯**——验证方必须能确认"哪些分片持有者参与了本次签名"（分量可识别），普通 FROST 聚合签名（匿名分量）**直接出局**；
- **选型方向（按优先级评估）**：① 带可识别分量的门限方案（identifiable threshold / 分量证明）；② 保守替代 = **多签聚合位图**（ signatures[] 照旧 + 对外呈现单一聚合公钥的索引方案）——若①评估不满足可溯性，用②以"形式上的单一公钥 + 实质可溯分量"交付，不硬上门限密码学；
- **线形**：`sigSetV: 2`（版本 bump 的既有预留启用）；`signingPolicy` 新增 `{ kind: "threshold", t, n, sharesCommit }`；**分片分布由组织策略声明**（哪些成员持分片、阈值多少——分片分配记录入策略文档，变更走公示延迟）；
- **核心价值场景**：组织对外单一稳定公钥（不懂 Spark 语义的外部系统可验签）；分片刷新（换届 / 失联）按 governance 治理连续性既有语义（钥匙是耗材、策略是本体）；
- **硬门槛**：选型报告必须先回答"验证方如何逐分量识别签名者"——答不出，本项目不启动（产品原话：不满足宁可不做）。

### 4.4 中心化密钥保管提醒（#19）

策略配置界面（组织管理应用）在检测到中心化配置（multisig m=1 / single-admin / threshold 分片集中于 ≤2 人）时，展示醒目提示：权力集中度说明 + 密钥保管纪律（生物识别 / 芯片隔离 / 助记词备份，引 identity 篇）+ "权力越集中，钥匙越要看好"。纯 UI 义务，无内核改动。

### 4.5 契约发布物输入（#23 接口约束，设计归 plugin-types 篇）

rules / policy 求值器接受的"资格 / 权重输入"扩展一种来源：**插件按契约发布的数据快照**（发布物 = { 插件签名, 存证锚, 时点, 数据哈希 }）；求值器只验证来源 / 完整性 / 时点（快照须早于决议开始 T0），不理解语义；**不存在求值器直读插件集合的通道**（产品拍板）。

**求值输入白名单（结构封死，防钱权交易的落点）**：求值器接受的输入类型为封闭集合——名册 / 票数 / 签名集 / 契约发布物快照；**内核经济对象（积分余额 / 债权 / 任何 economy 状态）永远不得作为票权 / 资格输入**（product/affairs/governance「内核经济对象仍不得作为票权输入」；capital-defense 防线 #8、fiat-compliance 闸门④的校验点在此）。新增输入类型须走规则 schema 演进（公示延迟），白名单在求值器入口处静态强制。

## 五、迁移路径

1. **rules.rs 校验切换**：m<2 拒绝删除 + delayMs 强制——**存量无窗规则 grandfathered**：旧校验放行的存量 vote/multisig 规则继续有效（replay 确定性不受影响——不读入注入，注入会破坏各节点视图一致性与 rulesHash 复算，`PubPeriodMismatch` 有先例）；新校验只作用于**新提交 / 修订**的规则文档；UI 提示存量组织尽快经一次 ruleChange 补窗（补窗本身走公示延迟）；delayed-veto 规则零影响；
2. **org:policy 链**：签名策略修订走 ruleChange（公示延迟）后写新 policyHash——`org:policy:` 生产方（org-genesis 头部自承缺口）随本批一并接线；
3. **sigSetV**：验证器对 v=2 未启用前拒绝（现状即拒绝非 1），门限上线后双版本并行；
4. **执行顺序**：G1/G2（模型统一）→ G5（求值输入扩展，配合 plugin-types 篇）→ G4（UI）→ G3（门限，独立排期）。

## 六、验收

- **向量**：rules 校验新真值表（delayMs=0 必败 / m=1+delayMs 通过 / m=0 必败 / 阈值非法必败）；multisig 带窗生效与超时失败；vote 窗口即公示期；evi:resolution 联动；门限（若启动）签名者可溯性用例（验证方逐分量识别）；
- **单测**：存量规则补窗迁移；策略修订链 policyHash 联动；
- **集成**：独断组织（m=1）章程修改——公示 3 天 → 生效 → 留痕；延迟否决失联解锁路径（§5.3 既有）在新模型下回归；signature 向量组（anyAdmin/mOfN/tamper/legacyDegraded）全绿；
- **回归**：rules / rulechain / decide / meta / ladder / profile 既有测试（m<2 拒绝相关用例按新真值表改写）。

---

> 关联：product/todo #20（§4.1–4.2）、#21（§4.3）、#19（§4.4）、#23（§4.5）；architecture/affairs/affair-model.md §4.1（留痕闭环）；architecture/community/membership.md §4.4（签名面域私钥——OrgSigSet 分量密钥来源切换的衔接）；`org-signature.md`（签名包规格权威，sigSetV=2 在此扩展）。

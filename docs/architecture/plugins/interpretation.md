# 架构设计：解释权模型（声明式策略引擎与凭证）

> 模板：产品目标 → 现状 → 差距 → 目标设计 → 迁移路径 → 验收。对应产品文档：[../../product/plugins/interpretation.md](../../product/plugins/interpretation.md)（定稿）。

## 一、产品目标（来自 product/plugins/interpretation.md）

1. 定义权（内核最小集：身份 / 事务容器 / 凭证 / 数据集合 / 积分状态机 / 存证）与解释权（下放）分离；
2. 解释的两种产物：**A. 声明式规则文档**（内核确定性求值：开放三档、字段级授权、向上开放矩阵、城门规则）与 **B. 凭证 / 产物**（内核只验证形式与签名，不重跑现实判断）；
3. 执行点不跑插件自有代码；fail-closed 缺省拒绝；保存时静态分析（查冲突 + 暴露面扩大必须显式确认）；规则哈希自认证；
4. 三级公开（规则文本 / 实现开源 / 结果可复算）；规则代际化、可替换，数据不属于插件；
5. 可见性新框架：**写时开放声明 + 读时城门**（product/membership §二/§五，本篇求值器口径随之重排）。

## 二、现状（核实日期：2026-09-08）

**wiki**：`wiki/protocol/community/policy.md`（声明式策略求值规格）；`community-affairs.md` §7 方案 B（B1 最小声明式规则集选型已定：零新依赖、覆盖名册三档 / 开放矩阵 / 字段掩码，`engine` 字段预留换 Cedar）。

**代码**：

- **策略引擎已建成（B1）且已在生产通路接线**：`core/src/policy/`——`doc.rs`（策略文档线形与哈希自认证）、`eval.rs:93 evaluate_read`（求值器）、`analyze.rs`（静态分析）。生产调用方：orgq credential 类 readPolicy 查询门禁（`kernel/inbound_dm/orgq.rs:261 verify_read_auth`、`:324 evaluate_read` 求值第 5 步，`tests/kernel_orgsync_inbound/read_gate.rs` 覆盖——membership 篇 §二 O3 同口径）；`analyze` 接线于 `kernel/policy_ops.rs:75`（`policy_submit_draft` 保存路径与上一版草稿比对暴露面）；
- **policy 门面（C9）已落地**：`kernel/policy_ops.rs` `policy_read` / `policy_submit_draft` / `policy_publish`——publish 已实现"analyze → OrgSigSet 签署 → 存证锚 → `org:policydoc:` 发布键域合入"全链（含 m-of-n 多签如实报错）；策略文档合入 `org/service/policy_doc.rs:72`（OrgSigSet 五步链验证）；
- **凭证体系已实现**：`core/src/credential/`（schema、签名链验证、注销列表 append-only、`trust.rs` TrustDecl 结构）；TrustDecl 经 OrgSigSet 合入在 `org/service/verifiers.rs:111 adjudicate_incoming_trust_decl`；SDK 门面 C9 已落地（`sdk.credentials` 读持有 / holderProof，无签发接口）；
- affair 侧规则求值独立已落地：`affair/decide.rs`（vote / multisig / delayed-veto——**事务规则文档的求值在 affair 模块，不在 policy 引擎**，两体系分工见 §四）。

## 三、差距

| # | 差距 | 出处 |
| --- | --- | --- |
| — | G2/G3 已由 A15 落地核销（2026-09-08：`org:disclosure:` 声明记录 + 公示延迟（governance §4.1 适用范围已含）+ orgq 调用方过渡完成，见 membership 篇 §二 A15 行） | — |
| G1 | `evaluate_read` 引擎重排（拆 `eval_disclosure`/`eval_gate` 两函数 + 兼容壳）归 **A30**——A15 已落地求值语义切换（orgq 读取点 = 城门 + 开放声明求值，`eval_disclosure` 纯函数已建），但引擎结构拆分本篇不做 | membership 篇 §4.3（product/todo #14） |

## 四、目标设计

### 4.1 两套规则体系的分工（先划清，避免双引擎混淆）

| 体系 | 管什么 | 求值位置 | 现状 |
| --- | --- | --- | --- |
| **affair 规则文档** | 事务的关闭 / 采纳 / 核查（vote / multisig / delayed-veto） | `affair/decide.rs` | 已落地 |
| **policy 策略引擎** | 开放声明（名册开放档位 / 字段授权 / 向上开放矩阵）+ 城门规则 + 未来声明式扩展 | `policy/`（本篇） | 已接线（B1 旧语义），待按城门口径改造 |

结论：policy 引擎**不碰**事务决策求值（affair 已有且经向量锁定）；它的领地是"**可见性与开放**"这一族声明式规则。

### 4.2 求值器重排（G1）

`evaluate_read` 拆为两个确定性纯函数（同一引擎、两套输入）：

1. **`eval_disclosure(声明集, 目标域, 集合) → 可见视图`**：装配"某集合对某域可见什么"——输入 = 开放声明（`org:disclosure:` 记录，membership 篇 §4.3）+ 字段授权 + 向上开放矩阵；用在**写时**（发布物生成）与**查询装配时**（服务节点装配响应）；
2. **`eval_gate(凭证, 名册视图, 规则) → 放行 / 拒绝`**：城门验证——成员资格凭证验签 + 名册回查（`verify_read_auth` 裁剪后形态）+ 城门规则（如"租户凭证可读公共集合"这类域级规则）。

fail-closed、纯函数、不碰网络 / 存储 / 时钟（引擎纪律不变）；`analyze.rs` 静态分析复用于开放声明（保存时查冲突 + **暴露面扩大必须显式确认**——防借升级悄悄放宽，产品原话场景）。与插件钩子边界：membership §4.3 既定口径——`filtered` 插件钩子对插件自有集合（canRead/canWrite）继续有效，执行点从"数据账号侧"变为"任一服务节点侧"，policy 引擎不管插件自有集合。

### 4.3 接线点（G2）

1. **保存路径**：现状 `policy_publish` 已有"analyze → OrgSigSet 签署 → 存证锚 → `org:policydoc:` 合入"全链——**增量 = 开放声明记录化（`org:disclosure:`）+ 公示延迟插入**（A15 / governance §4.1 适用范围）：组织管理应用保存开放声明 → `analyze.rs` 静态分析（冲突阻断 + 暴露面扩大确认）→ OrgSigSet 签署 → 公示延迟 → 生效；
2. **服务路径**：成员查询 → 服务节点 `eval_gate`（城门）→ 放行后 `eval_disclosure` 装配视图 → 响应；现有 orgq credential 通路按 membership §五.2 过渡（readAuth 段线形不变、求值口径一次性切换、版本发布对齐）；
3. **发布路径**：下级组织发布名册开放物 → `eval_disclosure` 生成发布视图 → 签名锚定。

**规则代际化承接**：affair 侧规则代际已有 rulesChain 版本链；`org:disclosure:` 记录自带 `version` 字段（membership §4.3）——policy 引擎无需另建代际机制。

### 4.4 产物 B（凭证）的现状确认

凭证体系已完整（§二），本篇只锁定一条：凭证**签发流程永远不在内核**（plugin 人机流程 + 验证人签名），内核只验证——既有实现已符合，验收节回归即可。

## 五、迁移路径

1. `evaluate_read` 拆为 `eval_disclosure` / `eval_gate` 两函数，保留兼容壳转调——**有生产调用方（orgq 通路），切换必须走 membership §五.2 过渡**：readAuth 段线形不变、求值口径一次性切换（旧名册三档判定映射为"仅组织"默认档）、切换点以版本发布对齐；
2. 旧"读授权门禁"语义（B1 模型的名册三档 + 字段掩码读取点求值）在 wiki `policy.md` 与 community-affairs §7.1 标注改订为开放授权框架；
3. 策略文档 `engine` 字段保留（B2 Cedar 升级出口不变）。

## 六、验收

- **向量**：`eval_disclosure` 求值用例集（三档 × 字段授权 × 矩阵，确定性逐字节）；`eval_gate` 真值表（有效凭证 / 已注销 / 非成员 / 租户凭证）；静态分析（冲突检出、暴露面扩大阻断与确认）；
- **单测**：策略文档哈希自认证（篡改失配拒执行）；未知规则字段拒绝（不猜着执行）；
- **集成**：保存开放声明（静态分析 → 签署 → 公示延迟 → 生效）→ 成员查询（城门 → 装配）全链路；`sdk.credentials` 回归；
- **回归**：policy_doc 合入五步链、TrustDecl 合入、affair decide 三组（不受本篇影响证明）；**orgq 现有调用方过渡回归**（切换前旧语义服务 → 切换点版本对齐 → 切换后城门语义，readAuth 段线形不变证明）。

---

> 关联：architecture/community/membership.md §4.3（开放声明与城门的语义权威）、architecture/affairs/governance.md §4.1 / A24（策略修订的公示延迟联动）；`policy.md`（声明式规则规格权威）；product/todo #14（G1/G2）。

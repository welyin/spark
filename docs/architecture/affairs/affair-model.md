# 架构设计：事务容器（复制、生命周期与决议存留两层）

> 模板：产品目标 → 现状 → 差距 → 目标设计 → 迁移路径 → 验收。对应产品文档：[../../product/affairs/affair-model.md](../../product/affairs/affair-model.md)（定稿）。

## 一、产品目标（来自 product/affairs/affair-model.md）

1. 事务 = 只有发起人、没有归属的通用容器；关注即副本，无人持有即消亡，删除只有"不再有人提供"一种语义；
2. **例外层（决议存留两层，P#7/Q10）**：已产生组织级效力的事务，其决议**结论**（结果哈希 + 签名）锚定**该组织存证链**（组织数据，全员数据节点保存，不随本体消亡）；事务**本体**去留组织自选（关注即保留 + 可选保留策略），不设强制副本；
3. 两面分工：元数据面（标题/简介/标签，全网只索引这三样）+ 内容面（按需拉取）；
4. 生命周期：发起（写明关闭规则）→ 进行 → 达标自动决议 →〔执行型：待执行 → 回报 → 核查〕→ 关闭；中止须集体决策；规则修改走事务自身集体决策；主持人权限仅展示层；
5. 两条确定性硬规则：时间权威以存证链为准；法定人数取开始前名册快照；
6. 参与身份（上下文 / 公共 + 公开履历三原则）；事务引用四种（继承 / 申诉 / 父子 / 关联，申诉自指，父子不做状态耦合）；决议 ≠ 已执行。

## 二、现状（核实日期：2026-09-08）

**wiki**：`docs/protocol/affairs/affair*`（容器线形、规则文档与集体决策机制、决议产物、时间语义）；`docs/protocol/affairs/affair-metadata.md`；`wiki/architecture/community-affairs.md`。

**代码**：`core/src/affair/` 已按域成体系——`actor.rs`（操作与 org_sig 结构）、`decide.rs`（vote / multisig / delayed-veto 三形态求值）、`rules.rs`（规则校验）、`rulechain.rs`（规则修订链，`evaluate_rule_change` 两处生产调用）、`meta.rs`（元数据修订 delayed-veto 固定）、`ladder.rs`（参与阶梯推导）、`profile.rs`（公开履历推导）、`exec.rs`（执行核查三形态）、`snapshot.rs`（法定人数/名册快照）、`core/src/sync/affairsync/`（C4 元数据面 gossip + 白名单整批拒收 + 关注门槛）、`core/src/index/`（C10 indexer，健康信号确定性计算已实现）。

**锚定挂点现状**：`kernel/evidence_ops.rs` 的治理事件驱动锚定 API（`wiki/architecture/sync/evidence-anchoring-export.md` §1.2 触发①"事务容器关闭 / 法定人数快照时锚一次"）**存在但无 affair 侧生产调用方**——现状 affair 操作只写本机链（`affair_ops.rs:979`），锚由通用 doc 写入挂钩与 p2p 启动兜底刷新；关闭路径的治理侧触发是**新增接线**，不是复用。另注意：存证链是**节点本地链**（`doc:evidence:proof:`/`doc:evidence:head` 本地键），链条目不随同步流动，跨成员只流动锚记录（`org:evi:anchor:` 经 orgsync org:structure@v1）——"组织存证链全员保存"在现状架构中的承载 = 锚流动 + 条目确定性可复算（见 §4.1）。

## 三、差距

| # | 差距 | 出处 |
| --- | --- | --- |
| G1 | **决议结论存证条目缺失**：决议产物未作为显式条目写入存证链（现状关闭路径只经通用挂钩刷组织锚根，治理侧无 evi:resolution 条目与触发接线）——事务本体消亡后，链上找不到"为什么这么改"的结论记录 | product/todo #7 |
| G2 | 事务本体**可选保留策略**机制未定义（组织自选保留的组织级声明）——**A22 已落地**（2026-10-09，见 §4.2 落地口径） | product/todo #7 |
| G3 | 执行型核查已实现（exec.rs）但"核查方式由事务规则声明"的产品语义需与规则文档对齐核验 | 本篇验收节 |

## 四、目标设计

### 4.1 决议结论存证条目（G1，#7 核心）

- **新增存证条目类型** `evi:resolution`：**生效决议**（公示窗无阈值异议后生效判定通过——非关闭条件初通过的"待确认"态，与 governance 篇 §4.1 第 4 条口径一致）由**每个求值到该生效判定的节点**在本机存证链**确定性自写**（写入点 = `replay_resolution` 生效判定路径，与 effect.rs 治理钩子同点；条目不流动、锚流动——存证链本地性见 §二）：
  ```json
  { "kind": "evi:resolution", "affairId": "<64hex>", "subject": "<64hex 效力对象>",
    "conclusionHash": "<64hex>", "sigSet": { /* 决议产物的 OrgSigSet 原样内嵌 */ },
    "effectiveTs": "<生效判定所依据的存证锚时刻>" }
  ```
  - **写入拓扑（本篇裁定）**：不引入"组织级复制链"新概念——各节点对同一生效决议算出**逐字节相同**的条目内容（输入全确定性：决议记录 + 名册快照 + 锚时刻），各自入本机链、各自锚定，锚记录经 orgsync 全员流动。于是"组织存证链全员保存"的语义落点 = **锚全员流动 + 条目任何成员节点可确定性复算重写**，证明力等效且不改动存证与同步正交的既有结构（product 篇"组织数据全员保存"的表述以此口径承载，如实标注差异）；
  - **效力相关组织** = 事先声明"本域成员 / 某项规则由该事务决议决定"的组织（governance 效力来源事先声明的既有声明面反查）；无声明组织效力的事务（纯讨论）不写此条目；
  - `conclusionHash` = 决议记录（产物）的 canonical 哈希——本体消亡后，凭此哈希 + 签名可证明"该决议存在过"，持有决议原文时证内容绑定（与 membership §四"被裁剪内容哈希锚仍留在存证链"同族语义）；
- 锚定触发接线既有"治理事件驱动"API（`kernel/evidence_ops.rs`，新增 affair 侧调用）：生效判定路径加一步——先写 `evi:resolution` 条目 → 再触发锚（链头承诺自然覆盖新条目）。

### 4.2 事务本体可选保留策略（G2）

- 事务规则文档新增可选字段 `retention: { "policy": "followers" | "org-pinned", "orgs": ["org_<…>"] }`：
  - `followers`（默认）：现状语义（关注即保留）；
  - `org-pinned`：声明的组织的数据节点将该事务本体纳入长期副本（组织级自选动作——**是组织自己的选择，不是内核强制**，与 Q10 拍板一致）；
- **防摊派双条件（本篇裁定）**：事务声明只是意向，须配对**组织侧 pin 接受声明记录**（`org:pin:{orgId}:{affairId}`，组织级动作、走公示延迟，与 `org:disclosure` 同族）——复制组并集以"事务声明 ∧ 组织接受"双条件生效，防止垃圾事务向任意组织摊派长期副本；
- 内核只提供字段与复制组语义（双条件满足时 affairsync 复制组 ∪= 该组织数据节点），不评估"该不该保留"。

**A22 落地口径**（2026-10-09 实现）：

- **线形**：`retention` 经 §5.6 静态检查把关（fail-closed：`org-pinned` 必须点名非空去重组织列表（≤64），`followers` 不得携带 orgs；非法即拒，不静默回退缺省）；字段缺席/null = followers，旧事务零感知；修改走事务自身 ruleChange 机制（patch 顶层键覆盖后重过静态检查）。实现：`core/src/affair/rules.rs`（`RetentionPolicy` / `RetentionDecl` / `parse_retention`）。
- **pin 接受声明线形**：`org:pin:{orgId}:{affairId}` = `{ pinV: 1, orgId, affairId, version, updatedAt, effectiveAt, revoked?, sigSet }`——组织级动作（OrgSigSet 背书、subject 绑定 pinHash 防搬签），**发布即公示**（键域并入 orgsync `org:structure@v1` 内建集合全员流动），**生效由 `effectiveAt` 门控**：接受（首次 / 撤销后再接受）= 副本摊派面扩大 → `effectiveAt = updatedAt + 24h` 且发布须显式确认；撤销 = 收窄即时生效；version 单调 LWW。入站合入 `adjudicate_incoming_pin` 以 disclosure 同一五步链把关。实现：`core/src/affair/retention.rs`（纯逻辑）+ `core/src/org/service/pin.rs`（合入裁决）+ `Kernel::org_pin_publish`。
- **复制组并集求值**：`Kernel::affair_retention_status` 输出逐声明组织 pin 状态（none / pending / effective / revoked）与 `effectivePinOrgs` = 声明 ∧ 接受 ∧ 已生效（`effective_pin_orgs` 纯函数）——双条件满足时该组织数据节点（A14 全员数据节点：成员即数据节点）将本体纳入长期副本。
- **GC/清理面联动（不杀最后副本原则）**：`Kernel::affair_body_hold` 是清扫事务本体前的必查挂钩——followers 档只看关注副本（取关即可回收）；org-pinned 档 = 关注 ∨（双条件生效 ∧ 本机是任一生效 pin 组织的数据节点）；生效 pin 在手但组织记录缺失（无法证伪成员资格）按保守分支保留；判定报错（锁定/存储故障）时清理面须 fail-safe 跳过回收。

### 4.3 执行核查对齐（G3）

验收节核验 exec.rs 三形态（delayed-veto / 指定核查方签名 / 正式投票）与产品"核查方式由事务规则声明，默认公示 N 天无阈值异议即通过"逐条对齐，差异按产品为准修正。

## 五、迁移路径

1. `evi:resolution` 为新增条目类型（append-only 链自然兼容）；旧版本节点读到未知条目类型按"忽略未知键"既有行为跳过（先例：nodeInfo 端点化）；
2. `retention` 字段缺省 = followers（现状行为），旧事务无感知；
3. 无存量数据迁移；首个含 evi:resolution 的锚根与旧锚根计算方式一致（条目进链即入树）。

## 六、验收

- **向量**：`evi:resolution` 条目逐字节（含签名包内嵌）、conclusionHash 计算、retention 字段解析与缺省；
- **单测**：效力相关组织反查（声明面）、生效判定路径"先条目后锚"顺序、org-pinned 双条件复制组并集（声明 ∧ 接受）；
- **集成**：换届事务生效 → 各成员节点本机链确定性出现同一 evi:resolution → 锚流动 → 全员取关事务本体（消亡）→ 半年后凭条目 + conclusionHash 独立证明决议存在（持有决议原文时证内容绑定）；
- **回归**：affair 全部既有用例（decide 三形态 / rulechain / meta / ladder / profile / exec / snapshot / affairsync 白名单）、evidence-anchor 向量全绿；§4.3 对齐核验单（exec 三形态 vs 产品语义）。

---

> 关联：product/todo #7（本篇全部）；architecture/community/membership.md §4.1（组织存证链的全员数据节点保存）；architecture/affairs/governance.md（效力来源事先声明的声明面）；architecture/foundation/evidence.md（锚与导出——导出治理包时 evi:resolution 条目即"为什么这么改"的随包证据）。

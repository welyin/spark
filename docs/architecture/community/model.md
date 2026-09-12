# 架构设计：共同体模型（DAG、域类型与删除通路移除）

> 模板：产品目标 → 现状 → 差距 → 目标设计 → 迁移路径 → 验收。对应产品文档：[../../product/community/model.md](../../product/community/model.md)（定稿）。

## 一、产品目标（来自 product/community/model.md）

1. 三层结构与唯一成员规则：个人只在叶组织、组织才能加入共同体（kind 硬规则）；可多父、绝不成环（**含多跳间接环**，沿成员关系图传递检查）；
2. 加入不是吞并、层级不产生权力；以"升格"代替"分裂"（升格不分旧账）；
3. **退出留史、域不可解散只可退出**：全体退出 → 空域只读档案；**空域历史无人兜底**（有人持有期间留史，自愿归档，内核不设托管规则）；
4. orgId = 创世策略记录哈希（自认证）；成员身份每域派生、跨域不可关联；
5. 创建者默认值（首任管理员；成员数 >1 策略修改默认走延迟 + 否决——模板层默认，可改）；
6. **组织删除通路整体移除**（Q21 = A，product/todo #18）：所有域一律只可退出。

## 二、现状（核实日期：2026-09-08）

**wiki**：`wiki/protocol/community/org-genesis.md`（C1 已落地：domainType 全链路、GenesisPolicyRecord、`orgId = genesis_org_id()` 自认证、member-kind 矩阵、**成环检查口径 = 目标域可达祖先集（含自身）纯逻辑 DFS**）；`wiki/architecture/community-affairs.md` §4.2–4.4。

**代码**：

- 创世与域类型：`core/src/org/genesis.rs`（domain_type、组织根密钥对、orgAddress 互绑）、`org/service/create.rs`（:98 domain_type 读取）；
- 成员种类硬规则：`memberKindEnforce` 矩阵（community 拒 person / leaf 拒 org，向量已产出）；
- 成环检查：`genesis.rs` `membership_would_cycle`（纯逻辑，上级域闭包由调用方注入）+ `org/service/community.rs` `validate_org_join`（存储接线；未公开绑定不进图，如实不承诺）；
- 空域：`community.rs` 全体退出 → 空域只读档案（`unit_org/service/community.rs:384` 测试覆盖"退出留史；域不可解散"）；
- **删除通路（#18 对象）**：~~共同体域守卫已有；叶组织完整删除通路存在~~ **A13 已整体移除**（2026-09-08：前端/壳层/kernel/service 公开入口全拆，仅存 `create.rs::delete_organization_impl` 全域守卫 fail-closed；`leaveOrganization` 自退出承接，最后一名成员退出即成空域）。

## 三、差距

G1/G2 已由 A13 落地核销（2026-09-08：删除通路全链拆除 + `delete_organization_impl` 全域守卫 fail-closed 兜底 + `leaveOrganization` 自退出 UX 承接，「删除组织」入口不复存在）。遗留：leaf 域无显式空域判定（无成员即无 admin，写路径自然封死；显式提示登记 A48）。

其余产品目标的现状交代：

- **已实现**（本篇不重设计，仅在验收节锁定回归）：kind 矩阵、禁环含间接环、空域只读档案、orgId 自认证；组织作为成员一侧的域派生（community.rs kind=org 条目 rootId 槽位 = 派生共同体域身份 id）；
- **域派生（个人侧）未闭环**：org_user_id 名册切换承接于 membership 篇 §4.4（其 G5，product/todo #2），地基清单在 foundation/identity 篇 §4.4——本篇不误记为已实现；
- **原则性目标由对应篇承接**：目标 2（以升格代替分裂，升格不分旧账）是事务决议行为原则，落地在 affairs/affair-model 篇；目标 5（创建者默认值：成员数 >1 默认延迟 + 否决）落地在 affairs/governance 篇目标 2（模板层默认）。

## 四、目标设计（#18：删除通路整体移除）

### 4.1 守卫扩域（一行级改动）

`delete_organization_impl`（`org/service/create.rs:273`）的域删除守卫从"仅 community 域拒绝"扩为**全部域类型拒绝**：统一返回 `org/mod.rs:191` 同类硬错误（"域只可退出，不可解散；历史保留为只读档案"）。守卫留在 service 层最深处——即使上层有遗漏入口，底层依然封死（fail-closed）。

### 4.2 移除链（自上而下全拆）

| 层 | 移除对象 |
| --- | --- |
| 前端 | `OrgSettingsPanel.vue`「删除组织」按钮与确认弹窗、`api/command-map.ts` 的 `org-delete` 映射 |
| 壳层 | `commands/org.rs:354 org_delete` 命令 + `lib.rs:276` 注册 |
| kernel | `kernel/org_ops.rs:346 org_delete` |
| service | `delete_organization` / `delete_organization_pdsync` / `delete_organization_impl` 三个公开入口删除（守卫逻辑随函数一起退役——函数不存在即不可调用）；delete 事务类型与 pdsync 墓碑传播路径一并评估退役（若 tx 类型被历史记录引用，保留解析、禁止新写入） |
| 测试 | `unit_org/service/create.rs` 删除流测试改写为"全域类型拒绝删除"守卫测试；`kernel_org.rs:179-184` 删除用例删除 |

### 4.3 UX 承接：「退出组织」引导

组织管理应用在原「删除组织」位置替换为「退出组织」：说明退出的语义（历史留史、自己设备上的组织数据转为只读档案、全员退出后成空域），单管理员组织提示"你是最后一名成员，退出即成为空域"。无新机制（退出通路既有）。

## 五、迁移路径

1. 纯移除型变更，无数据迁移；存量组织数据不受影响（删除是本地发起、墓碑经 pdsync/dlog 传播的动作；存量墓碑按 §4.2 保留解析、禁止新写入）；
2. 旧版本前端仍可能展示「删除组织」按钮 → 调用壳层命令得到"未知命令"错误（自然失效）；新版本守卫在 service 层，双保险；
3. `org:tx` 审计日志中历史 delete 记录保留（只读历史，不清理）。

## 六、验收

- **单测**：`delete_organization*` 对 leaf / community 两域类型均拒绝（守卫扩域）；service 公开入口编译期消失（调用方零残留）；
- **向量**：不涉及线形变更，无新增向量；`community_orgmember_vectors`（cycleCheck / memberKindEnforce）回归；
- **集成**：前端组织设置页无删除入口；「退出组织」全流程（含最后一名成员退出 → 空域只读）；
- **回归**：创世（C1）、member-kind 矩阵、成环检查（含间接环用例）、空域只读档案既有测试全绿。

## 七、组织与共同体合并（2026-09-10 决策）

> 决策来源：2026-09-10 拍板（产品层决策记录由产品侧同步落于 product/community/）。决策原文：**组织与共同体合并为一个概念——组织可包含组织和个人，是否包含个人由组织自己决定**。§一–§六 为合并前历史设计记录（A13 等），其中"域不可解散、退出留史、成环检查、域身份派生"等结论在合并模型下**原样保留为不变量**。

### 7.1 决策要点与不变量

1. **组织＝唯一概念**；成员分两类（个人成员 / 组织成员）；组织可嵌套多父、禁成环（含多跳）等结构规则不变；
2. **计票权重＝纯凭证/规则问题**（资格插件表达），平台不预设一人/一户一票——内核只认成员资格与凭证，表决权重归 affairs 层规则与插件；
3. **副本责任下沉设备层**：K 副本按"全副本设备"计数，与设备归属（组织/个人）无关；成员资格是持有副本的前提（域密钥随资格下发）；副本不足提示按设备口径——设计见 [membership.md](membership.md) §七；
4. **加入门槛＝创世策略声明**（仅个人 / 仅组织 / 两者），内核只验证；默认模板维持现状（叶＝仅个人、共同体＝仅组织），存量零迁移；
5. **不变**：加入不吞并、退出留史 / 域不可解散、升格不分旧账、域身份派生、组织签名机制（signingPolicy / 策略修订链 / sigset 全部不动）。

### 7.2 现状（核实 2026-09-10）：合并的结构性机制已大体落地

合并模型的"骨架"已随 C1/A13/A16/A17 等批次落在代码里，差距集中在**「允许哪类成员」由 `domainType` 硬编码，而非创世策略声明**：

- 成员记录类型字段**已在**：`org/types/member.rs:10-18` `MemberKind::{Person, Org}` + `:380-384` `OrganizationMember.kind: Option<MemberKind>`——**wire 兼容已就绪**：serde 缺省丢键、键缺失 = person（存量成员记录原样可读）；`kind=org` 时 rootId 槽位承载该组织在本域的域身份 id（org-genesis §4 派生，`genesis.rs:289-323`），orgId 不进成员关系；
- 域类型**已在**：`org/types/record.rs:61-74` `DomainType::{Leaf, Community}`（键缺失 = leaf 兼容）+ `:147` `OrganizationRecord.domain_type`；创世承诺位 `genesis.rs:121` `GenesisPolicyRecord.domain_type`（创建时确定、不可变更，入 orgId 哈希承诺）；
- 成环检查**已在**：`genesis.rs:348-372` `membership_would_cycle`（多跳间接环、多父 DAG）+ `org/service/community.rs:108-126` `validate_org_join` 存储接线（图来源 = 各域公开名册 `kind=org` 成员的 opt-in `orgBinding.orgId`；未公开绑定不进图，如实不承诺）；合入侧执法 `community.rs:164-209` `enforce_incoming_roster`（违规条目剔除并 WARN，不整份拒收）；
- 不变量**已在**：删除通路全拆（A13，`create.rs:255-261` 全域守卫）、退出留史（`community_leave.rs` `org:cleave:` append-only）、空域只读档案（`community.rs:397-422`）、组织签名（`sigset.rs` + `org:policy:` 修订链）。

### 7.3 差距：叶/共同体二分假设点清单

「二分」的现状化身 = `enforce_member_kind` 硬矩阵（`genesis.rs:327-338`：community 只收组织、leaf 只收个人，**任何组织策略不得覆盖**）。合并决策把它从"内核硬规则"降级为"默认模板"，全部假设点（核实 2026-09-10）：

| # | 位置 | 假设 |
| --- | --- | --- |
| 1 | `core/src/org/genesis.rs:327-338` | `enforce_member_kind(domainType, kind)` 二选一硬矩阵——合并后改为按创世策略声明验证 |
| 2 | `core/src/org/service/members.rs:150-153` | `addMember` 强制录入个人（community 域拒绝） |
| 3 | `core/src/org/service/join.rs:199-201` | 免预录凭证入册（org-join-request）强制个人，注释"org-join-request 路径不录组织成员" |
| 4 | `core/src/org/service/community.rs:116` | `validate_org_join` 要求目标域为 community |
| 5 | `core/src/org/service/community.rs:183` | `enforce_incoming_roster`：非 community 域合入侧剔除 `kind=org` 条目 |
| 6 | `core/src/org/service/community.rs:233` | `create_community_org_invite` 仅 community 域 admin |
| 7 | `core/src/org/service/community.rs:458` | `leave_community` 退出留史语义只对 community 域成立 |
| 8 | `core/src/org/service/community.rs:397-404` | 空域只读档案判定仅 community（leaf 域靠"无成员即无 admin"自然封死，A48 已登记显式提示） |
| 9 | `core/src/kernel/community_ops.rs:59/157/231` | 组织加入/退出走独立的共同体编排入口（与个人邀请流 `kernel/org_join_ops.rs` 双轨平行） |
| 10 | `core/src/org/community_invite.rs:24-27` | 专用载荷 `community-org-invite` / `community-org-join-notice`（与个人邀请 `invite.rs` 双轨） |
| 11 | `core/src/org/mod.rs:164-214` | 错误族按二分命名（`MemberKindNotAllowed` / `NotCommunityMember` / `CommunityDomainArchived`） |
| 12 | `core/src/org/types/member.rs:13-17` | `MemberKind` 注释"只能加入叶组织 / 只能加入共同体域"（注释口径随实现改） |
| 13 | `app/src/components/org/CreateOrgDialog.vue:21-25` | 创建 UI 二选一 radio（"共同体域的成员是组织…创建后不可变更"） |
| 14 | `app/src-tauri/src/commands/dto.rs:167-177`、`app/src/api/types.ts:961` | `domainType` DTO/命令链 |
| 15 | `core/src/org/roles/mod.rs:35-36` | **同名异义警示**：此处 `leaf` 是"叶子设备"（mobile-leaf-mode 不履职），与"叶组织"无关——合并修订时勿混淆 |

### 7.4 目标设计

1. **成员记录类型字段**：`kind` 字段与 wire 兼容形态**维持现状即达标**（§7.2——键缺失 = person；`kind=org` 槽位语义不变）；合并不改线形、不动 golden 向量。
2. **创世策略新字段 `acceptedMemberTypes`**（A52 落线形）：`['person'] | ['org'] | ['person','org']`，创建时确定、不可变更，入 canonical 载荷（即入 orgId 哈希承诺，与 domainType 同待遇）。**缺省推导（存量零迁移的关键）**：键缺失时按 `domainType` 推断——`community → ['org']`，`leaf`/缺省 `→ ['person']`；旧创世记录无此键，推断结果逐字节等价于现行硬矩阵。
3. **加入/邀请协议验证点换源**：`enforce_member_kind(domainType, kind)` 的全部调用点（§7.3 清单 #1–#7）改为按**创世策略声明**验证——`accepted_member_types(创世记录)`（缺失则按 domainType 推断）∋ kind 才放行；拒绝错误沿用 `MemberKindNotAllowed`。个人邀请流（org-invite/org-join-request）与组织邀请流（community-org-invite）**双轨保留、验证源统一**——`['person','org']` 域两条流皆可用；`['org']` 域个人流在验证点被拒（同现状 community 域行为）。
4. **环检测不变**：`membership_would_cycle` 与图来源（公开名册 `orgBinding`）不动；混合成员域不改变图语义——仅 `kind=org` 边入图，个人成员天然不是图节点。
5. **与 org 签名/sigset 的关系**：signingPolicy、策略修订链、sigset 名册回查（admin 集合、m≤n≤快照内 admin 数）一律按"成员表"口径，**不区分成员类别**——组织成员入册后能否成为签名主体由 signingPolicy 与 affairs 层规则决定，内核无差异路径；域身份派生（org-genesis §4）与 orgUserId 双写过渡（membership §4.4）均不受影响。
6. **计票权重**：内核不预设一人/一户一票（现状即无——affair rules 表决规则是事务层声明，`rules.rs` 校验不含成员类别分支）；决策此条为**确认性约束**，架构层无新增机制，仅要求在 affairs/governance 篇后续修订中不回填平台级默认权重。

### 7.5 迁移路径

1. **存量零迁移**：旧创世记录无 `acceptedMemberTypes` 键 → 按 domainType 推断，验证结果与现行硬矩阵逐案例等价（member-kind 矩阵向量回归即证明）；
2. `enforce_member_kind` 矩阵实现保留为**默认模板的推导函数**（不再是不可覆盖的硬规则语义），仅显式声明非默认组合时走新字段；
3. **wire 兼容与混跑**：新字段 additive（serde 无 deny_unknown_fields 先例，旧端读新创世记录忽略该键、验签/orgId 复算不受影响——字段在记录内即入哈希）。但旧端按旧矩阵误判非默认组合域的加入——**声明 `['person','org']` 或 `leaf+['org']` 等非默认组合的域，须全端升级后启用**（版本门控，UI 侧提示）；
4. 分阶段：P1 验证换源（A50）→ P2 创世字段 + 模板 + UI（A52）→ P3 wiki 规格改写（A53）。

### 7.6 验收

- **向量**：`acceptedMemberTypes` 线形（缺省 / 仅个人 / 仅组织 / 两者 × domainType 推断矩阵；入 orgId 承诺的哈希用例）；
- **单测**：四组验证真值表（成员类别 × 声明组合）、混合成员域环检测（org 边成环剔除、person 边不入图）、旧创世记录推断等价性；
- **集成**：`['person','org']` 域双邀请流（个人邀请 + 共同体邀请）皆可达；非默认组合域对旧端的拒绝行为如实呈现；
- **回归**：`community_orgmember_vectors`（cycleCheck / memberKindEnforce）、A13 全域删除守卫、空域档案、A16 双键、A17 免预录全部既有用例。

---

> 关联：product/todo #18（§一–§六）；2026-09-10 合并决策（§七）；architecture/community/membership 篇（数据面 / org_user_id / 网关活跃集 / §七 副本设备口径）；architecture/affairs/governance 篇（公示延迟禁令——删除通路曾绕过的那条约束）。

# 架构工程任务排期（开发指导）

> 本表是开发任务的唯一排期来源。两个合法来源：
> ① [../product/todo.md](../product/todo.md)（需求权威，回链编号，如"P#20"）；
> ② 架构师差距分析（product 文档写了、现状未做、product/todo 未覆盖，回链 product 文档章节）。
> 每项注明架构文档与依赖；完成后移出本表并更新对应架构文档状态。

| # | 任务 | 来源 | 架构文档 | 依赖 | 批次 |
| --- | --- | --- | --- | --- | --- |
| A50 | 成员记录类型字段＋加入协议按创世策略验证成员类型：`kind` 线形已在（member.rs，键缺失=person 兼容），本项 = `enforce_member_kind` 由 domainType 硬矩阵换源为创世策略 acceptedMemberTypes（缺失按 domainType 推断），涉及 org/types/member.rs、kernel/org_join_ops、create 守卫及 community.rs 各验证点 | P#26 | [community/model.md](community/model.md) §7.3–7.4 | 无 | 三 |
| A51 | 副本健康度按全副本设备计数：全副本设备集合 = 成员资格 ∩ 设备全副本声明 ∩ 在线证据；health.rs/presence.rs 计数输入换集合，K 不足提示按设备口径；域密钥随资格下发机制重建（orgkey 族，C7 退役后） | P#27 | [community/membership.md](community/membership.md) §七 | A50 弱相关（成员资格判定复用名册口径） | 三 |
| A52 | 创世策略 `acceptedMemberTypes` 字段线形与默认模板（叶=仅个人、共同体=仅组织，缺失按 domainType 推断）＋创建/设置 UI（CreateOrgDialog 二选一改为成员类型组合，非默认组合版本门控提示） | P#28 | [community/model.md](community/model.md) §7.4 | A50 | 三 |
| A53 | wiki 协议规格全量改写：org-join / org-invite / org-record / org-orgsync / community 各篇按合并模型修订（二分假设清单见 model §7.3） | 架构师差距分析（community/model §7.2–7.3） | [community/model.md](community/model.md) §七 | A50–A52 设计冻结 | 三 |
| A49 | 副本健康度体验增强：presence 变化实时推送（现打开设置页才拉取）+ 配对/迁移流程 onboarding 副本数告知（A3 遗留） | 架构师差距分析（personal-data §4.5） | [foundation/personal-data.md](foundation/personal-data.md) §4.5 | 无（A3 已完成） | 三 |
| A46 | dlogAck 收敛：hello 未携带 dlogAck，未全员确认的墓碑每轮 hello Equal 分支重推一轮——评估收敛方案（hello 携带 dlogAck 水位或重推节流） | 架构师差距分析（A1 边界外发现，personal-data-sync dlog 节） | [foundation/personal-data.md](foundation/personal-data.md) §二 | 无 | 三 |
| A48 | leaf 空域显式判定与提示（A13 遗留：现靠「无成员即无 admin」自然封死，如需显式空域提示位） | 架构师差距分析（model §三） | [community/model.md](community/model.md) §4.3 | 无（A13 已完成） | 三 |
| A21 | `evi:resolution` 存证条目（生效判定路径各节点确定性自写：先写条目后锚）+ 效力相关组织反查 | P#7 | [affairs/affair-model.md](affairs/affair-model.md) §4.1 | 无（锚定 API 已有，affair 侧触发为新增接线） | 三 |
| A22 | 事务 `retention` 保留策略字段（followers / org-pinned，公示延迟约束） | P#7 | 同上 §4.2 | A21 | 三 |
| A23 | rules.rs 校验改写：删 m<2 拒绝 + delayMs>0 全机制强制；vote/multisig 补公示窗字段；存量规则 grandfathered（UI 提示补窗）；exec 核查三形态与规则声明对齐核验 | P#20 | [affairs/governance.md](affairs/governance.md) §4.1、[affairs/affair-model.md](affairs/affair-model.md) §4.3 | 无 | 三 |
| A24 | `org:policy:` 策略修订链生产方接线（ruleChange 联动 policyHash） | P#20 配套 | 同上 §4.2 / §五 | A23 | 三 |
| A25 | 门限签名选型评估（可识别门限 vs 多签聚合位图；先答"如何逐分量识别签名者"）→ sigSetV=2 设计 | P#21 | 同上 §4.3 | 无（signingPolicy 面，与 A23 弱相关） | 四（独立排期） |
| A26 | 策略配置 UI 中心化密钥保管提示（m=1 / 分片集中 ≤2 人检测） | P#19 | 同上 §4.4 | A23 | 三 |
| A27 | org-card 扩展字段线形（名片插件）+ 发布通路（spark-affair-meta 发布侧）+ indexer 前端设置界面 + 启用态持久化（intake 与 Tauri 命令已于 186f7a2 落地） | C11 剩余、零调用清单（09-08 部分已消） | [affairs/public-topics.md](affairs/public-topics.md) §4.1–4.2 | 无 | 三 |
| A28 | Git 工作流落地：只读镜像分发 + PR 子事务（bundle/patch blob）+ 维护者合并 + git CLI 工具链；wiki 旧传输层标作废 | P#24 | 同上 §4.3 | A1（blob 层） | 三 |
| A29 | indexer 索引侧过滤规则（运营者本地配置，不入协议） | P#22③ | 同上 §4.4 | A27 | 三 |
| A30 | policy 求值器重排：`eval_disclosure` / `eval_gate` + 三接线点（保存静态分析 / 服务装配 / 城门验证） | P#14 配套 | [plugins/interpretation.md](plugins/interpretation.md) §4.2–4.3 | A15 | 三 |
| A31 | 插件间契约层：manifest provides/consumes 解析 + RPC 中介路由 + 版本协商降级 | P#9 | [plugins/plugin-types.md](plugins/plugin-types.md) §4.1 | 无 | 三 |
| A32 | 契约发布物 schema（snapshotV 线形）+ 求值器三验证（签名/哈希/时点）+ 监督驾驶舱首场景 | P#23、#9 | 同上 §4.2–4.3 | A31（与 A23 弱相关：同触 rules 求值器） | 三 |
| A33 | `spark-plugin-cli` 双产物构建 + manifest kind/dependencies + SBOM + 依赖哈希锁定 | P#25 | [plugins/runtime-and-trust.md](plugins/runtime-and-trust.md) §4.1 | 无 | 三 |
| A34 | 市场界面迁为默认内置插件（前置：market 12 命令等语义移植为 `sdk.market` 桥模块 + 权限位）+ blob API 插件命名空间隔离 | P#17、product runtime-and-trust 能力最小化节 | 同上 §4.2–4.3 | A18、A1 | 三 |
| A35 | 经济内核 `core/src/economy/`：fixed + types → issuance（策略触发 + 分流 + 形式合规校验） | P#8 | [economy/credits.md](economy/credits.md) §4.1–4.2、§4.5 | A21（evi:resolution 授权锚）、A23（公示延迟校验） | 四（里程碑一前置） |
| A36 | redemption 兑付单状态机 + supply 供给声明 + reserve 不可分割储备 + 生命周期（红冲/时效/退社/排队） | P#8 | 同上 §4.3–4.4 | A35 | 四 |
| A37 | 恒等式独立复算工具 + 导出包积分流水核验扩展（evidence 篇导出包扩展随本项补设计） | P#8 | 同上 §六、[foundation/evidence.md](foundation/evidence.md) §六 | A36 | 四 |
| A38 | inter-org：agreements/ledger 线形 + 两阶段实报实销 + 平仓/解除 + 定点汇率 | P#8、Q13 后段 | [economy/inter-org.md](economy/inter-org.md) §4 | A36（里程碑二后启动） | 五 |
| A39 | 健康度指标 `health.rs` + 出清原语 `winddown.rs` + 枢纽/重叠度（在外/可兑比可随 A36 先行） | P#8、Q13 后段 | [economy/capital-defense.md](economy/capital-defense.md) §4 | A38 | 五 |
| A40 | 经营账插件（模板层候选：法币收支留痕、分账、分配决议走事务） | product/economy/fiat-compliance §四 | [economy/fiat-compliance.md](economy/fiat-compliance.md) §4.2 | A31（契约层） | 五 |
| A42 | 组织管理、文件两界面插件化（P#17 五界面中剩余两项；聊天/通讯录=A19、市场=A34 已登记） | P#17 | [community/communication.md](community/communication.md) §4.2 同模式 | A18 | 三 |
| A43 | 组织域内容处置操作（#22②：本域折叠/移除 + 公示延迟 + 存证留痕 + 申诉复核） | P#22② | [affairs/public-topics.md](affairs/public-topics.md) §4.5 | A23（公示延迟适用范围） | 三 |

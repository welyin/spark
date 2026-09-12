# 待实现清单：产品已明确、实现未做

> 工作记录（非正式分册）。只登记"**产品方向已拍板，但代码 / 工程尚未落地**"的事项；
> 还在争论的问题在 [open-questions.md](open-questions.md)。
> 按 docs 文档结构（foundation → community → affairs → economy → plugins）排序；
> 事项完成后移出本表，并在对应文档中更新实现状态。
> 文末两个附录：**已决与不要做**（拍板留痕，防重新提议被否决机制）、**影响面映射**（交架构师任务包索引）。

## foundation 底座

| # | 事项 | 产品定义出处 | 当前实现状态 | 登记 |
| --- | --- | --- | --- | --- |
| 1 | **个人域 K=3 目标制**：大块数据内容寻址分块、按需 P2P 回补、配额与清理（驱逐不杀最后副本）、副本健康度可见 | [personal-data.md](foundation/personal-data.md) 副本目标节；Q17（方向已决） | 现状为全量同步；配额用户可配规则产品化中 | 2026-09-06 |
| 2 | **org_user_id 改造**：组织内个人成员标识 rootId → 派生域身份（标识面 + 签名面 + 邀请定向分离，含存量迁移与 opt-in 个人身份） | [identity.md](foundation/identity.md) 三层身份承诺；[open-questions Q20](open-questions.md)（方向已决） | 未动；地基已有（`derive_domain_identity`、`OrganizationAccessKey` 惰性保留） | 2026-09-06 |
| 3 | **改密传播三件套**：乙（epoch 标记 + 手动重封 UX）+ V（`pwv:self` 单密码校验器，防多设备密码分叉）+ D′（ack 水位门控，不知新口令设备自动断粮；曾验证设备宽限 7 天、从未验证立即暂扣） | [identity.md](foundation/identity.md) 改密节；`wiki/architecture/identity/password-change-propagation.md`（2026-08-12 设计定稿、契约冻结） | E / F 系列派工推进中 | 2026-09-06 |
| 4 | **手机端每 7 天强制密码验证**（"密码考试"：日常生物识别照常，到期必输密码，防长期只用生物识别遗忘密码） | [identity.md](foundation/identity.md) 登录态节（2026-09-06 拍板，参照主流手机厂商设计） | 未做 | 2026-09-06 |
| 5 | **设备层数据隔离（OS 账号）**：桌面端数据目录放 OS 用户目录、借 OS 文件权限隔离（不硬绑单账号；本机静态层纵深防御，家庭小主机多成员各自配对的前提） | [open-questions Q18](open-questions.md)（方向已决 = B）；源自 topics.md 便签② | 未做（移动端 OS 沙箱天然已有，本条主要约束桌面端安装器） | 2026-09-06 |
| 6 | **导出包附成员名册存证锚快照**：外部核验方（街道办）可验证"签名者当时确为成员 / 有资格"，里程碑二前完成 | [open-questions Q05](open-questions.md)（已决前置）；[evidence.md](foundation/evidence.md) | 未做（导出包 + evidence-verify CLI 已有地基） | 2026-09-06 |

## community 共同体

| # | 事项 | 产品定义出处 | 当前实现状态 | 登记 |
| --- | --- | --- | --- | --- |
| 14 | **组织数据面去管理员化 + 名册开放授权框架**：全体成员 PC 自动承担数据节点（K=3 集体口径不变），数据账号与管理员角色解耦；名册三档 / 字段级改为"下级组织向上授权开放"，个人字段进名册须本人显式同意；读授权收敛为"成员 vs 非成员"城门 | [membership.md](community/membership.md) §二/§四（2026-09-07 拍板）；源自 topics.md 便签⑥ | 现状：数据账号缺省=全体管理员；读门禁组件（`verify_read_auth`/`evaluate_read`）已建成未接线——按新城门口径裁剪后接线 | 2026-09-07 |
| 15 | **移除"管理员指定网关"能力**：内核只保留默认策略（全员候选 + 活跃轮流），不提供指定 / 指派网关的入口 | [network.md](foundation/network.md) 网关节（2026-09-07 拍板，与数据节点去管理员化同一理由：通信面不长权力中心） | 现状：代码存在 gateways 指定 / 反查通路（`org/types/member.rs`），需移除 | 2026-09-07 |
| 16 | **组织副本健康度展示核对**：Q06「只告知」的展示面是否已实现（副本健康度如实可见、不达标如实告知）；核对后未做则补做 | [open-questions Q06](open-questions.md)（已决）；[membership.md](community/membership.md) §四"K=3 是目标而非保证" | **已核对落地（2026-09-08 A41）**：计算（replica.rs 组织级合计≥3+成员级明细）、UI 展示（副本 N/3 + 成员副本行 + kApplicable 分流）、只提醒不处置、叶子不计入四项全在；补做 kApplicable 前端分流（纯 all-members 组织不再误报副本不足） | 2026-09-07 |
| 17 | **内置界面插件化（聊天 / 通讯录 / 组织管理 / 文件 / 应用市场）**：五个默认内置界面全部抽为可整体替换的独立插件（数据全在内核：DM 底座与通讯录数据、组织数据、文件 blob、分发验签）；完成后壳层只剩空间切换器与设置骨架；群聊未来在聊天插件内实现（属插件自身演进，不进 Spark 里程碑） | [communication.md](community/communication.md)；[catalog.md](plugins/catalog.md)（2026-09-07 拍板） | 现状：IM / 通讯录 / 组织管理等 UI 内置于 `code/app/src`（非插件）；spark-moments 已有插件复用 DM 底座（feed 信封）先例 | 2026-09-07 |
| 18 | **移除组织删除通路（守卫扩展到全部域）**：叶组织当前可被单个 admin 直接删除，违反"域不可解散"；所有域一律只可退出、退出留史（全员退出 → 空域只读档案）。移除：前端「删除组织」按钮（`OrgSettingsPanel.vue`）、`org_delete` Tauri 命令与 kernel 通路；`delete_organization` 守卫从"仅共同体域"扩到全部域 | [model.md](community/model.md)"域不可解散只可退出"；[open-questions Q21](open-questions.md)（已决 = A） | 现状：共同体域守卫已有（`org/service/create.rs:282` + `org/mod.rs:191`）；叶组织删除通路完整存在（UI / 命令 / service / pdsync 墓碑） | 2026-09-07 |
| 26 | **成员记录加类型字段（个人 / 组织）+ 加入协议按创世策略声明验证成员类型**（内核改造，含 UI 成员列表类型标记、邀请流程分流）；架构任务 A50 | [model.md](community/model.md) 修订节（2026-09-10 组织 / 共同体合并决策） | 未做 | 2026-09-10 |
| 27 | **副本健康度改按全副本设备计数**：内核 replica / health 计算与 UI 提示口径迁移，组织成员代理口径退役；架构任务 A51 | [membership.md](community/membership.md) §四修订（2026-09-10 拍板） | 未做 | 2026-09-10 |
| 28 | **创世策略新增"接受的成员类型"声明字段与默认模板**（叶 = 仅个人、共同体 = 仅组织）、创建 / 设置 UI；架构任务 A52 | [model.md](community/model.md) 修订节（2026-09-10 组织 / 共同体合并决策） | 未做 | 2026-09-10 |

## affairs 事务

| # | 事项 | 产品定义出处 | 当前实现状态 | 登记 |
| --- | --- | --- | --- | --- |
| 7 | **决议事务存留两层结构**：决议结论锚定组织存证链（内核保证）+ 事务本体组织自选保留策略 | [affair-model.md](affairs/affair-model.md) §复制"例外层"（2026-09-07 正文落笔）；[Q10](open-questions.md) 已决（否决强制副本） | 未做 | 2026-09-06 |

| 19 | **中心化决策结构的密钥保管提醒**：组织配置中心化决策结构（如单管理员独断）时，向掌权者明确提示密钥劫持风险与保管纪律（生物识别 / 芯片隔离 / 助记词备份引导） | [governance.md](affairs/governance.md)"权力越集中，钥匙越要看好"（2026-09-07 拍板） | 未做 | 2026-09-07 |
| 20 | **治理模型重构：单点禁令 → 公示延迟禁令**：内核从"章程级必须集体决策"改为"公示 + 延迟生效 + 留痕（唯一硬约束）"，决策结构由组织策略声明、内核确定性求值；wiki 规格与代码同步重构（`wiki/architecture/community-affairs.md` §4.5"单点禁令/延迟否决解锁硬校验"、`org-signature`、`affair` 规格；代码 `org/sigset.rs` 策略求值与合入侧校验） | [governance.md](affairs/governance.md) 透明与延迟禁令节（2026-09-07 拍板 = A） | 未做；现状代码按单点禁令硬校验实现 | 2026-09-07 |
| 21 | **门限签名设计与实现**：可识别门限方案选型（**签名者可溯 = 硬前提**：谁参与了签名必须可查，不满足宁可不做）→ 设计一次到位 → 实现 + 向量测试。核心价值 = 组织对外单一稳定公钥（外部系统不懂 Spark 语义也能验签，对外交互场景）；策略语言需支持**分片分布声明**（哪些人持有分片、阈值多少）。签名形式集合 = anyAdmin / mOfN / threshold，组织策略自选 | [governance.md](affairs/governance.md) 组织签名节（2026-09-07 拍板：保留并做出来，不搞远期预留） | 未做；现状仅 `sigSetV` 版本字段预留 | 2026-09-07 |
| 22 | **自我审查机制落地**：① 个人内容过滤规则（关键词 / 来源屏蔽 / 分类）——客户端 / 插件实现，内核零改动；② 组织域内容处置操作（折叠 / 移除本域内容，公示 + 留痕 + 可申诉）；③ indexer 索引侧过滤 | [public-topics.md](affairs/public-topics.md) 自我审查节（2026-09-07 拍板：内核不做内容理解，判断归人 / 组织 / 插件） | 现状：拉黑 / 免打扰、主持人展示层折叠已有；其余未做 | 2026-09-07 |

## economy 经济

| # | 事项 | 产品定义出处 | 当前实现状态 | 登记 |
| --- | --- | --- | --- | --- |
| 8 | **经济内核**：积分发行 / 兑付销毁 / 发行恒等式 / 兑付单状态机（里程碑⑥前置） | [economy/](economy/) 五篇；[roadmap.md](roadmap.md) 阶段②剩余 + §三前置 | 代码为零 | 2026-09-06 |

## plugins 插件

| # | 事项 | 产品定义出处 | 当前实现状态 | 登记 |
| --- | --- | --- | --- | --- |
| 9 | **插件间契约**（业务插件只读接口 + 监督驾驶舱聚合） | [plugin-types.md](plugins/plugin-types.md)（自承设计草案） | 未实现，第一版单插件闭环 | 2026-09-06 |
| 23 | **治理策略引用插件契约发布物**：数据持有插件按契约发布可锚定数据快照（插件签名 + 存证锚 + 时点），策略文本声明引用为资格 / 权重输入（股权加权等）；求值器验证来源 / 完整性 / 时点（快照须早于决议开始），不理解数据语义。**不设"求值器直接读插件集合"的通道**（2026-09-07 拍板） | [governance.md](affairs/governance.md) 决策结构条；[plugin-types.md](plugins/plugin-types.md) 契约节 | 未做；依赖插件间契约（#9）与策略引擎接线（#20） | 2026-09-07 |
| 24 | **代码仓库工作流落地（取消独立 Git 协议层）**：项目仓库经内核文件同步分发为**只读镜像**；桌面随代码仓库应用安装本地 git CLI、移动端 isomorphic-git 只读；PR = 子事务 + bundle / patch 附件（blob）；维护者本地合并、串行约定；**wiki 侧 Git over P2P 独立协议层设计作废重订** | [public-topics.md](affairs/public-topics.md)（2026-09-07 拍板：写从协议问题变为制度问题） | 未做；现状 wiki 有 Git over P2P 传输层设计（过度设计，待 rework） | 2026-09-07 |
| 25 | **插件库包机制与构建工具链**：库包角色（纯代码、无数据域、不单独运行）；manifest 依赖声明以仓库地址 + commit/哈希为准（不以包注册中心名为准）；构建工具链双产物（安装包 = 全量依赖打进 + SBOM + 锚定签名；库包 = npm 包 / git 仓库引用，Spark 不自建包仓库）；安装时依赖树展示 | [plugin-types.md](plugins/plugin-types.md) 组合节（2026-09-07 拍板：组合 = 构建期代码复用，不是 iframe 拼装） | 未做 | 2026-09-07 |
| 10 | **里程碑业务插件**：自举 6 个 + 小区自治 10 个（MVP 铁三角 = 签名表决 + 公共账本 + 信息公示优先） | [plugins/catalog.md](plugins/catalog.md) | 全部"计划"状态 | 2026-09-06 |
| 11 | **手机屏幕手写签字**：投票 / 签名征集场景在手机屏上手写签名并随事务存证（MVP 签名表决插件的组成部分；业委会场景实名签字的配套） | Q05 拍板时用户提出（2026-09-06） | 未做 | 2026-09-06 |

## 文档治理（决定已做出、清理未执行）

| # | 事项 | 产品定义出处 | 当前实现状态 | 登记 |
| --- | --- | --- | --- | --- |
| 12 | **wiki 旧中继文档标作废**：`wiki/product/p2p-relay.md` 顶部加 superseded 指引（指向 relay-strategy.md）；`notify-relay.md` 仍有效勿误标 | Q03（已决）；[network.md](foundation/network.md) 采新口径 | 未做 | 2026-09-06 |
| 13 | **wiki 排期文档立场对齐**：`wiki/product/development_plan.md` 远期清单删除"信用分体系、随机陪审团仲裁机制"（或移入"明确不做"） | Q19（已决）；[roadmap.md](roadmap.md) 第六节 | 未做 | 2026-09-06 |

---

## 附一：已决与不要做（拍板留痕）

> open-questions Q01–Q21 全部拍板（2026-09-06 / 09-07），归档于此。新问题在 [open-questions.md](open-questions.md) 登记、拍板后移入本表。**本表同时是"不要做"清单**：架构师不得重新提议已被否决的机制。

| 编号 | 已决 = 结论 |
| --- | --- |
| Q01 | 身份可恢复 ≠ 数据可恢复：onboarding 如实告知 + 副本健康度可见（todo #1）；自动快照留评估（09-06） |
| Q02 | 个人设备一律全信任，**不做设备级分级**；要隔离就另开根身份（09-06） |
| Q03 | 中继采新口径（公网节点自然属性 / 内核共享池）；旧 wiki 文标 superseded（todo #12）（09-06） |
| Q04 | 连通性靠**组织自备一台公网可达节点**（部署前置）；内核不加机制、共享池纯自愿（09-07） |
| Q05 | 导出包附成员名册存证锚快照**前置**（todo #6）；手机手写签字（todo #11）（09-06） |
| Q06 | 组织 K=3 **只告知、不干涉**；"公共数据账号"不方案化（09-07 已废弃：全员 PC 数据节点）（09-06） |
| Q07 | 空域历史**不做兜底**：有人持有期间留史、自愿归档、内核零机制（09-07） |
| Q08 | **不做**验证人批量冻结：拉黑 / 撤销信任 + 申诉推翻 + 换插件（09-07） |
| Q09 | 业委会选举**记名 + 全员可审计 = 法规必须**；无记名保密投票排远期（09-06） |
| Q10 | **否决强制副本**：决议结论锚组织存证链 + 事务本体组织自选（todo #7）（09-07） |
| Q11 | 维持沉默通过制，**不做正向批准 / 送达门槛**：效力源于成员的承认与执行（09-07） |
| Q12 | 记账主体自选 + 加入明示知情同意（一般规则，**不搞家庭特殊化**）（09-07） |
| Q13 | 前两个里程碑只做**单组织积分闭环**；跨组织双边 / 代理链 / 出清归其后（09-07） |
| Q14 | 交付佐证 = 模板层可替换插件（catalog 已登记）；内核交付流程 = 兑付单状态机（09-07） |
| Q15 | 资本结构三层论：信息面可映射（插件）/ 结算面系统外（银行）/ 权力面 = 内核经济对象封死 + 插件数据组织自选；**对外不得宣称软件能防法币私分**（09-07） |
| Q16 | L0/L1/L2 = **安装通路信任级**（非开源度 / 敏感度）：L0 侧载 / L1 仓库锚定（主路径）/ L2 官方签名（增强、非可复现构建——勘误）；验证类必须 ≥ L1（L0 拒装）。遗留：wiki 产品侧可补三档定义表（可选） |
| Q17 | 个人域 **K=3 目标制**（目标兼上限、核心数据全量、大块分块回补、驱逐不杀最后副本）；**废弃跨身份密文托管**；个人域数据不出自有设备（09-06） |
| Q18 | 数据目录入 OS 用户目录借 OS 权限隔离（本机静态纵深防御），**不硬绑**"一台设备一个账号"（09-06 决 B，09-07 复审维持 B）（todo #5） |
| Q19 | **不做**信用分体系、**不做**自动仲裁 / 随机陪审团；wiki 排期文档对齐（todo #13）（09-06） |
| Q20 | org_user_id 改代码三联动（标识面 / 签名面 / 邀请定向分离，域串 `org-access:{orgId}` 不变）（todo #2）（09-06） |
| Q21 | 组织删除通路**整体移除**（守卫扩到全部域）：所有域只可退出、退出留史（todo #18）（09-07） |

---

## 附二：影响面映射（交架构师的任务包索引）

> 用法：本表 + [open-questions.md](open-questions.md)（已决 = **不要做**的清单，防止重新提议被否决机制）即完整设计任务包。docs 各篇为产品权威；wiki 规格与代码按本表定位。

| # | 影响面（wiki 规格 / 代码模块） |
| --- | --- |
| 1 | `wiki/architecture/sync/org-data-sync.md`；`core/src/sync/`、`kernel/`（pdsync 通路、配额与清理） |
| 2 | `wiki/protocol/community/org-genesis.md` §4、`docs/protocol/community/credential.md`；`core/src/org/types/member.rs`（rootId→org_user_id）、`org/service/*`、`kernel/contact_ops.rs`、`kernel/community_ops.rs`（邀请定向分离、复活 `OrganizationAccessKey`） |
| 3 | `wiki/architecture/identity/password-change-propagation.md`（契约已冻结）；kernel epoch 系列（E/F 派工中） |
| 4 | `wiki/ui/`（登录）；`code/app` 移动端登录 / 锁定流程 |
| 5 | 桌面安装器与数据目录布局（`code/app/src-tauri`）；移动端天然已有 |
| 6 | `core/src/affair/snapshot.rs`（`verify_ladder_roster` 已有、零调用）；导出包 + `evidence-verify` CLI |
| 7 | `docs/protocol/community/affair*`；`core/src/affair/`、组织存证链（`org/`） |
| 8 | 母稿 §9（技术落点）；新建 `wiki/protocol/economy/` 与 `core` 经济模块（代码为零） |
| 9 | `wiki/architecture/plugins/`（plugin-data-api、契约格式）；插件 SDK / bridge |
| 10 | `plugins/catalog.md` 各行；新插件仓库（spark-* 系列） |
| 11 | 签名表决插件 + 移动端手写输入 |
| 12 | `wiki/product/p2p-relay.md`（标 superseded，纯文档动作） |
| 13 | `wiki/product/development_plan.md`（纯文档动作） |
| 14 | `wiki/product/community-model.md`、`wiki/architecture/sync/org-data-sync.md`；`core/src/org/service/`（data_accounts 缺省→全员 PC）、`credential/read_gate.rs` + `policy/eval.rs`（裁剪为城门验证 + 开放声明求值）、org 同步 |
| 15 | `wiki/architecture/p2p/`（网关邮箱）；`core/src/org/types/member.rs`（gateways 指定 / 反查通路移除）、`kernel/community_ops.rs` |
| 16 | `core/src/org/`（副本统计 / 健康度是否已实现，先核对）；组织管理应用 UI |
| 17 | `code/app/src`（IM / 通讯录 / 组织管理 / 文件 / 市场五个界面）；`wiki/architecture/plugins/`（SDK / bridge）、`code/app/src-tauri`（插件宿主） |
| 18 | `OrgSettingsPanel.vue`、`commands/org.rs`（org_delete）、`kernel/org_ops.rs:346`、`org/service/create.rs`（守卫扩域）、`org/mod.rs:191`、pdsync 墓碑（`sync/versioned.rs`） |
| 19 | 组织管理应用·策略配置界面（`code/app/src`） |
| 20 | `wiki/architecture/community-affairs.md` §4.5、`wiki/protocol/community/org-signature.md`、`docs/protocol/community/affair*`；`core/src/org/sigset.rs`、`policy/`、合入侧校验（`org/service/*`） |
| 21 | `wiki/protocol/community/org-signature.md`（sigSetV 版本位）；可识别门限方案选型 + `org/sigset.rs` 扩展 + 向量测试 |
| 22 | 聊天应用（个人过滤规则）、`core/src/index/`（indexer 过滤）、内核域内容处置操作（新增，走公示延迟） |
| 23 | `core/src/policy/eval.rs`、`kernel/policy_ops.rs`、plugin-data-api（契约发布物格式）、`core/src/affair/`（决议开始时点快照锚） |
| 24 | wiki Git over P2P 传输层（**作废重订**）；`core/src/sync/`（blob 分发）、代码仓库应用插件、PR 子事务（`core/src/affair/`） |
| 25 | 插件 SDK / CLI、`docs/protocol/plugin-dist`（manifest 依赖声明与哈希锁定）、打包格式与 SBOM |

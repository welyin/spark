# 架构设计：成员、名册开放授权与全员数据节点

> 模板：产品目标 → 现状 → 差距 → 目标设计 → 迁移路径 → 验收。对应产品文档：[../../product/community/membership.md](../../product/community/membership.md)（定稿）。本篇是 community 层改动重心（product/todo #14、#2、#15）。

## 一、产品目标（来自 product/community/membership.md）

1. **数据归属不随加入关系上移**：组织数据只属于它的托管域；名册开放 = 下级组织**向上授权**（三档：仅组织 / 代表可见 / 名册公开 + 字段级授权，个人字段须本人显式同意），共同体可声明开放档位为加入条件但无权越取；
2. **全员 PC 都是数据节点**（K=3 集体口径，目标而非保证；数据服务 = 结构角色非权力角色，与管理员解耦）；读授权收敛为**成员城门**（成员资格凭证，离队即失效、零密钥轮换）；
3. **org_user_id**（Q20）：名册与组织内引用 = `sha256hex(derive_domain_identity(seed, "org-access:{orgId}").公钥)`；三联动——标识面 / 签名面 / 邀请定向分离；
4. 加入流程：邀请预录-认领；**免预录准入**（持有效凭证者自签即入册，零管理员在线）；
5. 验证即插件、验证人才是信任；验证人失信既往不咎，被污染表决走申诉推翻。

## 二、现状（核实日期：2026-09-08）

**wiki**：`wiki/architecture/sync/org-data-sync.md`（账号角色模型）；`org-genesis.md`、`credential`（规格目录）、`community-affairs.md` §7.1。

**代码与实施状态**：

- **O1 已落地（2026-08-10, `aa4a851`）**：角色缺省推导 + **显式覆盖**（`org::roles`）、**网关活跃集确定性轮换限流 3**、两级 K 记账概览、`org_set_data_accounts` / `org_set_member_role` 接口与 UI；
- 两层复制：org scope 复制组（hello/need/data + 删除日志，验签放宽为 from ∈ 复制组 ∩ 组织成员）+ 账号内 pdsync 复用（零新增机制）；K=3 口径 = 全体数据账号 PC 副本合计 ≥ 3 且每账号 ≥1 PC；不达标只提醒；
- **数据账号缺省 = 全体管理员**（权力耦合点）；显式指定通路存在（G2）；
- **读门禁与策略引擎已建成且已在生产通路接线（O3）**：`credential/read_gate.rs verify_read_auth`、`policy/eval.rs evaluate_read`——现有调用方为 orgq credential 类 readPolicy 查询门禁（`tests/kernel_orgsync_inbound/read_gate.rs` 覆盖）；
- **A15 开放授权框架已落地（2026-09-08）**：`org:disclosure:{orgId}:{targetDomain}` 声明记录（`policy/disclosure.rs`：档位 + 字段授权 + 开放集合 + version + effectiveAt + OrgSigSet）——发布面 `kernel/policy_ops.rs disclosure_publish`（暴露面扩大须显式确认 + 公示延迟 24h 生效、收窄即时），入站合入 `org/service/disclosure.rs adjudicate_incoming_disclosure`（五步链 + version LWW，orgsync-data 键分支已接线、org:structure 键域全员流动）；**城门** `verify_read_auth` = 纯凭证校验（验证链 + credType/subjectDomain 匹配 + **名册回查**当时确为成员——双键兼容 rootId/org_user_id，退队即拒零轮换，名册不可用 fail-closed）；orgq 求值口径已一次性切换（§五.2：readAuth 线形不变，policyRef 槽位 = 「受开放声明约束」开关，第 5 步 = 生效 disclosure 覆盖判定；`evaluate_read` B1 求值器保留、求值器重排归 A30）。向量：`policy.disclosure`（线形 + 扩大分析 + 求值）+ `readGate.verifyChain` 城门真值表回填；集成：三组织嵌套（disclosure 装配视图 + 退楼即拒）见 read_gate.rs `nested_orgs_building_disclosure_view_and_gate`；
- 名册以 rootId 为键：`org:member:{orgId}:{rootId}`；`prepare_accept_invite(code, &root_id)`；is_admin / data_accounts / gateways 反查、contact_ops 寻址全用 rootId；`derive_domain_identity` 就绪（identity 篇 §4.4）；
- **org_user_id 签名面已落地（A16 切片二，2026-09-08）**：`RosterMember` 加 `orgUserId` 可选字段（additive，memberSetHash 随包自洽）；sigset 第 4 步名册回查双键兼容（signer 按 rootId 或 orgUserId 命中 admin——双写期新旧签名均可验）；生产签名点 `policy_ops.rs` 切 `org-access` 域私钥（signer=org_user_id，org-signature §2.1 口径，线形零改动）；`verifiers.rs` 名册投影携带 org_user_id；
- **org_user_id 地基已落地（A16 切片一，2026-09-08）**：`org/access_key.rs`——`derive_access_key`（域公钥 + 根绑定签名 + rootPubkey 锚点）、`verify_access_key_binding`（sha256(rootPubkey)==名册键 + 验签双步）、`member_org_user_id`（= sha256hex(域公钥)）；成员三处自发布挂点（create_org / accept_invite 成功 / update_my_identity 惰性补齐，`publish_access_key` 写一次 + whole/per-member 双写）；合入侧验绑（snapshot 采纳点 + per-member 入站 + whole 入站 `strip_unverified_access_keys`）；`OrganizationMember::org_user_id()` 派生访问器（双写过渡映射 = accessKey 自带）。**注**：`OrganizationAccessKey` 加 `rootPubkey` 可选字段——设计原结构（域公钥+绑定签名）不含验绑所需的根公钥，根公钥是公开信息，随记录携带无泄露增量；
- **org_user_id 双写过渡消费面已切换（A16 切片三，2026-09-08）**：名册键双写过渡——**条目键维持 `org:member:{orgId}:{rootId}`**（混跑期旧端只认 rootId 键，换键即名册分裂；切换窗口与移除条件见 §五.3 与 `org_member_key` 注释），消费面一律双键解析：contact_ops 寻址切 `find_member_any_key`（org_user_id 可命中成员端点）、城门/sigset 名册回查（前批已双键，本批回归）；**公共面**：概览面（`MemberSyncOverview`/`MemberReplicaOverview` 与壳层 DTO/前端类型携带 `orgUserId`，A14 memberReplicas 随迁）、存证导出 roster 段（`identity` 保 rootId 作 ② 层签名回查零依赖锚点——exporter/anchor 均 root 身份签名，`orgUserId` additive 携带，credential::RosterMember 同款形态；导出证明去 rootId 属格式 v3，归切换窗口）；**邀请定向分离**：信封 rootId 寻址不变，合入后名册只经 org_user_id 解析（自发布挂点切片一已接线）；**存量迁移执行**：`migrate_access_key_backfill`（unlock 时幂等补齐本机在册未发布成员，whole + 条目双写）+ `roster_fully_mapped`（切换窗口条件①的可判定形式）；三组织嵌套集成扩展 org_user_id 键面（`nested_orgs_org_user_id_key_face`：名册 rootId 槽位不出示成员身份、org_user_id 命中放行、退楼即拒零轮换）；
- **A17 免预录凭证入册已落地（2026-09-08）**：准入策略声明 `org:accept:{orgId}`（`policy/accept.rs`：`acceptCredentials: [{credType, issuerTrust}]` + version + effectiveAt + OrgSigSet——issuerTrust = 签发者信任声明所在域，凭证 subjectDomain 必须等于它）——发布面 `kernel/policy_ops.rs accept_policy_publish`（扩大方向公示+延迟 24h、收窄即时、扩大须显式确认，governance §4.1 适用范围），入站合入 `org/service/accept_policy.rs adjudicate_incoming_accept_policy`（五步链 + version LWW，orgsync-data `org:accept:` 键分支 + builtin 键域已接线）；加入声明 `org-join-request`（`org/join_request.rs`，org-mail 明文载荷 append-only 新类型：申请人根签 + accessKey 自发布挂点复用 + 附凭证 + declaredAt ±10min）——合入侧双路径合一验证 `adjudicate_join_request`（预录条目存在 → 认领不消费凭证；无预录 → 准入策略生效门控 + 类型/信任域匹配 + credential §6 第 1–5 步验证链），受理即入册 `OrganizationService::accept_join_request`（原子段 whole + per-member 双写，免预录 addedBy=申请人自录、事务审计携带 credId，认领补 accessKey 写一次）；kernel op `org_send_join_request` / `org_accept_join_request`（处理节点须为成员，零管理员在线）。向量：`policy.acceptCredentials`（线形 + 扩大判定 + 生效门控）+ `joinRequest.merge`（认领/有效凭证受理，已注销/签发者不受信任/类型不匹配/策略缺失/未附凭证必败）；集成：`kernel_join_request.rs` 双路径合一 + 拒收 fail-closed + 非成员处理拒绝；**边界**：org-mail 收信的自动解箱-合入编排（fetch 即处理）不在本批——合入入口已就位（呈现层/后续自动处理皆调 `org_accept_join_request`），壳层命令与 UI 归应用批次；
- 邀请流：`kernel/community_ops.rs`（`community_create_invite` / `community_send_invite` / `community_accept_invite` / `community_list_members`，走 org-mail）；
- 凭证：`core/src/credential/`（schema、签名链、注销列表、TrustDecl 经 OrgSigSet 合入）。

## 三、差距

| # | 差距 | 出处 |
| --- | --- | --- |
| — | G1/G2 已由 A14 落地核销（2026-09-08：副本池 = 全体成员 `data_node_set`、K=3 集体口径 `MemberReplicaOverview`、指定通路全拆 + `data_accounts` 字段 A9 同款 serde 老化）；orgq 在线投递/成员缓存机制 A15 评估结论 = **保留不瘦身**（保守：非成员 credential/public 查询发起侧与已退出成员降级视图仍消费在途记录/应答关联（Z2/Z3 防伪造是安全语义），成员侧虽事实不可达但路径共用，拆除只省存储面几个键却增分支与安全面） | — |
| — | G3/G4 已由 A15 落地核销（2026-09-08：写时开放声明 + 读时城门 + orgq 调用方过渡，见 §二 A15 行；求值器重排 `eval_disclosure`/`eval_gate` 引擎拆分归 A30） | — |
| G5 | 名册 rootId → org_user_id 三联动 + 存量迁移（**双写过渡已落地**：切片一派生/验绑/自发布/合入验绑 + 切片二签名面域私钥与双键回查 + 切片三消费面双键/公共面随迁/存量补齐迁移/三组织键面验收（见 §二）；剩余：切换窗口观测（全员补齐 + 双键面各稳定一个版本周期）后执行名册键正式切换与 rootId 键移除，条件见 §五.3） | product/todo #2 |
| — | G6 已由 A17 落地核销（2026-09-08：准入策略声明 `org:accept:` + 加入声明 `org-join-request` 双路径合一验证 + 直接入册，见 §二 A17 行；剩余：org-mail 自动解箱-合入编排、壳层命令与 UI 归应用批次） | — |

**product 覆盖登记（本篇不展开设计，去向如实指向）**：

| # | product 要求 | 去向 |
| --- | --- | --- |
| — | 托管域 × 公开发布（每个集合托管于某域、选托管域 = 选谁出存储；公开发布开关与托管域正交；集合托管在上级域 / 写权限属下级的声明机制；现状雏形：`create.rs:172 is_public`、orgq readPolicy `public` 分流） | 声明机制与事务数据面一体，承接于 affairs/affair-model 篇与 public-topics 篇（公开发布）；org-data-sync 下一版补集合托管声明线形 |
| — | 历史保留由组织自治（公告 N 年裁剪、关键数据默认不可裁剪、系统如实反映磁盘压力） | 事务保留策略承接于 affairs/affair-model 篇 `retention`（A22）；组织域通用裁剪在 org-data-sync 下一版登记 |
| — | 「以家庭名义加入」引导（一人家庭即时成立 UX） | UX 引导属应用层，组织管理应用排期时登记（内核零工作，预录/认领机制已覆盖其数据面） |

## 四、目标设计

### 4.1 全员数据节点：角色退役（#14 核心）

- **数据账号角色退役**：不再存在"数据账号"这一指定 / 缺省角色——**副本池 = 全体成员账号**；org scope 复制组推导从"数据账号集合"改为"全体成员集合"（与 all-members 趋同：组织数据默认全员可持、全员服务）；
- **K=3 集体口径不变**：全体成员的 **PC 类设备**副本合计 ≥ 3 即达目标（手机叶子不计入）；**不达标只提醒不处置**；副本健康度复用 presence 账本模式（foundation/personal-data 篇 §4.2——`blob:presence` 推广为 `presence:{collection}:{account}`；复用账本**模式**、不共用键线形，组织域与个人域同一机制、两套口径）；
- **读服务**：任一在线成员 PC 均可响应读取——先过城门（§4.3），再按集合驻留策略服务；驻留模型沿用 org-data-sync §3 的 `accounts / devices` 声明轴（`data-accounts` 取值语义改为"按 K 目标分布"，`all-members` 语义不变）；
- **管理员任免不再牵动数据职责**（O1 的"晋升即承担副本"提示退役）——数据面与治理面彻底解耦。

### 4.2 角色显式指定通路移除（#15 同源）

- 移除：`org::roles` 显式配置、`org_set_data_accounts` / `org_set_member_role` 命令与 UI、`record.rs` 的 `gateways` 字段消费侧与 `roles/mod.rs` 反查（network 篇 §4.2 已列）；
- 网关活跃集沿用 O1 已落地的**确定性轮换限流 3**（全员候选）——G2 删除的只是"指定"能力，活跃集机制保留；
- 结构保证：指定入口**不存在**（非禁用）。

### 4.3 名册开放授权框架（G3 + G4）

- **写时开放声明**：新增声明式记录 `org:disclosure:{orgId}:{targetDomain}` = `{ 开放档位（仅组织 / 代表可见 / 名册公开）, 字段授权[], 版本, 签名 }`——下级组织对自己名册向上级域的**发布物**；属组织级动作。**本篇裁定**：开放面扩大方向受**公示延迟**约束（公示 + 延迟生效，收窄即时）——理由是防管理员瞬间开放名册造成不可逆暴露，与 governance 篇"开放面扩大必须公示"同原则；该约束属本篇新增的适用范围裁定，governance 篇回写"章程级动作"清单时纳入；
- **读时城门**：`verify_read_auth` 裁剪为纯凭证校验——成员资格凭证验签 + 名册回查（当时确为成员），通过即放行其所在域可见的数据；**零密钥轮换**（凭证注销即失效）；
- **`evaluate_read` 求值范围重定义**：从"读取点逐条门禁"改为"**开放声明求值**"（某集合对某域可见什么——写时/查询装配时求值，确定性、可静态查冲突与暴露面扩大）；`filtered` 插件钩子对**插件自有集合**继续有效（插件数据的 canRead/canWrite 钩子语义不动，执行点从"数据账号侧"变为"任一服务节点侧"）；
- 旧 B1 模型中的"名册三档 + 字段掩码读授权"语义由开放档位 + 字段授权吸收，wiki `community-affairs` §7.1 策略引擎行与 org-data-sync §5 相应段落标注改订。

### 4.4 org_user_id 三联动（#2）

1. **标识面**：加入流程不再向组织出示 rootId——名册键 `org:member:{orgId}:{org_user_id}`；复活 `OrganizationAccessKey`（成员自发布：域公钥 + 根密钥对 `"org-access:{orgId}:{publicKey}"` 绑定签名，合入侧验绑）；组织公共数据面只出现 org_user_id；
2. **签名面**：组织内操作（成员条目自写、事务签名、存证）改用域私钥签名、名册域公钥验证——关联性不从签名侧漏回（org-signature §2.1 签名者密钥口径本就是"该组织内的域身份私钥"，线形零改动，切换的是密钥来源）；
3. **邀请定向分离**：邀请信封仍定向到人（rootId 寻址不变），合入后名册只记 org_user_id；contact_ops 加好友 / 私聊维持"默认隔离 + opt-in `usePersonalIdentity`"；
4. **存量迁移**：双写过渡——一个版本周期内名册同时携带 rootId ↔ org_user_id 映射（仅组织内可见），全量切换后移除 rootId 键。

### 4.5 免预录凭证入册（G6）

- 策略声明 `acceptCredentials: [{ credType, issuerTrust }]`（组织级动作，公示延迟约束）；
- 加入流新增路径：申请人自签加入声明 + 附凭证 → 合入侧纯逻辑验证（凭证签名链 + 注销列表 + 类型匹配 + 签发者在信任声明内）→ 直接入册（写 org_user_id 条目）；
- 与预录-认领并存：预录条目存在时走认领，无预录但持有效凭证走免预录；两条路径合入同一验证函数。

## 五、迁移路径

1. **角色退役**：`org::roles` 与 `gateways` 字段读取忽略（serde 兼容），复制组推导切换为全员；存量"数据账号"设备上的数据自然成为全员副本池的一部分（无数据移动）；
2. **开放授权**：存量组织默认档位 = 仅组织（最保守），首个开放声明须经公示延迟；旧读授权语义随 `evaluate_read` 重定义切换——**现有调用方过渡**：orgq credential 类集合读门禁的 readAuth 段线形不变、求值口径一次性切换为城门语义（旧"名册三档 + 字段掩码"判定在新口径下映射为"仅组织"默认档）；B1 向上开放矩阵求值随 `evaluate_read` 重定义同步切换，切换前由 orgq 通路继续按旧语义服务、切换点以版本发布对齐（filtered 插件钩子不受影响）；
3. **org_user_id**：双写过渡（§4.4-4）已落地——映射 = accessKey 自带（rootPubkey 锚点，仅组织内可见），消费面双键解析，存量补齐迁移 `migrate_access_key_backfill` 随 unlock 幂等执行；org-signature 向量组补充 org_user_id 签名者用例（切片二已落地）。**切换窗口与移除 rootId 键的条件**（全部满足才执行，本批只立条件不动键）：① 各域名册 `roster_fully_mapped` 恒真（补齐迁移上线稳定运行一个版本周期）；② 双键消费面稳定一个版本周期（不再出现只认 rootId 的在役端）；③ 切换动作 = 条目键换 `org:member:{orgId}:{org_user_id}` + 名册移除 rootId 槽位（协议线形变更，先改 `code/spec/` 规格与 golden vectors 再动实现；存证导出 roster 的签名回查锚点同步迁格式 v3）；
4. 分阶段：P1 角色退役 + 全员数据节点 + #15 通路移除；P2 开放授权 + 城门接线；P3 org_user_id 双写 → 切换；P4 免预录。

## 六、验收

- **向量**：开放声明线形（disclosure 记录、暴露面扩大静态分析用例）、城门验证（凭证有效 / 注销 / 非成员真值表）、org_user_id 派生与 OrganizationAccessKey 验绑、签名面域私钥用例（orgSigSet 组扩展）、免预录合入（有效凭证 / 已注销 / 签发者不受信任各必败）；
- **单测**：复制组全员推导、K=3 两级呈现（成员 PC 合计）、`org::roles` / `gateways` 忽略读取、双写迁移；
- **集成**：三组织嵌套——楼栋名册不上移、楼栋授权"代表可见"后小区层可见代表、成员退楼后小区层城门立即拒读（零轮换）；邀请加入与免预录加入双路径；
- **回归**：O1 网关活跃集轮换、两层复制、K 记账、凭证 TrustDecl 合入、org-signature 全部向量。

## 七、副本责任下沉设备层（2026-09-10 合并决策）

> 决策来源：2026-09-10 拍板（组织与共同体合并，设计总纲见 [model.md](model.md) §七）第 3 条：**K 副本按"全副本设备"计数，与设备归属（组织/个人）无关；成员资格是持有副本的前提（域密钥随资格下发）；副本不足提示按设备口径**。

### 7.1 现状（核实 2026-09-10）

- **个人域 blob 健康度已是设备口径，但设备集 = 本域设备清单**：`sync/blob/health.rs:58-91` `blob_health`——`device_count` = 域内未撤销设备数（`quota.rs:70-80` `active_device_count`，设备清单 `device:{peerId}`，`device/mod.rs` DeviceRecord），`K = min(3, deviceCount)`（`quota.rs:64-66`，≤3 台退化全量）；完整副本数 = presence 账本 `blob:presence:{cid}:{deviceUid}` 按 deviceUid 去重、位图全块判定（`presence.rs:140-173` `list_presence` / `full_replica_count`）；
- **组织域 K 是账号中心口径，非设备口径**：`org/replica.rs`——计数单元 = 去重（成员 × PC 设备）**履职对**（hello 入站 roles 含 `data` 且 `deviceClass=pc`，30 天窗口 `ORG_REPLICA_FRESH_WINDOW_MS`），组织级合计 ≥ 3（`replica.rs:95-100` `is_replica_sufficient`、`:349-364` duty_pairs）；逐成员第二级 `member_replicas_sufficient`（`:130-136`）；K 适用性挂在 data-accounts 集合声明上（`k_applicable`，`:71`）；
- **复制组验签是"成员资格前提"的唯一现存形式**：orgsync 验签 = from ∈ org:meta 成员表 ∩ 集合复制组（`sync/orgsync.rs:10-11`）——挡的是"写入扩散"，不是"持有副本"；
- **域密钥下发机制缺位**：orgkey 密钥表 / orgkey-deliver 信封已随 encrypted 轴退役（`sync/pdsync.rs:108` C7；`sync/orgsync/access.rs:4` 注释）——"域密钥随资格下发"目前**无任何机制**，需重建（原语族与 `org/mailbox.rs` §20.6 epoch 包裹同构，可复用）。

### 7.2 差距

| # | 差距 | 出处 |
| --- | --- | --- |
| G7 | 组织域 K 按"成员×PC 履职对"计数（账号中心），未按"全副本设备"计数；设备归属（组织所有/个人所有）在计数中不可表达 | 2026-09-10 决策 3 |
| G8 | 个人域（presence 账本设备计数）与组织域（hello 履职观测账号计数）两套副本口径并存，决策要求统一为"全副本设备"集合 | 同上 |
| G9 | "成员资格是持有副本的前提"只有复制组验签弱形式；域密钥随资格下发/随出册停发无机制（C7 退役后未重建） | 同上 |
| G10 | K 不足提示措辞两套（个人域"你当前只有 N 份副本"按设备；组织域 memberReplicas 按成员），决策要求一律按设备口径 | 同上 |

### 7.3 目标设计

1. **全副本设备集合**（统一计数输入）：设备 d 计入域 D 的副本池 ⟺ 三条件交集——
   - ① **成员资格**：d 的属主账号当时是 D 成员（名册回查，双键兼容 rootId/org_user_id——与城门同口径；资格吊销/退出即出池，零密钥轮换）；
   - ② **全副本声明**：d 声明持有 D 数据的全副本（blob 数据 = presence 位图全块，既有 `full_replica_count` 判定；结构化核心数据全量，device 在域内即默认持有——判定输入复用 hello 履职/sync-state 证据）；
   - ③ **在线证据**：d 在新鲜窗口（30 天，沿用 `ORG_REPLICA_FRESH_WINDOW_MS`）内有同步/履职观测；
   - **设备归属（组织所有 / 个人所有）不进计数**——归属只是资产管理层事实，不是副本责任参数。
2. **K 计数与提示口径**：`K = min(3, 全副本设备数)`（≤3 退化全量不变）；副本不足提示一律按设备口径（"全副本设备 N 台"），不再出现"成员副本"措辞；不达标只提醒不处置（红线不变）。
3. **与个人域 K=3 目标制的统一叙述**（foundation/personal-data §一 目标 2 / §4.5）：个人域 = "成员资格即本人设备配对关系"的退化形态——同一函数、同一 presence 账本，两域只差成员资格来源（个人域 = 设备清单未撤销，组织域 = 名册成员 ∩ 设备绑定）。`health.rs` 的 `active_device_count` 输入从"未撤销设备清单"换为"全副本设备集合"，`K=min(3,N)` 公式与退化语义不变。
4. **域密钥随资格下发**：成员资格是持有副本的**可读性**前提——域内数据密钥（重建 orgkey/epoch 包裹族，原语与 mailbox §20.6 同构）随入册下发、随出册停发（既往副本不可读性靠密钥不再下发新 epoch，与"已发出的副本收不回"口径相容）；**计数本身不依赖密钥**——presence 是公开账本，密钥只保证非成员持有即不可读。此条为机制重建项，归 A51 边界内的设计声明，落地排期独立评估（encrypted 轴退役背景见 pdsync.rs C7）。
5. **驱逐/回补/配额不变**（A1/A2 既有）：仅计数输入换集合，驱逐选择器"不杀最后副本"按新集合口径判定。

### 7.4 迁移路径

1. presence 账本线形不变（键形 `blob:presence:{cid}:{deviceUid}` 已含设备维度）；组织域 hello 履职观测**保留为在线证据来源之一**，"成员×PC 履职对"口径退役为展示层历史字段（一个版本周期后移除）；
2. 存量无数据迁移：旧账本记录直接是新集合的输入；健康度 UI 措辞切换随版本发布；
3. 混跑期：旧端按旧口径显示——只提醒不处置，无安全面；新旧端对同一账本算出的 N 可能不同，各自如实呈现（与健康度"确定性复算"约束不冲突：同一版本内同账本同结果）。

### 7.5 验收

- **单测**：全副本设备集合三条件交集真值表（资格吊销出池 / 部分副本不计 / 离线超窗不计；deviceUid 去重防虚增沿用 presence.rs 既有规则）；K=min(3,N) 在 N=1/2/3/5 的退化语义；
- **集成**：三设备两账号——同一物理设备分别服务组织域与个人域时**按域各计一次**；设备撤销/成员退出后两域 K 同步降；组织设备（归组织所有的设备）与个人设备同权计数；
- **回归**：A1/A3 全部向量、`k_semantics_at_domain_level` 改造后、驱逐选择器（副本 =K 不驱逐 / >K 按龄驱逐）既有用例。

---

> 关联：product/todo #14（§4.1–4.3）、#15（§4.2）、#2（§4.4）、P#27（§七）；2026-09-10 合并决策（model 篇 §七）；architecture/foundation/identity.md §4.4（派生地基）、foundation/network.md §4.2（网关活跃集）、foundation/personal-data.md §4.2/§4.5（presence 账本模式与个人域 K=3 目标制）；`org-data-sync.md`（账号角色模型，本篇为其下一版）。

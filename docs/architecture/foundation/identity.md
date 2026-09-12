# 架构设计：身份（RootID、设备信任、改密与域派生）

> 模板：产品目标 → 现状 → 差距 → 目标设计 → 迁移路径 → 验收。对应产品文档：[../../product/foundation/identity.md](../../product/foundation/identity.md)（定稿）。

## 一、产品目标（来自 product/foundation/identity.md）

1. 三级派生链：RootID（助记词自证）→ 设备（配对授权）→ 域身份（每域派生、跨域不可关联）；org_user_id = `sha256hex(derive_domain_identity(seed, "org-access:{orgId}").公钥)`（Q20，地基在本篇，名册切换在 architecture/community 篇）；
2. 设备信任：配对即全量信任；撤销 + 密钥轮换事后止损；丢钥 / 忘口令靠延迟恢复与备份二维码；
3. **改密传播三件套（乙 + V + D′）**：乙（改密广播标记，各设备手动重封）+ V（`pwv:self` 单密码校验器，防多设备密码分叉）+ D′（ack 水位门控，不知新口令设备自动断粮；曾验证设备宽限 7 天、从未验证立即暂扣）；改密是单机动作，踢设备只能走撤销；
4. **手机端每 7 天强制密码验证**（"密码考试"，product/todo #4）；
5. **设备层数据隔离（OS 账号）**：桌面数据目录入 OS 用户目录、借 OS 权限隔离，不硬绑单账号（Q18 = B，product/todo #5）。

## 二、现状（核实日期：2026-09-08）

**wiki**：`wiki/architecture/identity/password-change-propagation.md`（2026-08-12 **设计定稿、契约冻结**，§10–§13 乙+V+D′ 完整规格）；`device-trust-and-biometric.md`、`device-revocation-and-recovery-contracts.md`、`m3-epoch-rotation-plan.md`、`organization.md`。

**代码**：

- **改密三件套已接线（A5，2026-09-08）**：`core/src/pw/mod.rs`（pwv/pwack 线形、Kverify/Kack、可信锚门控谓词）+ `kernel/pw_ops.rs`（验票/重封/状态查询/unlock 自动 ack/heal）+ `epoch/mod.rs`（rotate 与补发逐设备门控）+ pdsync 入站（pwv 专用分支、pwack 批尾锚定钩子）+ QR 载荷携带 pwv 与注入守卫（§13.8/13.9）。关键接线裁定：**口令驱动轮换先发布新 V 再以同一时戳 rotate**（门控以「最新 V」为水位且满足 §13.5 时戳同源）；**unify 的内核复验与 ack 均针对 newPassword**（m45 §13.1）；**`inject_pwv` 的 pmeta ts 落 `changedAt`**（否则 QR 早于改密时恢复设备 LWW 学不到新 V）；
- **epoch 轮换已落地**：`core/src/epoch/mod.rs`、`kernel/epoch_ops.rs`（M3 身份级 epoch，`p2p:epoch:key:` 表）；
- **域派生地基已在**：`core/src/identity/derive.rs:75` `derive_domain_identity`（个人 seed SLIP-0010 派生，`org-access:{orgId}` 域串不变）；`org/types/member.rs:296-314` `OrganizationAccessKey` 绑定结构（域公钥 + 根密钥绑定签名，C7 后惰性保留）；`usePersonalIdentity` 骨架（opt-in）。注意区分：`org/genesis.rs:289-303` `OrgDomainIdentity::derive` 是**组织根私钥**经 HMAC-SHA512 派生**组织域**身份（`community:{id}` 等），密钥来源与算法均不同，不是本条资产；
- **身份文件**：v2 加密格式，个人资料已拆出为 `profile:self` 明文（可锁定 diff 同步）；
- 壳层三命令（`root_verify_password_ticket` / `root_unify_password` / `root_password_unify_status`）与三事件（PasswordChangeObserved / PasswordUnificationDone / DeviceOutOfGrace）已落地（`commands/pw.rs`，三态错误码透传，216 项壳层测试含 DTO camelCase 线形断言）；前端 F 系列由 ui-dev 推进；
- **密码考试已落地（A6，2026-09-08）**：`p2p:pw:lastPasswordAuth` 只认真实密码输入（`pw/mod.rs` 存储读写 + `Kernel::unlock_bio_sourced` 生物识别变体不刷新 + init/unlock/recover/change/reset/unify 全路径刷新）；`root_password_exam_status` 锁定态可调（Kernel::init 已按活动身份预开存储）；老账号首查懒初始化以启用时刻为初始值；移动端 LoginPage 超 7 天挂起生物识别（查询失败 fail-open，桌面不强制）；
- **OS 数据目录已落地（A7，2026-09-08）**：`layout.rs`——桌面数据根 = OS 用户目录（%LOCALAPPDATA%/Library·Application·Support/~/.local/share 下 `spark`），旧 app_data_dir 存量一次性迁移（移动 + `.spark-layout-v2` 标记 + 原位置留 `MIGRATED.txt` 说明；失败回退旧布局 + 事件提示）；不限制实例数；隔离强度 = OS 文件权限（不是加密替代品）；移动端沙箱零工作；
- **A45 已落地（2026-09-08）**：profile-sync 直发通道纳管 D′——`apply_self_profile` 写入前过 `should_gate`（与 epoch 同谓词同口径），暂扣快照挂起 `p2p:pw:pendingProfileSync`，ack 补齐后 `maybe_apply_pending_profile` 补应用（与 epoch 补发同型）；`PasswordChangeObserved` 改为仅接收端 pwv 入站广播（改密端不自报，本机回环判回放天然不到事件分支；reason 以同 ts epoch:state 为权威，缺省 password_change）；配套修复：懒发布/init 路径 ack 跳过时补齐本机自锚（否则本机门控把已验证设备误判为从未验证）；
- **口令保鲜（RootGate，app 层既有行为）**：指纹来源登录 bioSourced=true 时跳过 storePassword 重刷，手动密码才保鲜——§4.2 密码考试"bioSourced 不刷新时间戳"与之同源。

**未做**：org_user_id 名册键正式切换与 rootId 键移除（#2，属 community 篇——A16 三切片已落地双写过渡：派生/验绑/自发布、签名面域私钥、消费面双键解析与存量补齐迁移；切换窗口条件见 architecture/community/membership.md §五.3）。

## 三、差距

| # | 差距 | 出处 |
| --- | --- | --- |
| G4 | org_user_id 上层切换（名册 / 签名面 / 邀请定向） | product/todo #2（community 篇，本篇只登记地基） |

G1/G2/G3 已分别由 A5/A6/A7 落地核销（2026-09-08）；G1 遗留的 profile-sync 纳管已裁定并登记 A45。

## 四、目标设计

### 4.1 改密三件套：接线清单（设计已冻结，不再重设计）

以 `password-change-propagation.md` §10–§13 为唯一权威，E1 纯逻辑（`pw/mod.rs`）已覆盖 V 校验与 ack 门控谓词。剩余接线按四件落地：

1. **乙（广播标记 + 重封 UX）**：改密设备写 `epoch:state`（reason=password_change，已支持 reason 枚举含 `#[serde(other)] Unknown` 兜底）；他端感知后进入"密码已在他端变更"引导态——输入一次新密码完成三件事：V 校验（`verify_value`）→ 本机身份文件重封 → 写 `pwack:{peer}`；
2. **D′ 门控接线**：sync 消费侧在发放新 epoch 数据密钥前检查 `ack_covers(pwv, ack)`；**曾验证设备**（`lastVerifiedVTs:{peer} > 0`）宽限 `graceMs`（默认 7 天），**从未验证设备（含新配对 / 重配对）立即暂扣、不吃 grace**；heal 24h 与 graceMs 解耦（冻结口径）；
3. **壳层三命令**：`root_verify_password_ticket`（三态错误码 ticket-mismatch / invalid-password / ticket-unavailable）、`root_unify_password`（内核复验防绕过）、`root_password_unify_status`（锁定态可调）；三事件 PasswordChangeObserved / PasswordUnificationDone / DeviceOutOfGrace；
4. **QR 载荷携带 pwv**（§13.8 裁定）+ QR 恢复 gated 行为（同口令自动重获数据密钥；口令分叉时敏感面停更，输新口令后收敛）——按 §13.7/13.8 既有行为说明执行。

### 4.2 密码考试（#4）

- **状态**：`p2p:pw:lastPasswordAuth`（本机最近一次**真实密码**验证时间戳，只认密码输入——bioSourced 生物识别解锁**不刷新**此戳，与 RootGate 口令保鲜纪律同源）；
- **强制点**：移动端解锁路径检查 `now - lastPasswordAuth > 7 天` → 生物识别通道挂起，必须输入密码；密码验证通过即刷新时间戳并恢复生物识别；
- **失败出口**：连续失败不锁死（防 DoS 自己），引导"忘记密码"→ 助记词 / QR 恢复（既有延迟恢复通道，通知 + 延迟 + 否决不变）；
- **桌面端**：不强制（PC 长期登录态是设计预期，密码考试定位是防"长期只用生物识别遗忘密码"，主战场在手机）；

### 4.3 OS 账号数据隔离（#5）

- **目录布局**：桌面端数据根目录 = OS 用户目录下（Windows `%LOCALAPPDATA%\spark\`，macOS `~/Library/Application Support/spark`，Linux `~/.local/share/spark`），一个 OS 账号即一个逻辑设备（各自实例、各自数据目录）；
- **不硬绑**：不限制同一 OS 账号下的实例数（Q18=B，要隔离就另开根身份）；隔离强度 = OS 文件权限（本机静态纵深防御，**不是加密替代品**——文档与 UI 提示必须如实标注）；
- **移动端**：OS 应用沙箱天然满足，本条零工作；
- **安装器**：默认布局如上；存量数据目录提供一次性迁移（移动 + 原位置留迁移说明），多实例场景（Q18 选项 C 双安装模式）留安装器阶段增强。

### 4.4 org_user_id 地基清单（供 architecture/community 篇直接引用）

| 资产 | 位置 | 状态 |
| --- | --- | --- |
| 域身份派生（个人 seed，`org-access:{orgId}` 域串不变） | `core/src/identity/derive.rs:75` `derive_domain_identity` | 已就绪 |
| `OrganizationAccessKey` 绑定结构（域公钥 + 根绑定签名 + **`rootPubkey` 可选字段**——A16 切片一设计补充：验绑须比 sha256(rootPubkey)==名册键，rootId 不可逆推，原结构不含根公钥则合入侧验绑不可能；rootPubkey 是公开信息无泄露增量，serde 兼容 None=不采信） | `core/src/org/types/member.rs:296-314`、`core/src/org/access_key.rs`（derive/verify/strip 纯逻辑） | 已复活（A16 切片一） |
| opt-in 个人身份开关 | `usePersonalIdentity` 骨架 | 已就绪 |
| 多设备一致性 | 同 seed 派生同一域身份（纯函数） | 天然保证 |

## 五、迁移路径

1. **改密功能上线**：无 pwv 设备视为"从未改密"（正常态）；首次改密产生 `pwv:self` 后三件套自然激活；旧版本设备收到含未知 reason 的 epoch 标记按 Unknown 兜底忽略（不 fail-closed）；
2. **密码考试**：纯新增，无迁移；首次启用时以启用时刻为 `lastPasswordAuth` 初始值（避免老用户当场被拦）；
3. **OS 目录**：安装器检测旧布局 → 一次性移动 → 写布局标记；移动失败回退旧布局并提示；
4. **org_user_id**：存量 rootId 名册迁移属 community 篇（双写过渡）。

## 六、验收

- **向量**：pw 模块线形向量（pwv / pwack 逐字节、Kverify / Kack 派生、ack MAC、门控谓词真值表、grace 边界 ±1ms、从未验证立即暂扣）——E1 已自产，接线后补端到端组；
- **单测**：密码考试计时（7 天 ±1s、生物识别不刷新、密码刷新、忘记密码出口）；OS 布局解析与迁移回退；
- **集成**：双设备改密 e2e——A 改密 → B 引导态 → 输新密码（V 校验 + 重封 + ack）→ B 恢复数据面；遗忘设备（不知新口令）grace 后断粮；QR 旧口令恢复 gated → unify → 收敛（§13.8 既有用例扩展）；
- **回归**：epoch 轮换（M3 既有用例）、device-trust 恢复契约、identity_full_lifecycle 全绿。

---

> 关联：product/todo #3（§4.1）、#4（§4.2）、#5（§4.3）、#2（§4.4 地基，上层在 architecture/community/membership 篇）；`wiki/architecture/identity/password-change-propagation.md`（改密设计唯一权威，本篇不重述规格）。

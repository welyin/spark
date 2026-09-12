# 架构设计：网络（打洞、中继、网关与叶子）

> 模板：产品目标 → 现状 → 差距 → 目标设计 → 迁移路径 → 验收。对应产品文档：[../../product/foundation/network.md](../../product/foundation/network.md)（定稿）。

## 一、产品目标（来自 product/foundation/network.md）

1. 「打洞 → 中继 → 离线暂存」三级兜底，验收标准一句：**成员感觉不到网络的存在**；
2. **中继 = 公网节点的自然属性 + 全网共享池**：自动启用、无需登记、配额防白嫖、**切换零成本**（无状态 / 无注册 / 无批准）——"这张网无人能够收购"的网络层落点；
3. **网关邮箱**：中继管"在线但打不通"、网关管"不在线"；网关是角色不是账号——**全员候选 + 活跃成员轮流履职，不可被指定**（无管理员指派入口，通信面不长权力中心）；密文暂存、作恶只能丢信；
4. **手机是叶子**：砍掉一切"为网络 / 他人服务"的角色，保留全部消费能力；组织空间单连接，离线暂存与地址发布两件脏活优先找网关活跃集；
5. Q04：组织自备一台公网可达节点为部署前置（组织自助，内核无新机制）；
6. 离线通知诚实边界（系统通道 / 第三方 IM 提醒 = 可选插件，不是内核承诺）。

## 二、现状（核实日期：2026-09-08）

**wiki**：`wiki/product/relay-strategy.md`（2026-09-01 **设计定稿、待转实施**）；`architecture/p2p/`：`dcutr-hole-punch.md`（打洞）、`peer-rediscovery-implementation.md`（DHT 重寻址）、`relay-implementation.md`、`mobile-leaf-mode.md`、`org-gateway-mailbox.md`（**已实现** `/spark/org-mail/1.0.0` deliver/fetch 两 op + golden vectors + 三节点 loopback e2e）、`connection-policy.md`、`android-notifications.md`。

**代码**：`core/src/p2p/`（node、gossip、relay_manager 等）；网关 / 数据账号角色解析在 `core/src/org/roles/mod.rs`（`is_gateway_active` / `gateway_active_set` / `data_account_set` 等反查族），`gateways` / `data_accounts` 字段在 `org/types/record.rs` `OrganizationRecord`；**托管节点向导已实现**（`SystemSettingsPanel.vue` U2 向导，AutoNAT/UPnP 自检展示，relay-implementation §3）。

**关键状态**：① relay 方案**已转实施**（2026-09-01 c8c7dfb 起 + 2026-09-08 A8 补齐「配额超限拒绝」集成测试 `tests/p2p_relay_quota.rs` + 修复被拒时 in-flight 泄漏，G1 已核销）；② **A9 已落地（2026-09-08，G2/G3 核销）**：`gateways` 指定通路全链移除（service/kernel/命令层/UI 入口删除，字段 serde 解析兼容、读取即忽略、保存即老化），活跃集计分推导（在线 > 最近活跃 > rotate_key 轮换 > 字典序，叶子不履职；`roles/mod.rs` `GatewayCandidateScore`/`select_gateway_active`），地址记录 gateways 线形不变、语义转为「履职集快照」，org-mail fetch 等调用点已迁移新活跃集；③ **A10 已落地（G4 核销）**：`wiki/product/p2p-relay.md` 已标 superseded（指向 relay-strategy.md，`notify-relay.md` 未误标）。

## 三、差距

| # | 差距 | 出处 |
| --- | --- | --- |
| G5 | 叶子模式（`mobile-leaf-mode.md`）现状实现状态未与本篇对齐（product 注"部分能力待转实施"） | product/network §手机是叶子 |

G6 已由 A47 落地核销（2026-09-08：`org_gateway_active_set` 只读命令 + OrgSettingsPanel「当前履职网关」只读行）。

## 四、目标设计

### 4.1 relay 转实施（以 relay-strategy.md 为唯一权威）

落地四步，全部为既有 libp2p 能力的接线、零协议发明：

1. **资格自动判定**：AutoNAT 实测可达（公网 IPv4 / IPv6 / UPnP 稳定映射均可，地址族无关）→ 自动 `enable_relay_server`；动态 IP 不否决、只降权（presence 周期 + 即时重发维持地址新鲜度）；
2. **发布与发现**：relay 节点对约定键 `spark:relay` 做 DHT provide + identify 宣告 hop 能力；任何节点上线即 `get_providers("spark:relay")` 得全网共享池（叶子经 kad client 一次性查询，零配置）；
3. **选择纪律**：直连 > 自设备 / 本组织 relay > 共享池；多 relay 分摊防单点作恶；切换零成本（relay 无状态）；
4. **配额**：预约限时限量（默认 2h / 256MiB 续期），转发限量防白嫖。

### 4.2 网关活跃集：移除指定通路 + 既有推导计分细化（#15 核心设计）

- **移除指定通路**：`OrganizationRecord.gateways` 字段消费侧及反查、`kernel` / 命令层相关入口一并删除（存量记录中的 gateways 字段忽略处理，见迁移）——删除的只是"指定"能力，活跃集机制保留；
- **活跃集推导在既有实现上细化**（现状 `gateway_active_set`：全员候选 + sha256 确定性轮换、小时粒度、N=3，全成员独立算出同一排序、无需协调）：
  - 在既有 `rotate_key` 轮换基础上**叠加计分因子**：在线状态（当前在线优先）+ 最近活跃时间（存证 / 同步元数据可证）+ 设备类型（PC 类优先于叶子）；
  - **视图分歧如实说明**：各节点在线视图天然不一致，履职集可能暂时分歧——org-mail"任一履职即收 + 按消息唯一标识去重"幂等容忍冗余（现状注释"职责天然幂等，超发无害"同口径），无需视图协商；
- **履职**：org-mail deliver 时向履职集投递（任一在线履职网关即收）；发件人侧按消息唯一标识去重；履职网关密文暂存、转发成功回执（org-mail 既有语义不变）；
- **不可指定的结构保证**：不存在"设置网关"的数据入口与命令——指定能力不是被禁用，而是**不存在**（与"协议无 fee 字段"同族的结构封死）；
- **叶子衔接**：手机组织空间单连接，离线暂存与地址发布优先发履职集（product §手机是叶子原句落地）。

### 4.3 Q04 与部署前置

内核零工作（已拍板）。组织管理应用在"组织无公网可达节点"时如实提示并给出自助引导（自备一台常开设备 / 小主机），属 UI 诚实告知，非机制。与已实现的系统设置托管向导（U2）关系：组织视角提示**复用**该向导的自检展示，不另建一套。

### 4.4 文档治理

`wiki/product/p2p-relay.md` 顶部加 superseded 指引（指向 relay-strategy.md）；`notify-relay.md`（第三方 IM 提醒插件）仍有效，勿误标（product/todo #12）。

## 五、迁移路径

1. **gateways 字段退役**：存量组织记录中 `gateways` 字段读取时忽略（serde 兼容：保留字段不解析即自然忽略），活跃集推导即时生效；不写字段清除迁移（随记录自然老化）；
2. **relay 分阶段**：P1 relay server 自动启用 + DHT 发布 / 发现；P2 选择纪律与配额；P3 叶子共享池查询接入。旧版无 relay 时不影响直连与 org-mail 既有路径；
3. **活跃集上线无断档**：现状代码缺省（gateways 空）即走轮值推导，忽略字段后存量组织自然落入全员候选推导、投递语义不变——无需双跑。

## 六、验收

- **向量**：活跃集推导确定性用例（固定名册 + 固定在线视图 → 固定履职集；同分 tie-break 按 identity 字典序）；relay 发现 / 预约 / 配额向量（relay-implementation 既有组扩展）；
- **单测**：履职集随在线变化自动更替；gateways 字段忽略读取；配额超限拒绝；
- **集成**：三节点——A 离线、B/C 履职集 → D 发 org-mail → B/C 暂存 → A 上线 fetch 取回（org-mail 既有 golden vectors `spec/vectors/org-mail.json` 回归）；
- **回归**：dcutr 打洞、peer-rediscovery、mobile-leaf-mode 既有测试全绿。

---

> 关联：product/todo #15（§4.2）、#12（§4.4）；`wiki/product/relay-strategy.md`（relay 唯一权威，本篇不重述规格）；architecture/community/membership 篇（数据节点活跃集与本篇网关活跃集共用"确定性轮值推导"模式）。

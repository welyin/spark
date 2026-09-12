# 架构设计：个人数据（同步、副本与配额）

> 模板：产品目标 → 现状 → 差距 → 目标设计 → 迁移路径 → 验收。对应产品文档：[../../product/foundation/personal-data.md](../../product/foundation/personal-data.md)（定稿）。

## 一、产品目标（来自 product/foundation/personal-data.md）

1. 个人数据只存于本人设备（两级承诺：个人域不承诺"永不丢"，组织数据才有 K 副本承诺）；
2. **K=3 目标制**：K=3 是目标兼上限而非保证（设备 ≤3 即全量）；**核心数据**（结构化记录、消息文本）豁免上限、始终全量；**大块数据**（附件 / 文件）严格 K=3、内容寻址分块、按需 P2P 回补；
3. 配额与自动清理：驱逐不杀最后副本；副本健康度如实可见（Q01：onboarding 告知"你当前只有 N 份副本"）；
4. 配对设备完全信任（Q02：不做设备级分级）；个人域数据永不落在非自有设备上（Q17）；
5. 删除语义诚实（撤回 / 删除是"不再展示"，非物理抹除）；整体可导出。

## 二、现状（核实日期：2026-09-08）

**wiki**：`wiki/architecture/sync/personal-data-sync.md`（P1–P5 已实现并联调通过，2026-08）；`personal-data-sync-windows.md`（消息窗口）；`personal-data-sync-phases.md`（分期）。

**代码**：`core/src/sync/pdsync.rs`、`sync/personal.rs`、`sync/dlog.rs`、`sync/meta.rs`（VersionVector/DocMeta）、`kernel/inbound_dm/pdsync.rs`、`core/src/data-mgmt/exporter.rs`（全库 JSON 整体导出已存在，按现状全量模型工作）。

已实现的关键事实：

- **全量备份模型**：个人域 = 同一 rootId 下全部配对设备组成的完全信任副本组，除消息历史（最近窗口）外**一切数据全量同步**；信封三件套 hello / need / data，记录级向量时钟 + LWW（ts 相同按 nodeId 字典序兜底）；
- **删除日志**（dlog，2026-08 联调后取代墓碑增量方案）：`dlog:entry:{seq}` + `dlogAck` 回执 + 严格 GC，墓碑 pmeta 永久保留防复活；
- **附件策略已有雏形**：hello 携带 `attachmentPolicy: eager | lazy`（PC 默认 eager 全量拉附件，手机默认 lazy 点击才拉）与 `msgWindow`（每设备可配）；
- **设备清单即副本组成员列表**（`device:*` 纳入同步，`DeviceRecord.deviceUid` 标识物理设备）；
- **blob 层已落地（A1，2026-09-08）**：`core/src/sync/blob/`（mod / presence / fetch）——内容寻址分块（1MiB 阈值 / 256KiB 块）、manifest 与 `blob:presence:{cid}:{deviceUid}` 位图账本纳入 pdsync（`GATED_PUSH_CATEGORIES` 灰度门控）、`blob-fetch`/`blob-chunk` 回补信封（offset 续拉、收齐 sha256 校验）、确定性副本计数（`replica_summary`）、三态诚实降级；规格 `wiki/protocol/p2p/personal-data-sync.md` §14（字节级权威）+ golden vectors `spec/vectors/blob.json`（8 用例）+ 三设备 loopback 集成（`kernel_blob_sync.rs`）。

- **配额与驱逐已落地（A2，2026-09-08）**：`sync/blob/quota.rs`（`blob:quota` 配置 PC 10GiB/移动 1GiB、`blob:access:{cid}` LRU、水位、`k_target=min(3,未撤销设备数)`）+ `sync/blob/evict.rs`（`plan_eviction` 纯函数、GC `plan_gc`/`gc_unreferenced`）——**位次规则**（完整持有者按 deviceUid 升序，`rank ≥ K` 才可驱逐，`rank < K` 或账本未覆盖永不驱逐）使「副本 ≤K 永不驱逐」被蕴含且全设备并发驱逐无需协调即精确收敛 K；hello 扩展 `blobQuota`（老端不读兼容）；规格 §15 + `blob_quota.json` 6 向量。GC 周期接线待 A4 定义引用形态（§15.5，防空引用集误清）。

- **存量补登与 GC 已落地（A4，2026-09-08）**：`sync/blob/migrate.rs`——持续调和补登（每轮 hello 扫描「pdoc 引用 ∩ blob:data 在库」迁入 blob 层，存量与增量统一收口、幂等稳态零写）；P6 hash == blob cid（既有 `$blob` 引用天然即 cid 引用，记录零改写）；引用收集三前缀（pdoc/feed:inbox/msg:item）驱动 GC 周期接线；**驱逐标记 `blob:evicted:{cid}`**（§16.4——解「驱逐→P6 重拉→补登再迁入」死循环：驱逐置标记 P6 跳过，显式意图[mark_want/重写/回补落块/GC]解除）；读穿回退（旧设备从新设备 blob 层拉取无感）。

- **副本健康度与 onboarding 已落地（A3，2026-09-08，批次一收官）**：`sync/blob/health.rs` `blob_health` 域级聚合（deviceCount / kTarget / totalBlobs / underKBlobs / minFullReplicas / 配额水位，确定性复算、只提醒不处置）+ 壳层三命令（`root_blob_health` / `root_get_blob_quota` / `root_set_blob_quota`）+ 设置页「存储与副本」模块（「你当前只有 N 份副本」头部）+ 注册流程 onboarding 告知；头部 N 规则（§16.6：有 blob 取 minFullReplicas、无 blob 取 deviceCount、单设备 N=1、≤3 台退化全量如实表达）。遗留：presence 变化实时推送与配对流程告知（登记 A49）。

**现状没有**：无（personal-data 篇目标全部落地；G4/G5 为跨篇承接与演进项）。

## 三、差距

| # | 差距 | 出处 |
| --- | --- | --- |
| G4 | 组织数据在自设备间走 pdsync（§1.1 概念错位修复的既有决策），K=3 模型需与组织域全员数据节点（architecture/community 篇）口径区分而不冲突 | product/community/membership §四 |
| G5 | K=3 分块化后"整体可导出"语义改变：exporter 按现状全量模型工作，本机缺块时导出语义未定义（先回补还是如实标注缺失） | product/personal-data「导出与可携带」 |

## 四、目标设计

### 4.1 数据二分：核心数据全量、blob 数据 K=3

| 类别 | 内容 | 同步模型 |
| --- | --- | --- |
| **核心数据** | pdsync §3.1 表内全部结构化记录（资料 / 设备 / 通讯录 / 会话元数据 / 消息文本 / org:meta 等） | **维持现状不变**（全量，向量时钟 + dlog）——体积小且每台设备渲染必需，产品豁免上限 |
| **blob 数据** | 附件、文件（消息附件、个人文件应用的文件） | **新增 K=3 分块模型**（本节） |

设计要点：不动 pdsync 既有通道，blob 层作为**独立的内容寻址层**叠加，消息 / 文件记录中只存 blob 引用（cid），不内嵌内容。

### 4.2 blob 层：内容寻址 + 分块 + 副本账本

- **寻址**：`cid = sha256hex(blob 内容)`；大块（> 阈值，建议 1 MB）切分为定长 chunk（建议 256 KB），`chunkCid = sha256hex(chunk)`，blob manifest = `{ cid, size, chunkSize, chunkCids[] }`；
- **存储键**：`blob:meta:{cid}`（manifest）、`blob:chunk:{chunkCid}`（内容）；消息 / 文件记录只存 `cid`；
- **副本账本**：`blob:presence:{cid}:{deviceUid}`（本机持有声明，含 chunk 位图）。presence 记录**纳入 pdsync 同步**（结构化小记录，走核心数据通道）——于是**每台设备都能确定性算出每个 blob 的域内副本数**（去重 deviceUid 计数），这就是健康度与驱逐判定的共同输入，天然满足"任何节点复算结果一致"；
- **持有即做种**：任何持有 chunk 的设备响应拉取；手机 lazy 模式下不主动持 blob，自然不计入副本（与既有 attachmentPolicy 衔接，个人域不再另造"PC 计入口径"——eager 即计入）。

### 4.3 按需回补

- 缺块设备读 blob 时：本地无 chunk → 查 presence 账本选持有设备 → 直连拉取（复用 dm 通道，新增信封 `blob-fetch { chunkCid }` / `blob-chunk { chunkCid, data }`）。**传输形态（§14.5，dm 单帧 1MiB 约束的落地取舍）**：块 ≤256KiB 单信封整块传输；单块 blob（>256KiB 且 ≤1MiB 不切块的）按 240KiB offset 切片续拉（P6 attachment 同款线形）——1MB 阈值与 256KB 块长不变；
- 拉取失败（持有者离线）：**诚实降级**为"暂不可用"提示（与个人域"不承诺永不丢"一致），后台待持有者上线自动重试；
- 拉取成功后本机落 chunk + 写 presence（副本数可能因此 >K——回补瞬时态，允许；"K=3 目标兼上限"的语义落点是**配额驱逐收敛至 K**，不拦回补路径；跨域同口径参照 product/community/membership §四"成员可主动多存"）。

### 4.4 配额与自动清理（驱逐不杀最后副本）

- 每设备声明配额（hello 扩展 `blobQuota` 字段，用户可配，默认建议 PC 10 GB / 移动 1 GB）；
- 超配额触发驱逐：按"最久未访问 + 域内副本数 >K"优先驱逐——**副本数 ≤K 的 chunk 永不驱逐**（驱逐不杀最后副本是硬规则，在驱逐选择器内核纯逻辑中强制，不靠调用方自觉）；
- 驱逐只删 `blob:chunk:` 内容，presence 同步标记移除；manifest 与消息记录不动（可随时回补）；
- 设备 ≤3 台时 K=3 退化为全量（副本上限自然等于设备数），单设备用户即单副本——onboarding 如实告知（G3 健康度同一数据源）；
- **blob 删除与 GC**：消息 / 文件删除（dlog 传播）后 blob 引用消失，本机 chunk 进入"无引用"集合，由本地周期 GC 清理（不占驱逐通道；记录已删则"不杀最后副本"不再适用——副本语义随记录生命周期结束）；presence 随清理同步移除；远端副本是否清理由各设备自主（与"已发出的副本收不回"一致）。

### 4.5 副本健康度与 onboarding 告知

- 健康度 = 对每 blob：域内副本数（presence 去重计数）；聚合成域级摘要：当前 blob 总量、副本数 <K 的 blob 数、本设备配额水位；
- UI 展示"你当前只有 N 份副本"（单设备用户 N=1）；副本不足**只提醒不处置**（Q06 同口径，个人域不做任何机制性干预）；
- 组织数据的 K=3（全员数据节点）在 architecture/community 篇设计，健康度计算复用同一 presence 账本模式，口径各自独立（G4 不冲突）。

## 五、迁移路径

1. **存量附件**：现状已全量存在于各设备——无需数据迁移。blob 层上线时做一次性"补登"（扫描存量附件 → 生成 manifest / chunk 键 / presence，先例：dlog 引入时的墓碑补登 §5.6）；
2. **行为切换**：补登完成后副本数普遍 ≥ 设备数，配额生效后各设备按 4.4 自主驱逐至 K 目标——驱逐是纯本地行为，收敛靠 presence 同步；
3. **协议兼容**：旧设备不识别 `blob-fetch` 信封按未知信封丢弃（既有先例）；新旧混跑期间旧设备仍是 eager 全量副本（语义安全，只是无 K 上限）；
4. 分阶段落地：P1 blob 层 + presence 账本 + 按需回补；P2 配额与驱逐；P3 健康度展示与 onboarding 告知；
5. **导出衔接（G5）**：整体导出走既有 exporter 通道——核心数据照常全量导出；blob 内容导出前按 manifest 逐项尝试回补，回补失败（持有者离线）的 blob 在导出包缺失清单中**如实标注**（cid + 缺块位图），不静默缺省（与个人域"不承诺永不丢"一致）。

## 六、验收

- **向量**（`code/spec/vectors/`）：blob manifest 逐字节、chunk 切分边界（空 blob / 恰阈值 / 阈值 ±1）、presence 账本确定性计数用例、驱逐选择器用例（副本 =K 不驱逐 / >K 按龄驱逐 / 全设备并发驱逐收敛）；
- **单测**：回补（缺块→拉取→落 presence）、拉取失败降级、配额水位计算、K 语义（设备 1/2/3/5 台各自的副本目标）；
- **集成**：三设备 loopback——A 写大附件 → B/C 同步 manifest → B 驱逐 → C 保留 → B 再读时向 C 回补；删除传播（dlog 与 blob presence 的交互：撤回消息后 blob 引用消失但 chunk 不连坐删除，保留语义同"已发出的副本收不回"）；
- **回归**：pdsync 现有向量与 `pdsync/tests.rs` 全绿（核心数据通道零改动证明）。

---

> 关联：product/todo #1（本条落地）；architecture/community/membership 篇（组织域全员数据节点，presence 账本模式复用）；[../../product/foundation/evidence.md](../../product/foundation/evidence.md)（导出包，blob 内容寻址直接服务导出与核验）。

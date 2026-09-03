//! K 副本统计（对齐 org.md §12 / org-share-sync.ts:107-176 `getOrgSyncOverview`）。
//!
//! 计入副本的判定（本机恒算一个）：**30 天窗口内同步过** 或 **sync-state 版本
//! 仍覆盖当前组织版本**，二选一（constants.ts:154-161 的设计注释：不能只用版本
//! 比较——每次编辑会瞬间翻转；也不能只用 TTL——静默组织不会刷新 sync-state）。
//!
//! ## 有意修复（org.md §12.3）
//!
//! TS 中 share 路径写入的污染形状 sync-state 使 `coversCurrent` 恒 true（成员
//! 永久计入 everSynced，绕过 30 天窗口）。Rust 的 [`covers_current`] 按四字段
//! 真实比较：sync-state 版本不落后于当前版本才成立。配合
//! [`crate::org::sync_state`] 的规范写入，统计语义恢复设计本意。

use super::snapshot::is_organization_sync_stale;
use super::sync_state::OrgSyncState;
use super::types::{OrganizationMember, OrganizationRecord, OrganizationSyncVersions};
use crate::p2p::{DhtMode, RecoveryState};

/// 副本目标 K（含本机，p2p/constants.ts:152）。
pub const ORG_REPLICA_TARGET: u32 = 3;

/// 副本新鲜窗口：30 天（p2p/constants.ts:161）。
pub const ORG_REPLICA_FRESH_WINDOW_MS: i64 = 30 * 24 * 60 * 60 * 1000;

/// 单成员副本状态。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemberSyncOverview {
    /// 成员 rootId。
    pub root_id: String,
    /// 成员 peerId（trim 后；无 nodeInfo 时为 `None`）。
    pub peer_id: Option<String>,
    /// 是否本机当前用户。
    pub is_self: bool,
    /// 是否计入副本（本机恒 true）。
    pub ever_synced: bool,
    /// 该成员 peer 的最近同步时间（无记录为 `None`）。
    pub last_synced_at: Option<i64>,
}

/// 组织副本概览。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OrgSyncOverview {
    /// 组织 id。
    pub org_id: String,
    /// 副本目标 K。
    pub replica_target: u32,
    /// 已持有副本数——K 口径适用（组织有 data-accounts 集合）时为**去重后的
    /// （数据账号 × PC 设备）履职对数**；不适用时为成员级 everSynced 计数
    /// （展示沿用，不参与达标判定）。
    pub synced_peers: u32,
    /// 有效成员数（rootId 非法的成员被跳过，不计入）。
    pub total_members: u32,
    /// 逐成员状态（按记录 members 顺序）。
    pub members: Vec<MemberSyncOverview>,
    /// 当前已连接的组织成员节点数（不含本机；含本机副本数 = `connected_peers + 1`）。
    /// 由 kernel 按 P2P 连接表填充；纯统计路径（[`compute_org_sync_overview`]）为 0。
    pub connected_peers: u32,
    /// 恢复模式状态（kernel 从 `RecoveryTrigger` 快照填充）。
    pub recovery_state: RecoveryState,
    /// 最近一次与组织成员建立连接的时间（无记录为 `None`；含当前仍连接的成员）。
    pub last_connected_at: Option<i64>,
    /// DHT 模式（kernel 从持久化配置填充）。
    pub dht_mode: DhtMode,
    /// 组织网络状态判定结果（[`decide_org_network_status`]；纯统计路径为 `LocalOnly` 占位）。
    pub status: OrgNetworkStatus,
    /// K 口径是否适用（组织声明了 data-accounts 集合）。false = 纯
    /// all-members 组织——无 K，不进达标判定、不提醒（batch2 §1.2）。
    pub k_applicable: bool,
    /// 两级记账第二级：逐数据账号的 PC 履职达标状态（证据源 = hello 履职
    /// 观测，见 [`DutyObservation`]）。
    pub data_accounts: Vec<DataAccountOverview>,
}

/// 逐数据账号概览（O1 两级记账的第二级：组织级 m/3 见 `synced_peers`）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DataAccountOverview {
    /// 数据账号 rootId。
    pub root_id: String,
    /// 该账号的 PC 类设备是否持有副本（everSynced 且设备类为 pc）。
    pub pc_synced: bool,
    /// 设备类（pc/mobile；无设备记录兜底 pc）。
    pub device_class: &'static str,
}

impl OrgSyncOverview {
    /// 副本是否达标（batch2 §1.2）：K 不适用（纯 all-members 组织）→ 恒达标
    /// （不提醒）；适用 → 组织级去重 PC 履职对 ≥ K 且每个数据账号 ≥1 台 PC
    /// 窗口内履职。不达标只提醒，无自动处置（红线不变）。
    pub fn is_replica_sufficient(&self) -> bool {
        if !self.k_applicable {
            return true;
        }
        self.synced_peers >= self.replica_target
            && self.data_accounts.iter().all(|a| a.pc_synced)
    }
}

/// 副本达标判定（成员级旧口径的独立助手，保留给网络状态判定等旧消费点）。
pub fn replica_sufficient(synced_peers: u32) -> bool {
    synced_peers >= ORG_REPLICA_TARGET
}

/// 数据账号履职观测（batch2 §1.2 的证据源）：hello 入站 roles 含 "data" 时
/// 按设备记录——（数据账号 rootId, 设备 peerId, deviceClass, 观测时刻）。
/// 「持有副本」的操作性定义 = 该设备在新鲜窗口（30 天）内履职被观测到。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DutyObservation {
    /// 数据账号 rootId。
    pub root_id: String,
    /// 履职设备的连接层 peerId。
    pub peer_id: String,
    /// 设备类（hello `deviceClass` 字段："pc" / "mobile"；手机不计入 K）。
    pub device_class: String,
    /// 观测时刻（ms，本地接收时刻）。
    pub observed_at: i64,
}

impl DutyObservation {
    /// 窗口内 PC 履职（计入 K 的副本证据）。
    fn is_pc_fresh(&self, now_ms: i64) -> bool {
        self.device_class == "pc" && now_ms - self.observed_at <= ORG_REPLICA_FRESH_WINDOW_MS
    }
}

/// O1 数据账号达标判定（wiki `org-data-sync.md` §4）：**全体数据账号 PC
/// 设备副本合计 ≥ 3，且每个数据账号至少 1 台 PC 计入**。
///
/// - `pc_total`：数据账号中 PC 且 everSynced 的数量（成员表口径每账号一个
///   nodeInfo 端点，故逐账号计数）；
/// - `accounts`：逐账号 PC 达标状态；
/// 两者都满足才达标。不达标只提醒，无自动处置（决定权在管理员）。
pub fn data_accounts_sufficient(accounts: &[DataAccountOverview]) -> bool {
    if accounts.is_empty() {
        return false;
    }
    let pc_total = accounts.iter().filter(|a| a.pc_synced).count() as u32;
    pc_total >= ORG_REPLICA_TARGET && accounts.iter().all(|a| a.pc_synced)
}

// ------------------------------------------------------------------
// 组织网络状态判定（development_plan「组织网络状态 UI」；kernel 采集输入，纯函数判定）
// ------------------------------------------------------------------

/// 「丢失」判定防抖：连续无组织成员连接超过该时长才判 Lost（需求 10–30s，取 15s）。
pub const ORG_NETWORK_LOST_DEBOUNCE_MS: i64 = 15_000;

/// 组织网络状态（五层；线形字符串见 [`OrgNetworkStatus::as_str`]）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OrgNetworkStatus {
    /// 组织网络良好：已连接副本达到目标（绿）。
    Good,
    /// 不稳定：有部分连接但少于目标，或断开仍在防抖窗口（黄）。
    Unstable,
    /// 组织网络丢失：所有已知成员地址失败（红；自动进入恢复模式）。
    Lost,
    /// 正在恢复中：DHT/恢复查询查找中（橙；超时由 `recovery_state` 转 failed 呈现）。
    Recovering,
    /// 仅本地：完全离线（灰）。
    #[default]
    LocalOnly,
}

impl OrgNetworkStatus {
    /// 线形字符串（DTO/前端分支用）。
    pub fn as_str(&self) -> &'static str {
        match self {
            OrgNetworkStatus::Good => "good",
            OrgNetworkStatus::Unstable => "unstable",
            OrgNetworkStatus::Lost => "lost",
            OrgNetworkStatus::Recovering => "recovering",
            OrgNetworkStatus::LocalOnly => "localOnly",
        }
    }
}

/// [`decide_org_network_status`] 的输入（kernel 侧采集）。
#[derive(Clone, Copy, Debug)]
pub struct OrgNetworkStatusInput {
    /// P2P 节点是否运行中。
    pub p2p_running: bool,
    /// 全网已连接 peer 数（区分「组织丢失」与「完全离线」）。
    pub total_connected_peers: usize,
    /// 已连接的组织成员节点数（不含本机）。
    pub connected_peers: u32,
    /// 副本目标 K。
    pub replica_target: u32,
    /// 有效成员数（小组织目标折算：`min(K, total_members)`）。
    pub total_members: u32,
    /// 恢复模式状态（`RecoveryTrigger::state` 快照）。
    pub recovery_state: RecoveryState,
    /// 连续无组织成员连接时长（有成员连接时为 0）。
    pub unreachable_ms: i64,
}

/// 组织网络状态判定（纯函数）。
///
/// 优先级：仅本地（P2P 未启动）→ 良好（连接副本含本机达标，小组织按成员数
/// 折算目标）→ 不稳定（有远端连接但不足目标，或断开未过防抖）→ 恢复中
/// （恢复查询窗口内）→ 仅本地（全网零连接）→ 丢失。
pub fn decide_org_network_status(input: &OrgNetworkStatusInput) -> OrgNetworkStatus {
    if !input.p2p_running {
        return OrgNetworkStatus::LocalOnly;
    }
    let effective_target = input.replica_target.min(input.total_members.max(1)).max(1);
    // 本机恒算一个已连接副本
    if input.connected_peers + 1 >= effective_target {
        return OrgNetworkStatus::Good;
    }
    if input.connected_peers > 0 || input.unreachable_ms < ORG_NETWORK_LOST_DEBOUNCE_MS {
        return OrgNetworkStatus::Unstable;
    }
    if matches!(input.recovery_state, RecoveryState::Recovering { .. }) {
        return OrgNetworkStatus::Recovering;
    }
    if input.total_connected_peers == 0 {
        return OrgNetworkStatus::LocalOnly;
    }
    OrgNetworkStatus::Lost
}

/// `coversCurrent`：sync-state 版本仍覆盖当前组织版本（不落后）。
///
/// 正确语义（有意修复）：`!isStale(state.versions, currentVersions)`，
/// 四字段真实比较；currentVersions 缺失时不成立。
pub fn covers_current(
    state: &OrgSyncState,
    current_versions: Option<&OrganizationSyncVersions>,
) -> bool {
    match current_versions {
        Some(current) => !is_organization_sync_stale(Some(&state.versions), current),
        None => false,
    }
}

/// 单成员 `everSynced` 判定：`isSelf || recentlySynced || coversCurrent`。
pub fn member_ever_synced(
    is_self: bool,
    state: Option<&OrgSyncState>,
    current_versions: Option<&OrganizationSyncVersions>,
    now_ms: i64,
) -> bool {
    if is_self {
        return true;
    }
    let Some(state) = state else {
        return false;
    };
    let recently_synced = now_ms - state.last_synced_at <= ORG_REPLICA_FRESH_WINDOW_MS;
    recently_synced || covers_current(state, current_versions)
}

/// `getOrgSyncOverview` 纯函数版：对组织每个成员（按记录顺序）判定副本状态。
///
/// - `current_root_id`：本机当前用户（判定 isSelf；未登录为 `None`）
/// - `current_versions`：当前组织版本（`record.sync.versions`，缺失时调用方
///   应以 `build_organization_sync_versions_default(record)` 兜底）
/// - `state_lookup`：**按账号查** org-sync-state（O1：`(rootId, legacy_peerId)`
///   ——调用方实现账号键优先 + 旧 peerId 键迁移回填，见
///   [`super::sync_state::read_org_sync_state_account`]）
/// - `duty`：本组织的 hello 履职观测（batch2 §1.2 的 K 记账证据源；
///   `orgq_da_duty_observations` 采集）
/// - `has_data_collections`：组织声明了 data-accounts 集合（K 口径适用性）；
/// - `device_class_of`：数据账号的设备类兜底查询（无履职观测时按设备记录
///   判定，无记录兜底 pc——宁可多算不漏算）
/// - rootId 为空串的成员跳过（对齐 TS 的 `if (!rootId) continue`）
///
/// K 口径（batch2 §1.2，overview 两级输出）：all-members 集合**无 K**
/// （全员全量，不做达标判定）；data-accounts 集合的副本计数单元 =
/// （数据账号 × PC 设备）对，证据 = 30 天窗口内履职观测（hello roles 含
/// "data" 且 deviceClass="pc"）。组织级 = 去重合计 ≥ 3；逐账号 = 每个
/// 数据账号 ≥1 台 PC 窗口内履职。本机是数据账号且为 PC 时自证计入
/// （本地驻留可服务是事实，无需 hello 自观测）。
pub fn compute_org_sync_overview(
    record: &OrganizationRecord,
    current_root_id: Option<&str>,
    current_versions: Option<&OrganizationSyncVersions>,
    mut state_lookup: impl FnMut(&str, Option<&str>) -> Option<OrgSyncState>,
    duty: &[DutyObservation],
    has_data_collections: bool,
    device_class_of: impl Fn(&str) -> &'static str,
    now_ms: i64,
) -> OrgSyncOverview {
    let members = &record.members;
    let org_id = &record.org_id;
    let mut overview_members = Vec::new();
    let mut member_level_synced = 0u32;

    for member in members {
        if member.root_id.is_empty() {
            continue;
        }
        // 端点化：取成员端点集首个 peerId 作为代表（sync-state 已按 rootId
        // 账号定键，peer_id 仅为旧键兜底 + overview 展示）。
        let peer_id = member
            .node_info
            .as_ref()
            .and_then(|set| set.iter().find_map(|e| e.peer_id.as_deref()))
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(str::to_string);
        let is_self = current_root_id == Some(member.root_id.as_str());
        let state = state_lookup(&member.root_id, peer_id.as_deref());
        let ever_synced = member_ever_synced(is_self, state.as_ref(), current_versions, now_ms);
        if ever_synced {
            member_level_synced += 1;
        }
        overview_members.push(MemberSyncOverview {
            root_id: member.root_id.clone(),
            peer_id,
            is_self,
            ever_synced,
            last_synced_at: state.map(|s| s.last_synced_at),
        });
    }

    // batch2 §1.2 K 口径：仅 data-accounts 集合参与 K=3 记账
    let k_applicable = has_data_collections;
    let data_account_roots = crate::org::roles::data_account_set(record);
    let data_accounts: Vec<DataAccountOverview> = data_account_roots
        .iter()
        .map(|root_id| {
            let has_pc_duty = duty
                .iter()
                .any(|d| d.root_id == *root_id && d.is_pc_fresh(now_ms));
            // 本机是数据账号且为 PC → 自证计入（本地驻留可服务是事实）
            let self_pc = current_root_id == Some(root_id.as_str())
                && device_class_of(root_id) == "pc";
            let observed_class: &'static str = match duty.iter().find(|d| d.root_id == *root_id) {
                Some(d) if d.device_class == "pc" => "pc",
                Some(_) => "mobile",
                None => device_class_of(root_id),
            };
            DataAccountOverview {
                root_id: root_id.clone(),
                pc_synced: has_pc_duty || self_pc,
                device_class: observed_class,
            }
        })
        .collect();
    // 组织级：去重后的（数据账号 × PC 设备）履职对合计；本机自证占一对
    let duty_pairs: std::collections::HashSet<(&str, &str)> = duty
        .iter()
        .filter(|d| d.is_pc_fresh(now_ms))
        .map(|d| (d.root_id.as_str(), d.peer_id.as_str()))
        .collect();
    let self_pair = if let (Some(self_root), true) = (current_root_id, k_applicable) {
        if data_account_roots.iter().any(|r| r == self_root)
            && device_class_of(self_root) == "pc"
        {
            1
        } else {
            0
        }
    } else {
        0
    };
    let synced_peers = if k_applicable {
        duty_pairs.len() as u32 + self_pair
    } else {
        member_level_synced
    };

    OrgSyncOverview {
        org_id: org_id.clone(),
        replica_target: ORG_REPLICA_TARGET,
        synced_peers,
        total_members: overview_members.len() as u32,
        members: overview_members,
        // 网络状态字段由 kernel.org_overview 采集后填充（纯统计路径无网络上下文）
        connected_peers: 0,
        recovery_state: RecoveryState::Idle,
        last_connected_at: None,
        dht_mode: DhtMode::default(),
        status: OrgNetworkStatus::LocalOnly,
        k_applicable,
        data_accounts,
    }
}

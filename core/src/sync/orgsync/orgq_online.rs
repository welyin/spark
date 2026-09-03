//! orgq（O3）在线数据账号目录 + 成员侧路由决策。
//!
//! - 在线目录：`orgq:da:online:{orgId}:{rootId}` → 最后在线时间戳(ms)，由
//!   `orgsync-hello` 入站解析 roles 含 `"data"` 时更新（hello roles 是数据账号
//!   在线履职声明）；目录是**尽力而为的最近在线提示**（无心跳，过期为陈旧提示），
//!   路由时还需对端连接层 peer ∈ `online_peers` 才真正可达；
//! - 路由：成员侧对 org data-accounts 集合的读取决策（本地直读 / 走 orgq /
//!   全离线回缓存或 UnavailableOffline），选一个在线可达的数据账号发 orgq-req。
//!
//! 从 `orgq.rs` 拆出的子模块（Z5 650 行硬线）。纯逻辑（存储泛型）。

use crate::org::OrganizationRecord;
use crate::storage::StorageBackend;

/// 在线数据账号目录键：`orgq:da:online:{orgId}:{rootId}` → 最后在线时间戳(ms)。
pub fn orgq_da_online_key(org_id: &str, root_id: &str) -> String {
    format!("orgq:da:online:{org_id}:{root_id}")
}

/// 数据账号对某集合的 degraded 标记键：`orgq:da:degraded:{orgId}:{rootId}:{col}`。
/// F4：对端 hello 摘要标注该集合 `degraded:true`（插件未运行 → 只存不服务）时
/// 记录，成员侧路由据此避让（orgq 查询该集合会 denied）。
pub fn orgq_da_degraded_key(org_id: &str, root_id: &str, col_full: &str) -> String {
    format!("orgq:da:degraded:{org_id}:{root_id}:{col_full}")
}

/// 标记某数据账号对某集合 degraded（F4）。值 = 时间戳。
pub fn orgq_mark_data_account_degraded<S: StorageBackend>(
    storage: &mut S,
    org_id: &str,
    root_id: &str,
    col_full: &str,
    now_ms: i64,
) {
    let _ = storage.put(
        &orgq_da_degraded_key(org_id, root_id, col_full),
        &now_ms.to_string(),
    );
}

/// 查询某集合的 degraded 数据账号 rootId 集合（F4，路由避让用）。
pub fn orgq_degraded_for_collection<S: StorageBackend>(
    storage: &S,
    org_id: &str,
    col_full: &str,
) -> std::collections::HashSet<String> {
    let prefix = format!("orgq:da:degraded:{org_id}:");
    storage
        .scan(&crate::storage::ScanOptions::prefix(&prefix))
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(key, _)| {
            // 键形 `...:{rootId}:{col}`，按后缀 col 过滤
            let rest = key.strip_prefix(&prefix)?;
            let (root_id, col) = rest.rsplit_once(':')?;
            if col == col_full {
                Some(root_id.to_string())
            } else {
                None
            }
        })
        .collect()
}

/// 标记某数据账号在线（hello roles 含 "data" 时由入站调用）。值 = 时间戳。
pub fn orgq_mark_data_account_online<S: StorageBackend>(
    storage: &mut S,
    org_id: &str,
    root_id: &str,
    now_ms: i64,
) {
    let _ = storage.put(&orgq_da_online_key(org_id, root_id), &now_ms.to_string());
}

// ── 履职观测（batch2 §1.2：overview K 记账的证据源）─────────────────────

/// 数据账号履职观测键（设备粒度）：`orgq:da-duty:{orgId}:{rootId}:{peerId}`
/// → `{"ts": ms, "deviceClass": "pc"|"mobile"}`。hello 入站 roles 含 "data"
/// 时按设备记录——hello 即「我驻留 data-accounts 集合数据并可服务」的履职
/// 声明（org-orgsync §20.3）。**本地键**（不进同步流量）；K 记账需要窗口期
/// 持久观测（在线目录是瞬态语义，只记最近在线时刻、不带设备类）。
pub fn orgq_da_duty_key(org_id: &str, root_id: &str, peer_id: &str) -> String {
    format!("orgq:da-duty:{org_id}:{root_id}:{peer_id}")
}

/// 记录一次履职观测（hello 入站 roles 含 "data" + deviceClass）。
pub fn orgq_note_data_account_duty<S: StorageBackend>(
    storage: &mut S,
    org_id: &str,
    root_id: &str,
    peer_id: &str,
    device_class: &str,
    now_ms: i64,
) {
    let value = serde_json::json!({ "ts": now_ms, "deviceClass": device_class });
    let _ = storage.put(&orgq_da_duty_key(org_id, root_id, peer_id), &value.to_string());
}

/// 读取某组织的全部履职观测 → `(rootId, peerId, deviceClass, 观测时刻)`。
pub fn orgq_da_duty_observations<S: StorageBackend>(
    storage: &S,
    org_id: &str,
) -> Vec<(String, String, String, i64)> {
    let prefix = format!("orgq:da-duty:{org_id}:");
    storage
        .scan(&crate::storage::ScanOptions::prefix(&prefix))
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(key, raw)| {
            // 键形 `...:{rootId}:{peerId}`，从右解析
            let rest = key.strip_prefix(&prefix)?;
            let (root_id, peer_id) = rest.rsplit_once(':')?;
            let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
            let ts = v.get("ts").and_then(serde_json::Value::as_i64)?;
            let device_class = v
                .get("deviceClass")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("pc")
                .to_string();
            Some((root_id.to_string(), peer_id.to_string(), device_class, ts))
        })
        .collect()
}

/// 在线数据账号目录中某组织标记在线的数据账号（rootId, 最后在线时间戳）。
pub fn orgq_online_data_accounts<S: StorageBackend>(
    storage: &S,
    org_id: &str,
) -> Vec<(String, i64)> {
    let prefix = format!("orgq:da:online:{org_id}:");
    storage
        .scan(&crate::storage::ScanOptions::prefix(&prefix))
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(key, raw)| {
            let root_id = key.strip_prefix(&prefix)?.to_string();
            let ts = raw.parse::<i64>().ok().unwrap_or(0);
            Some((root_id, ts))
        })
        .collect()
}

/// 成员侧路由目标选择：选一个**本组织数据账号**、且标记在线（hello roles
/// "data"）的、且其**连接层 peer 当前在线**的账号（排除本机自己）。返回其
/// rootId。
///
/// `degraded`（F4）：该集合**已降级**（hello 标注 degraded，插件未运行只存不
/// 服务）的数据账号 rootId 集。**优先选非 degraded 的在线数据账号**；仅当
/// 全部在线数据账号都 degraded 时才退而选之（全降级才选之）——避免路由到
/// 必然 denied 的降级数据账号。
pub fn select_online_data_account(
    record: &OrganizationRecord,
    online_peer_ids: &std::collections::HashSet<String>,
    my_root_id: &str,
    degraded: &std::collections::HashSet<String>,
) -> Option<String> {
    // 第一遍：跳过 degraded 的在线数据账号
    if let Some(pick) = pick_online(record, online_peer_ids, my_root_id, |r| {
        !degraded.contains(r)
    }) {
        return Some(pick);
    }
    // 全部在线数据账号都 degraded → 退而选之（全降级才选）
    pick_online(record, online_peer_ids, my_root_id, |_| true)
}

/// 在满足谓词的在线数据账号中选第一个可达的。
fn pick_online(
    record: &OrganizationRecord,
    online_peer_ids: &std::collections::HashSet<String>,
    my_root_id: &str,
    accept: impl Fn(&str) -> bool,
) -> Option<String> {
    for root_id in crate::org::roles::data_account_set(record) {
        if root_id == my_root_id || !accept(&root_id) {
            continue;
        }
        let Some(member) = record.find_member(&root_id) else {
            continue;
        };
        // 连接层在线：成员端点集任一 peer ∈ online_peers
        let reachable = member.node_info.as_ref().is_some_and(|set| {
            set.endpoints
                .iter()
                .filter_map(|e| e.peer_id.as_deref())
                .any(|p| online_peer_ids.contains(p))
        });
        if reachable {
            return Some(root_id);
        }
    }
    None
}

/// 本机对某 org 数据-accounts 集合是否应走 orgq（按需查询）而非本地直读：
/// 本机**非数据账号** 且集合 `accounts == data-accounts`。
pub fn should_route_orgq(record: &OrganizationRecord, my_root_id: &str) -> bool {
    !crate::org::roles::is_data_account(record, my_root_id)
}

/// 成员侧一次 org 数据-accounts 集合读取的路由决策（§20.5 / plugin-data-api
/// §3 读路径透明路由）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MemberReadPlan {
    /// 本地已驻留（本机是数据账号 / 集合 all-members 已驻留），直接读本地。
    Local,
    /// 走 orgq 按需查询：已选在线数据账号。
    Orgq {
        /// 目标数据账号 rootId（orgq-req 的信封 to）。
        target_root_id: String,
    },
    /// 全部数据账号离线：读返回缓存（UI 标注陈旧）或 `UnavailableOffline`。
    Offline { has_cache: bool },
}

/// 计算成员侧读取路由决策。
///
/// - 本机是数据账号（或集合非 data-accounts）→ [`MemberReadPlan::Local`]；
/// - 否则在有在线数据账号时 → [`MemberReadPlan::Orgq`]；
/// - 全部数据账号离线 → [`MemberReadPlan::Offline`]（`has_cache` 指示成员侧
///   缓存命名空间是否已有该集合数据，供调用方决定回缓存还是 UnavailableOffline）。
///
/// `local_resident`：本机是否已驻留该集合数据（数据账号 / all-members 天然驻留；
/// 普通成员对 data-accounts 恒 false）。
/// `degraded`（F4）：该集合已降级的数据账号 rootId 集（路由避让）。
pub fn member_orgq_read_plan(
    record: &OrganizationRecord,
    my_root_id: &str,
    online_peer_ids: &std::collections::HashSet<String>,
    local_resident: bool,
    cache_populated: bool,
    degraded: &std::collections::HashSet<String>,
) -> MemberReadPlan {
    if local_resident {
        return MemberReadPlan::Local;
    }
    match select_online_data_account(record, online_peer_ids, my_root_id, degraded) {
        Some(target) => MemberReadPlan::Orgq {
            target_root_id: target,
        },
        None => MemberReadPlan::Offline {
            has_cache: cache_populated,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::org::types::{OrganizationMember, OrganizationRecord, OrganizationRole};

    fn org_record(
        members: &[(&str, OrganizationRole)],
        data_accounts: &[&str],
    ) -> OrganizationRecord {
        OrganizationRecord {
            org_id: "org_0000000000000001".to_string(),
            name: "t".to_string(),
            description: String::new(),
            avatar: String::new(),
            base_plugin_domain: None,
            created_at: 1000,
            created_by: "creator".to_string(),
            updated_at: 1000,
            members: members
                .iter()
                .map(|(rid, role)| OrganizationMember {
                    root_id: rid.to_string(),
                    role: *role,
                    joined_at: 1000,
                    added_by: "creator".to_string(),
                    node_info: Some(crate::org::types::OrganizationDeviceSet {
                        endpoints: vec![crate::org::types::OrganizationNodeInfo {
                            peer_id: Some(format!("peer-{rid}")),
                            addresses: Vec::new(),
                            device_uid: None,
                        }],
                    }),
                    nickname: None,
                    avatar: None,
                    signature: None,
                    gender: None,
                    region: None,
                    use_personal_identity: None,
                    access_key: None,
                    extra: Default::default(),
                })
                .collect(),
            sync: None,
            gateways: vec![],
            data_accounts: data_accounts.iter().map(|r| r.to_string()).collect(),
            org_address: None,
            is_public: false,
            extra: Default::default(),
        }
    }

    /// batch2 §1.2：履职观测键读写（设备粒度 + deviceClass + 观测时刻）。
    #[test]
    fn da_duty_observation_write_read() {
        let mut s = crate::storage::MemoryStorage::new();
        orgq_note_data_account_duty(&mut s, "org_01", "da-a", "peer-a1", "pc", 1000);
        orgq_note_data_account_duty(&mut s, "org_01", "da-a", "peer-a2", "mobile", 2000);
        orgq_note_data_account_duty(&mut s, "org_02", "da-x", "peer-x", "pc", 3000);
        let obs = orgq_da_duty_observations(&s, "org_01");
        assert_eq!(obs.len(), 2);
        assert!(obs.contains(&("da-a".to_string(), "peer-a1".to_string(), "pc".to_string(), 1000)));
        assert!(obs.contains(&("da-a".to_string(), "peer-a2".to_string(), "mobile".to_string(), 2000)));
        assert_eq!(orgq_da_duty_observations(&s, "org_02").len(), 1);
        // 覆盖写（同设备再次履职刷新时刻）
        orgq_note_data_account_duty(&mut s, "org_01", "da-a", "peer-a1", "pc", 5000);
        let obs = orgq_da_duty_observations(&s, "org_01");
        assert_eq!(obs.len(), 2);
        assert!(obs.contains(&("da-a".to_string(), "peer-a1".to_string(), "pc".to_string(), 5000)));
    }

    #[test]
    fn orgq_online_directory_mark_and_list() {
        let mut s = crate::storage::MemoryStorage::new();
        orgq_mark_data_account_online(&mut s, "org_01", "da-a", 1000);
        orgq_mark_data_account_online(&mut s, "org_01", "da-b", 2000);
        orgq_mark_data_account_online(&mut s, "org_02", "da-x", 3000);
        let listed = orgq_online_data_accounts(&s, "org_01");
        assert_eq!(listed.len(), 2);
        assert!(listed.contains(&("da-a".to_string(), 1000)));
        assert!(listed.contains(&("da-b".to_string(), 2000)));
        let other = orgq_online_data_accounts(&s, "org_02");
        assert_eq!(other, vec![("da-x".to_string(), 3000)]);
    }

    #[test]
    fn select_online_data_account_picks_reachable() {
        let record = org_record(
            &[
                ("me", OrganizationRole::Member),
                ("da-a", OrganizationRole::Member),
                ("da-b", OrganizationRole::Member),
            ],
            &["da-a", "da-b"],
        );
        let online: std::collections::HashSet<String> =
            ["peer-da-b".to_string()].into_iter().collect();
        let none = std::collections::HashSet::new();
        assert_eq!(
            select_online_data_account(&record, &online, "me", &none),
            Some("da-b".to_string())
        );
        assert_eq!(
            select_online_data_account(&record, &std::collections::HashSet::new(), "me", &none),
            None
        );
    }

    #[test]
    fn select_online_data_account_skips_self() {
        let record = org_record(
            &[
                ("me", OrganizationRole::Member),
                ("da-a", OrganizationRole::Member),
            ],
            &["me", "da-a"],
        );
        let online: std::collections::HashSet<String> =
            ["peer-me".to_string(), "peer-da-a".to_string()]
                .into_iter()
                .collect();
        let none = std::collections::HashSet::new();
        assert_eq!(
            select_online_data_account(&record, &online, "me", &none),
            Some("da-a".to_string())
        );
    }

    /// F4：select 避让 degraded 数据账号——有非 degraded 的在线数据账号时优先
    /// 选之；全部在线都 degraded 才退选。
    #[test]
    fn select_online_data_account_avoids_degraded() {
        let record = org_record(
            &[
                ("me", OrganizationRole::Member),
                ("da-a", OrganizationRole::Member),
                ("da-b", OrganizationRole::Member),
            ],
            &["da-a", "da-b"],
        );
        let online: std::collections::HashSet<String> =
            ["peer-da-a".to_string(), "peer-da-b".to_string()]
                .into_iter()
                .collect();
        // da-a degraded，da-b 正常 → 优先选 da-b
        let degraded: std::collections::HashSet<String> =
            ["da-a".to_string()].into_iter().collect();
        assert_eq!(
            select_online_data_account(&record, &online, "me", &degraded),
            Some("da-b".to_string())
        );
        // 全部在线数据账号都 degraded → 退选（全降级才选之）
        let all_degraded: std::collections::HashSet<String> =
            ["da-a".to_string(), "da-b".to_string()]
                .into_iter()
                .collect();
        let picked = select_online_data_account(&record, &online, "me", &all_degraded);
        assert!(picked.is_some(), "全降级仍选一个（宁选 degraded 也不空）");
    }

    #[test]
    fn should_route_orgq_only_for_non_data_account() {
        let record = org_record(
            &[
                ("me", OrganizationRole::Member),
                ("da", OrganizationRole::Admin),
            ],
            &["da"],
        );
        assert!(should_route_orgq(&record, "me"));
        assert!(!should_route_orgq(&record, "da"));
    }

    #[test]
    fn member_orgq_read_plan_local_when_resident() {
        let record = org_record(
            &[
                ("me", OrganizationRole::Member),
                ("da", OrganizationRole::Admin),
            ],
            &["da"],
        );
        let none = std::collections::HashSet::new();
        let plan = member_orgq_read_plan(
            &record,
            "me",
            &std::collections::HashSet::new(),
            true,
            false,
            &none,
        );
        assert_eq!(plan, MemberReadPlan::Local);
    }

    #[test]
    fn member_orgq_read_plan_orgq_when_online_data_account() {
        let record = org_record(
            &[
                ("me", OrganizationRole::Member),
                ("da", OrganizationRole::Admin),
            ],
            &["da"],
        );
        let online: std::collections::HashSet<String> =
            ["peer-da".to_string()].into_iter().collect();
        let none = std::collections::HashSet::new();
        let plan = member_orgq_read_plan(&record, "me", &online, false, false, &none);
        assert_eq!(
            plan,
            MemberReadPlan::Orgq {
                target_root_id: "da".to_string()
            }
        );
    }

    #[test]
    fn member_orgq_read_plan_offline_reports_cache() {
        let record = org_record(
            &[
                ("me", OrganizationRole::Member),
                ("da", OrganizationRole::Admin),
            ],
            &["da"],
        );
        let none = std::collections::HashSet::new();
        let plan = member_orgq_read_plan(
            &record,
            "me",
            &std::collections::HashSet::new(),
            false,
            true,
            &none,
        );
        assert_eq!(plan, MemberReadPlan::Offline { has_cache: true });
        let plan2 = member_orgq_read_plan(
            &record,
            "me",
            &std::collections::HashSet::new(),
            false,
            false,
            &none,
        );
        assert_eq!(plan2, MemberReadPlan::Offline { has_cache: false });
    }
}

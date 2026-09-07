//! org:meta（org:structure whole 记录）在 orgsync 面的结构化合并（F1，
//! org-meta-lww-fix §2）：并发合入时的成员级确定性合并（LWW-map 语义），
//! 替代 whole-record LWW 整值覆盖（并发写丢更新：后写方把对方已可见的
//! accessKey/nodeInfo 整份抹掉，O2 联调实测）。
//!
//! 合并函数的三条结构不变量（违反即双端永久互推风暴）：
//! - **确定性**：merge(A, B) == merge(B, A)——所有选取依赖「记录秩」
//!   （与方向无关的全序），集合输出显式归一排序（members 经
//!   [`sort_members`]、extra 按键名字典序插入）；
//! - **可交换幂等**：合并结果再与任一输入合并 = 自身；
//! - **秩支配**：调用方落库时 vv 取 `merge_version_vectors`（合并 vv 支配
//!   两个输入），值/vv 不脱节（见 org-meta-lww-fix §1.1 末）。
//!
//! 与 legacy 快照平面（`snapshot.rs::merge_organization_sync_snapshot`）的关系：
//! 两平面在「members 按 rootId 并集 + accessKey 写一次保留」上同口径；残余
//! 差异（本人字段组选取粒度）随 legacy 退役自然消失。

use std::collections::BTreeMap;

use crate::org::types::{
    OrganizationDeviceSet, OrganizationMember, OrganizationRecord, sort_members,
};

/// 记录秩：两侧版本的全序（与方向无关）——`(updated_at, 记录 canonical 字节)`
/// 字典序，大者秩高。同 updated_at 时以字节序兜底，保证两端各自计算结果一致。
fn record_rank(record: &OrganizationRecord) -> (i64, String) {
    (
        record.updated_at,
        serde_json::to_string(record).unwrap_or_default(),
    )
}

/// orgsync 面 org:meta 并发合入的结构化合并：members 按 rootId 取并集，
/// 逐成员分字段组按「记录秩」选取；accessKey 写一次保留。确定性
/// （与 local/remote 方向无关）、幂等。
///
/// 仅在 vv 判 Concurrent 时由调用方启用（`handle_orgsync_data` 分流）；
/// Remote 整值覆盖、Local/Equal 不写的快路径不受影响。
pub fn merge_org_meta_record(
    existing: &OrganizationRecord,
    incoming: &OrganizationRecord,
) -> OrganizationRecord {
    let (high, low) = if record_rank(existing) >= record_rank(incoming) {
        (existing, incoming)
    } else {
        (incoming, existing)
    };

    // members 并集：BTreeMap 归一遍历序（方向无关），输出再经 sort_members
    // （稳定排序，(role, joinedAt) 并列时保持 rootId 字典序——仍是确定的）。
    let low_by_root: BTreeMap<&str, &OrganizationMember> = low
        .members
        .iter()
        .map(|m| (m.root_id.as_str(), m))
        .collect();
    let high_by_root: BTreeMap<&str, &OrganizationMember> = high
        .members
        .iter()
        .map(|m| (m.root_id.as_str(), m))
        .collect();
    let mut merged_members: Vec<OrganizationMember> = Vec::new();
    // 键集取并集去重后迭代（两 map 的 keys() 直接 chain 会把双侧共有成员
    // 推入两次——成员表翻倍）
    let all_roots: std::collections::BTreeSet<&str> = low_by_root
        .keys()
        .chain(high_by_root.keys())
        .copied()
        .collect();
    for root_id in all_roots {
        let member = match (low_by_root.get(root_id), high_by_root.get(root_id)) {
            (Some(low_m), Some(high_m)) => merge_member(low_m, high_m),
            // 仅一侧存在的成员原样保留（低秩侧优先取，高秩侧兜底）
            (Some(m), None) | (None, Some(m)) => (*m).clone(),
            (None, None) => continue,
        };
        merged_members.push(member);
    }

    // record 级 extra：并集（冲突键取秩高一侧），按键名字典序插入
    // （serde_json preserve_order 下插入序 = 线形键序，显式归一保确定性）。
    let merged_extra = merge_extra_maps(&low.extra, &high.extra);

    OrganizationRecord {
        // summary 字段（name/description/avatar/createdBy/gateways/
        // dataAccounts/orgAddress/isPublic/sync 等）取秩高一侧——管理员低频写，
        // 竞争罕见，近似 LWW
        name: high.name.clone(),
        description: high.description.clone(),
        avatar: high.avatar.clone(),
        base_plugin_domain: high.base_plugin_domain.clone(),
        created_at: high.created_at,
        created_by: high.created_by.clone(),
        members: sort_members(&merged_members),
        sync: high.sync.clone(),
        gateways: high.gateways.clone(),
        data_accounts: high.data_accounts.clone(),
        org_address: high.org_address.clone(),
        is_public: high.is_public,
        // 域类型（org-genesis §3.1）：summary 字段组，取秩高一侧。
        domain_type: high.domain_type,
        extra: merged_extra,
        // org_id 两侧相同（同键合入），updated_at 取大
        org_id: high.org_id.clone(),
        updated_at: high.updated_at.max(low.updated_at),
    }
}

/// 成员级合并（阶段四A P1：F1 的成员条目部分独立成函数，降级复用——
/// whole 合并退役到 summary 字段组后，成员条目并发由本函数裁决）。
///
/// 秩 = `(pmeta.ts, canonical 字节)` 字典序（成员条目自身无 updatedAt
/// 字段，秩由调用方从该条目的 pmeta 给），大者秩高；accessKey 写一次 +
/// nodeInfo 按 deviceUid 并集 + extra 并集（与 `merge_member` 同口径）。
pub fn merge_member_record(
    existing: &OrganizationMember,
    incoming: &OrganizationMember,
    existing_rank: &(i64, String),
    incoming_rank: &(i64, String),
) -> OrganizationMember {
    let (high, low) = if existing_rank >= incoming_rank {
        (existing, incoming)
    } else {
        (incoming, existing)
    };
    merge_member(low, high)
}

/// 逐成员分字段组合并（`low`/`high` 为同 rootId 成员在两侧记录中的版本，
/// 按所属记录的秩定高低——记录秩是整份记录的属性，逐成员沿用）。
fn merge_member(low: &OrganizationMember, high: &OrganizationMember) -> OrganizationMember {
    // accessKey：写一次语义——任一侧 Some 即采用（accessKey 由
    // `org-access:{orgId}` 域从 seed 确定性派生，同账号恒同值，合法变更
    // 不存在「两个不同真值」；与 legacy 快照守卫 snapshot.rs:399-402 同口径）。
    let access_key = match (&low.access_key, &high.access_key) {
        (Some(l), Some(h)) => {
            if l != h {
                // 异常态（派生确定性下不应发生）：按记录秩选取 + WARN
                log::warn!(
                    "[ORG-META-MERGE] divergent accessKey for member {} — taking higher-rank side",
                    &high.root_id[..std::cmp::min(16, high.root_id.len())]
                );
            }
            Some(h.clone())
        }
        (Some(l), None) => Some(l.clone()),
        (None, Some(h)) => Some(h.clone()),
        (None, None) => None,
    };
    // nodeInfo 端点集：按 deviceUid 取并集（O1 端点聚合语义；同 deviceUid
    // 冲突由秩高一侧覆盖——低秩侧端点先入集，高秩侧 upsert 收尾）。
    let node_info = merge_node_info(low.node_info.as_ref(), high.node_info.as_ref());
    OrganizationMember {
        root_id: high.root_id.clone(),
        // 管理员字段组（role/joinedAt/addedBy）：取秩高一侧
        role: high.role,
        joined_at: high.joined_at,
        added_by: high.added_by.clone(),
        node_info,
        // 本人字段组（nickname/avatar/signature/gender/region/
        // usePersonalIdentity）：取秩高一侧——并发改同一成员的本人字段只有
        // 「同账号多设备」一种正当场景，秩高者≈后写者，语义近似 LWW
        nickname: high.nickname.clone(),
        avatar: high.avatar.clone(),
        signature: high.signature.clone(),
        gender: high.gender.clone(),
        region: high.region.clone(),
        use_personal_identity: high.use_personal_identity,
        access_key,
        // 成员种类/组织绑定（org-genesis §3.2）：kind 随角色组取秩高一侧；
        // orgBinding 随本人字段组取秩高一侧。
        kind: high.kind,
        org_binding: high.org_binding.clone(),
        // 动态键：并集（冲突键取秩高一侧），字典序插入保确定性
        extra: merge_extra_maps(&low.extra, &high.extra),
    }
}

/// nodeInfo 端点集并集：低秩侧端点先入集（保序），高秩侧逐端点
/// [`OrganizationDeviceSet::upsert`]（同 deviceUid 覆盖/同设备陈旧 peerId
/// 墓碑化等既有端点化规则原样生效）。
fn merge_node_info(
    low: Option<&OrganizationDeviceSet>,
    high: Option<&OrganizationDeviceSet>,
) -> Option<OrganizationDeviceSet> {
    if low.is_none() && high.is_none() {
        return None;
    }
    let mut merged = low.cloned().unwrap_or_default();
    if let Some(high_set) = high {
        for endpoint in &high_set.endpoints {
            merged.upsert(endpoint);
        }
    }
    Some(merged)
}

/// 动态键并集：冲突键取秩高一侧的值；BTreeMap 归一遍历序后按键名字典序
/// 插入（preserve_order 线形键序 = 插入序，两端各自合并产物逐字节一致）。
fn merge_extra_maps(
    low: &serde_json::Map<String, serde_json::Value>,
    high: &serde_json::Map<String, serde_json::Value>,
) -> serde_json::Map<String, serde_json::Value> {
    let union: BTreeMap<&String, &serde_json::Value> = low
        .iter()
        .chain(high.iter())
        // 高秩侧后插覆盖冲突键（BTreeMap::insert 同键覆盖）
        .collect();
    let mut out = serde_json::Map::with_capacity(union.len());
    for (key, value) in union {
        out.insert(key.clone(), value.clone());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::org::types::{OrganizationAccessKey, OrganizationNodeInfo, OrganizationRole};

    fn member(root_id: &str, role: OrganizationRole, joined_at: i64) -> OrganizationMember {
        OrganizationMember {
            root_id: root_id.to_string(),
            role,
            joined_at,
            added_by: "creator".to_string(),
            ..Default::default()
        }
    }

    fn record(members: Vec<OrganizationMember>, updated_at: i64) -> OrganizationRecord {
        OrganizationRecord {
            org_id: "org_0000000000000001".to_string(),
            name: "t".to_string(),
            created_at: 1000,
            created_by: "creator".to_string(),
            updated_at,
            members: sort_members(&members),
            ..Default::default()
        }
    }

    fn access_key(tag: u8) -> OrganizationAccessKey {
        OrganizationAccessKey {
            public_key: format!("pk-{tag}"),
            bind_sig: "bind".to_string(),
        }
    }

    fn endpoint(peer_id: &str, device_uid: Option<&str>) -> OrganizationNodeInfo {
        OrganizationNodeInfo {
            device_uid: device_uid.map(str::to_string),
            peer_id: Some(peer_id.to_string()),
            addresses: Vec::new(),
        }
    }

    /// 靶心（联调 F1 实测序列）：A、B 各自本地发布 accessKey（互不知情），
    /// 并发合入后双方 accessKey 都保留。
    #[test]
    fn concurrent_access_key_publish_both_kept() {
        let mut a_member = member("root-a", OrganizationRole::Admin, 1000);
        a_member.access_key = Some(access_key(1));
        let mut b_member = member("root-b", OrganizationRole::Member, 1000);
        b_member.access_key = Some(access_key(2));
        // A 侧记录：只有自己发布了；B 侧记录：只有 B 发布了（并发）
        let rec_a = record(
            vec![
                a_member.clone(),
                member("root-b", OrganizationRole::Member, 1000),
            ],
            2000,
        );
        let rec_b = record(
            vec![
                member("root-a", OrganizationRole::Admin, 1000),
                b_member.clone(),
            ],
            2001,
        );

        let merged = merge_org_meta_record(&rec_a, &rec_b);
        assert_eq!(
            merged.members.len(),
            2,
            "members 并集去重（共有成员不重复）"
        );
        let ma = merged
            .members
            .iter()
            .find(|m| m.root_id == "root-a")
            .unwrap();
        let mb = merged
            .members
            .iter()
            .find(|m| m.root_id == "root-b")
            .unwrap();
        assert_eq!(ma.access_key, Some(access_key(1)), "A 的 accessKey 保留");
        assert_eq!(mb.access_key, Some(access_key(2)), "B 的 accessKey 保留");
        assert_eq!(merged.updated_at, 2001);
    }

    /// 方向无关性：merge(A, B) == merge(B, A)（含 updated_at 相等时字节序兜底）。
    #[test]
    fn merge_is_direction_independent() {
        let mut m1 = member("root-a", OrganizationRole::Admin, 1000);
        m1.nickname = Some("甲".to_string());
        m1.access_key = Some(access_key(1));
        let mut m2 = member("root-b", OrganizationRole::Member, 1001);
        m2.nickname = Some("乙".to_string());
        // 同 updated_at（字节序兜底路径）
        let rec_a = record(vec![m1.clone(), m2.clone()], 3000);
        let mut m1b = m1.clone();
        m1b.nickname = Some("甲-改".to_string());
        let mut m2b = m2.clone();
        m2b.access_key = Some(access_key(2));
        let rec_b = record(vec![m1b, m2b], 3000);

        let fwd = merge_org_meta_record(&rec_a, &rec_b);
        let rev = merge_org_meta_record(&rec_b, &rec_a);
        assert_eq!(fwd.members.len(), 2, "members 并集去重（共有成员不重复）");
        assert_eq!(
            serde_json::to_string(&fwd).unwrap(),
            serde_json::to_string(&rev).unwrap(),
            "合并结果与方向无关（逐字节）"
        );
        // 两侧各自改过不同成员：两变更都保留
        let ma = fwd.members.iter().find(|m| m.root_id == "root-a").unwrap();
        let mb = fwd.members.iter().find(|m| m.root_id == "root-b").unwrap();
        assert_eq!(mb.access_key, Some(access_key(2)));
        assert!(ma.access_key.is_some());
    }

    /// 不动点/幂等：合并结果与任一输入再合并 = 自身（逐字节）。
    #[test]
    fn merge_is_idempotent_fixed_point() {
        let mut a = member("root-a", OrganizationRole::Admin, 1000);
        a.access_key = Some(access_key(1));
        let mut b = member("root-b", OrganizationRole::Member, 1000);
        b.nickname = Some("乙".to_string());
        let rec_a = record(
            vec![a, member("root-b", OrganizationRole::Member, 1000)],
            2000,
        );
        let rec_b = record(
            vec![member("root-a", OrganizationRole::Admin, 1000), b],
            2001,
        );

        let merged = merge_org_meta_record(&rec_a, &rec_b);
        assert_eq!(
            merged.members.len(),
            2,
            "members 并集去重（共有成员不重复）"
        );
        let again_a = merge_org_meta_record(&merged, &rec_a);
        let again_b = merge_org_meta_record(&merged, &rec_b);
        assert_eq!(
            serde_json::to_string(&merged).unwrap(),
            serde_json::to_string(&again_a).unwrap()
        );
        assert_eq!(
            serde_json::to_string(&merged).unwrap(),
            serde_json::to_string(&again_b).unwrap()
        );
    }

    /// 字段组按记录秩整组选取：秩高一侧的管理员字段组与本人字段组整体生效。
    /// （设计 §4「管理员改 X.role 与 X 本人改 nickname 并发 → 两变更都保留」
    /// 与 §2.1 的记录秩语义不可兼得——记录秩是整份 whole 记录的属性，同成员
    /// 乃至跨成员的并发非并集字段变更只能存活一侧；§5 已记为字段组级已知
    /// 残余。按 §2.1 落地，偏差已回报架构师。）
    #[test]
    fn field_groups_follow_record_rank() {
        // A 侧：X 晋升 Admin（秩高 2002）；B 侧：X 本人改 nickname（2000）
        let rec_a = record(vec![member("root-x", OrganizationRole::Admin, 1000)], 2002);
        let mut x_b = member("root-x", OrganizationRole::Member, 1000);
        x_b.nickname = Some("X 自称".to_string());
        let rec_b = record(vec![x_b], 2000);

        let merged = merge_org_meta_record(&rec_a, &rec_b);
        let x = merged
            .members
            .iter()
            .find(|m| m.root_id == "root-x")
            .unwrap();
        assert_eq!(x.role, OrganizationRole::Admin, "管理员字段组取秩高侧（A）");
        assert_eq!(x.nickname, None, "本人字段组同取秩高侧（字段组级残余，§5）");

        // 反向：B 秩高 → 本人字段组变更生效、role 回退为 B 侧值
        let rec_a2 = record(vec![member("root-x", OrganizationRole::Admin, 1000)], 2000);
        let rec_b2 = record(
            vec![{
                let mut m = member("root-x", OrganizationRole::Member, 1000);
                m.nickname = Some("X 自称".to_string());
                m
            }],
            2002,
        );
        let merged2 = merge_org_meta_record(&rec_a2, &rec_b2);
        let x2 = merged2
            .members
            .iter()
            .find(|m| m.root_id == "root-x")
            .unwrap();
        assert_eq!(
            x2.role,
            OrganizationRole::Member,
            "管理员字段组取秩高侧（B）"
        );
        assert_eq!(
            x2.nickname.as_deref(),
            Some("X 自称"),
            "本人字段组取秩高侧（B）"
        );
    }

    /// 仅一侧存在的成员原样保留；nodeInfo 端点集按 deviceUid 取并集。
    #[test]
    fn member_union_and_endpoint_union() {
        let mut a = member("root-a", OrganizationRole::Admin, 1000);
        a.node_info = Some(OrganizationDeviceSet::from_single(endpoint(
            "peer-a1",
            Some("uid-a"),
        )));
        let mut a2 = member("root-a", OrganizationRole::Admin, 1000);
        a2.node_info = Some(OrganizationDeviceSet::from_single(endpoint(
            "peer-a2",
            Some("uid-a2"),
        )));
        let rec_a = record(vec![a], 2000);
        let rec_b = record(
            vec![a2, member("root-c", OrganizationRole::Member, 1000)],
            2001,
        );

        let merged = merge_org_meta_record(&rec_a, &rec_b);
        assert_eq!(
            merged.members.len(),
            2,
            "并集去重：root-a（双侧共有）+ root-c"
        );
        assert!(
            merged.members.iter().any(|m| m.root_id == "root-c"),
            "仅 B 侧存在的成员保留"
        );
        let ma = merged
            .members
            .iter()
            .find(|m| m.root_id == "root-a")
            .unwrap();
        let set = ma.node_info.as_ref().unwrap();
        assert_eq!(set.len(), 2, "两端点并集（不同 deviceUid 各保留）");
    }

    /// accessKey 异常分叉（两侧 Some 且不同）：确定性选取秩高一侧（两端一致）。
    #[test]
    fn divergent_access_key_deterministic_pick() {
        let mut a = member("root-x", OrganizationRole::Admin, 1000);
        a.access_key = Some(access_key(1));
        let mut b = member("root-x", OrganizationRole::Admin, 1000);
        b.access_key = Some(access_key(2));
        let rec_a = record(vec![a], 2000);
        let rec_b = record(vec![b], 2001);
        let fwd = merge_org_meta_record(&rec_a, &rec_b);
        let rev = merge_org_meta_record(&rec_b, &rec_a);
        let pick_fwd = &fwd.members[0].access_key;
        assert_eq!(pick_fwd, &rev.members[0].access_key, "方向无关确定性选取");
        assert_eq!(pick_fwd, &Some(access_key(2)), "秩高侧（B, 2001）胜出");
    }

    /// extra 动态键：并集 + 冲突取秩高侧 + 输出键序确定。
    #[test]
    fn extra_union_conflict_high_rank_wins() {
        let mut rec_a = record(vec![member("root-a", OrganizationRole::Admin, 1000)], 2000);
        rec_a.extra.insert("k1".to_string(), serde_json::json!("a"));
        rec_a
            .extra
            .insert("shared".to_string(), serde_json::json!("from-a"));
        let mut rec_b = record(vec![member("root-a", OrganizationRole::Admin, 1000)], 2001);
        rec_b.extra.insert("k2".to_string(), serde_json::json!("b"));
        rec_b
            .extra
            .insert("shared".to_string(), serde_json::json!("from-b"));

        let merged = merge_org_meta_record(&rec_a, &rec_b);
        assert_eq!(merged.extra["k1"], serde_json::json!("a"));
        assert_eq!(merged.extra["k2"], serde_json::json!("b"));
        assert_eq!(
            merged.extra["shared"],
            serde_json::json!("from-b"),
            "冲突键取秩高侧"
        );
        let keys: Vec<&String> = merged.extra.keys().collect();
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted, "extra 键序归一（字典序）");
    }

    // ── 阶段四A P1：merge_member_record（条目级并发合并）──────────────────

    /// 条目秩 = (pmeta.ts, canonical 字节)，大者秩高。
    fn entry_rank(ts: i64, m: &OrganizationMember) -> (i64, String) {
        (ts, serde_json::to_string(m).unwrap_or_default())
    }

    /// accessKey 写一次守卫下沉到成员记录合入：本地 Some vs incoming None →
    /// 保留本地（不论秩高低）；两侧 Some 且不同 → 确定性取秩高侧（方向无关）。
    #[test]
    fn member_record_merge_access_key_write_once() {
        let mut local = member("root-x", OrganizationRole::Member, 1000);
        local.access_key = Some(access_key(1));
        let incoming = member("root-x", OrganizationRole::Admin, 1000);
        // incoming 秩更高（ts 大）但无 accessKey → 本地 accessKey 保留，
        // 管理员字段组（role）仍取秩高侧
        let merged = merge_member_record(
            &local,
            &incoming,
            &entry_rank(2000, &local),
            &entry_rank(2001, &incoming),
        );
        assert_eq!(
            merged.access_key,
            Some(access_key(1)),
            "本地 accessKey 保留"
        );
        assert_eq!(merged.role, OrganizationRole::Admin, "role 取秩高侧");
        // 反向同结论（确定性）
        let rev = merge_member_record(
            &incoming,
            &local,
            &entry_rank(2001, &incoming),
            &entry_rank(2000, &local),
        );
        assert_eq!(
            serde_json::to_string(&merged).unwrap(),
            serde_json::to_string(&rev).unwrap(),
            "方向无关（逐字节）"
        );

        // 两侧 Some 且不同（异常分叉）：取秩高侧 + 方向无关
        let mut other = member("root-x", OrganizationRole::Member, 1000);
        other.access_key = Some(access_key(2));
        let m1 = merge_member_record(
            &local,
            &other,
            &entry_rank(2000, &local),
            &entry_rank(2001, &other),
        );
        let m2 = merge_member_record(
            &other,
            &local,
            &entry_rank(2001, &other),
            &entry_rank(2000, &local),
        );
        assert_eq!(m1.access_key, m2.access_key, "分叉选取方向无关");
        assert_eq!(m1.access_key, Some(access_key(2)), "秩高侧胜出");
    }

    /// 条目级合并的并集语义：nodeInfo 端点集按 deviceUid 并集、extra 并集
    /// （冲突取秩高侧）；低秩侧独有的本人字段组字段随字段组整组取舍（与
    /// whole 合并的成员条目部分同口径——字段组按条目秩整组选取）。
    #[test]
    fn member_record_merge_unions_and_rank_pick() {
        let mut a = member("root-x", OrganizationRole::Member, 1000);
        a.node_info = Some(OrganizationDeviceSet::from_single(endpoint(
            "peer-1",
            Some("uid-1"),
        )));
        a.nickname = Some("低秩昵称".to_string());
        a.extra.insert("k-low".to_string(), serde_json::json!("l"));
        let mut b = member("root-x", OrganizationRole::Member, 1000);
        b.node_info = Some(OrganizationDeviceSet::from_single(endpoint(
            "peer-2",
            Some("uid-2"),
        )));
        b.signature = Some("高秩签名".to_string());
        b.extra.insert("k-high".to_string(), serde_json::json!("h"));

        let merged = merge_member_record(&a, &b, &entry_rank(2000, &a), &entry_rank(2001, &b));
        let set = merged.node_info.as_ref().unwrap();
        assert_eq!(set.len(), 2, "nodeInfo 端点并集");
        assert_eq!(merged.extra["k-low"], serde_json::json!("l"), "extra 并集");
        assert_eq!(merged.extra["k-high"], serde_json::json!("h"));
        // 本人字段组整组取秩高侧（B）：B 未改 nickname → None 覆盖低秩侧昵称
        assert_eq!(merged.nickname, None, "本人字段组随秩高侧整组选取");
        assert_eq!(merged.signature.as_deref(), Some("高秩签名"));
    }
}

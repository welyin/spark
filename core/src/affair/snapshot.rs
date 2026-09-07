//! 法定人数快照（wiki/protocol/community/affair.md §9）。
//!
//! 名册承诺纯函数：`rosterHash` / `memberSetHash` 复算。阶梯名册从日志的确定
//! 性推导（含在级天数/衰减）属 C6；本模块只负责快照操作的线形校验与哈希口径。

use serde_json::Value;

use super::actor::is_valid_identity_id;
use crate::evidence::{normalize_object, sha256_hex};

/// 快照 payload（§9 两形态）。
#[derive(Clone, Debug, PartialEq)]
pub enum SnapshotPayload {
    /// 阶梯名册快照：从日志推导至 asOf 操作（含）为止的投票者集合。
    Ladder {
        /// 推导截止操作 opHash。
        as_of: String,
        /// 名册哈希。
        roster_hash: String,
    },
    /// 组织名册快照：指定存证锚点处的成员集。
    OrgRoster {
        /// 组织 id。
        org_id: String,
        /// 成员集哈希（org-signature §3 口径）。
        member_set_hash: String,
        /// 存证锚引用。
        anchor: Value,
    },
}

/// 解析快照 payload。
pub fn parse_snapshot_payload(payload: &Value) -> Result<SnapshotPayload, &'static str> {
    let obj = payload.as_object().ok_or("bad-snapshot-payload")?;
    match obj.get("basis").and_then(Value::as_str) {
        Some("ladder") => {
            let as_of = obj
                .get("asOf")
                .and_then(Value::as_str)
                .filter(|s| is_valid_identity_id(s))
                .ok_or("bad-snapshot-payload")?
                .to_string();
            let roster_hash = obj
                .get("rosterHash")
                .and_then(Value::as_str)
                .filter(|s| is_valid_identity_id(s))
                .ok_or("bad-snapshot-payload")?
                .to_string();
            Ok(SnapshotPayload::Ladder { as_of, roster_hash })
        }
        Some("org-roster") => {
            let org_id = obj
                .get("orgId")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .ok_or("bad-snapshot-payload")?
                .to_string();
            let member_set_hash = obj
                .get("memberSetHash")
                .and_then(Value::as_str)
                .filter(|s| is_valid_identity_id(s))
                .ok_or("bad-snapshot-payload")?
                .to_string();
            let anchor = obj.get("anchor").cloned().ok_or("bad-snapshot-payload")?;
            if !anchor.is_object() {
                return Err("bad-snapshot-payload");
            }
            Ok(SnapshotPayload::OrgRoster {
                org_id,
                member_set_hash,
                anchor,
            })
        }
        _ => Err("bad-snapshot-payload"),
    }
}

/// 阶梯名册哈希（§9）：`sha256hex(normalizeObject(按 identity 字典序排序的
/// 名册数组))`，名册条目 = `{ "identity": "<64hex>" }`（一人一票，不加权）。
/// 输入去重并排序——同一输入集合任何副本算出同一哈希。
pub fn roster_hash(identities: &[String]) -> Result<String, &'static str> {
    let mut sorted: Vec<&String> = identities.iter().collect();
    for id in &sorted {
        if !is_valid_identity_id(id) {
            return Err("bad-roster-identity");
        }
    }
    sorted.sort();
    sorted.dedup();
    let roster: Vec<Value> = sorted
        .into_iter()
        .map(|id| serde_json::json!({ "identity": id }))
        .collect();
    Ok(sha256_hex(&normalize_object(&Value::Array(roster))))
}

/// 组织成员集哈希（org-signature.md §3 口径）：条目只含 identity/role 两键，
/// 按 identity 字典序。输入条目形状为 `{ "identity", "role" }`。
pub fn member_set_hash(members: &[Value]) -> Result<String, &'static str> {
    let mut entries: Vec<(String, String)> = Vec::with_capacity(members.len());
    for member in members {
        let obj = member.as_object().ok_or("bad-member-entry")?;
        let identity = obj
            .get("identity")
            .and_then(Value::as_str)
            .filter(|s| is_valid_identity_id(s))
            .ok_or("bad-member-entry")?;
        let role = obj
            .get("role")
            .and_then(Value::as_str)
            .filter(|s| matches!(*s, "admin" | "member"))
            .ok_or("bad-member-entry")?;
        entries.push((identity.to_string(), role.to_string()));
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    entries.dedup();
    let array: Vec<Value> = entries
        .into_iter()
        .map(|(identity, role)| serde_json::json!({ "identity": identity, "role": role }))
        .collect();
    Ok(sha256_hex(&normalize_object(&Value::Array(array))))
}

/// ladder 快照复算校验：名册内容（随操作携带或本地提供）重算 rosterHash
/// 比对承诺（§9：哈希承诺防编造）。
pub fn verify_ladder_roster(payload: &SnapshotPayload, identities: &[String]) -> bool {
    match payload {
        SnapshotPayload::Ladder {
            roster_hash: committed,
            ..
        } => roster_hash(identities).is_ok_and(|h| &h == committed),
        _ => false,
    }
}

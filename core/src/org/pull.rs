//! org-pull 响应方纯逻辑（对齐 desktop/src/main/p2p/org-pull-sync.ts
//! `handleDirectRequest`，org.md §9.2/§9.3、p2p-messages.md §9.2/§9.3）。
//!
//! 两个入口：
//! - [`handle_pull_list_request`]：列出对请求方可见的组织（先处理 claim →
//!   重读记录 → 成员身份过滤）；
//! - [`handle_pull_org_request`]：返回单个组织的重建快照 + pluginDocs。
//!
//! 与 TS 的对应差异：TS 的 db 异常冒泡到直连 handler 的 catch 后编码为
//! `{ok:false, reason}`；Rust 存储错误经 [`OrgError`] 上抛，由 p2p 层包装
//! 为同样的响应帧（node.rs 的 `handle_org_share_inbound`）。

use serde_json::{Map, Value};

use crate::storage::StorageBackend;

use super::Result;
use super::claim::NodeInfoClaim;
use super::plugin_docs::collect_syncable_plugin_docs;
use super::service::OrganizationService;
use super::snapshot::normalize_incoming_snapshot;
use super::types::{OrganizationRecord, OrganizationSyncVersions};

/// `memberAuthStatus`（org-pull-sync.ts:98-116）：按 rootId 找成员；
/// 成员 nodeInfo 带 peerId 时要求请求方 peerId 一致（防冒领）。
///
/// 返回 `Err(reason)`：`"not-member"` / `"peer-mismatch"`。
pub fn member_auth_status(
    record: &OrganizationRecord,
    requester_root_id: &str,
    requester_peer_id: Option<&str>,
) -> std::result::Result<(), &'static str> {
    let Some(member) = record
        .members
        .iter()
        .find(|m| m.root_id == requester_root_id)
    else {
        return Err("not-member");
    };
    let expected = member
        .node_info
        .as_ref()
        .and_then(|n| n.peer_id.as_deref())
        .map(str::trim)
        .unwrap_or("");
    if expected.is_empty() {
        return Ok(());
    }
    let actual = requester_peer_id.map(str::trim).unwrap_or("");
    if actual.is_empty() || actual != expected {
        return Err("peer-mismatch");
    }
    Ok(())
}

/// 请求方 peerId 解析（org-pull-sync.ts:158-160）：声明值优先，连接层兜底。
fn resolve_requester_peer_id<'a>(
    payload: &'a Value,
    remote_peer_id: Option<&'a str>,
) -> Option<&'a str> {
    payload
        .get("requesterPeerId")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .or(remote_peer_id)
}

/// `org-pull-list` 响应（org-pull-sync.ts:162-198）。
///
/// 顺序要点（此前 review 修过的点）：**仅当 requesterRootId 是本地任一组织
/// 成员时才处理其 nodeInfoClaim**（未认证请求不得触发 claim 验签与落库
/// 扫描）；claim 可能 bump 版本，处理后**重新读取全部组织记录**再生成
/// 响应列表，否则响应里的旧版本会让拉取方误判"本地更新"而把记录回推。
///
/// `current_root_id` 为 `None`（未登录）时跳过 claim 应用（TS 的
/// applyNodeInfoClaim 会抛"未解锁"被 catch，等价效果）。
///
/// 返回 `(响应帧, claim 实际落库的组织 id 列表)`——后者供宿主触发
/// "落库后推送"（service.ts:450）。
pub fn handle_pull_list_request<S: StorageBackend>(
    storage: &mut S,
    payload: &Value,
    current_root_id: Option<&str>,
    remote_peer_id: Option<&str>,
    now_ms: i64,
) -> Result<(Value, Vec<String>)> {
    let requester_root_id = payload
        .get("requesterRootId")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|r| !r.is_empty());
    let Some(requester_root_id) = requester_root_id else {
        return Ok((
            serde_json::json!({
                "ok": false,
                "type": "org-pull-list-response",
                "reason": "missing-requester-root"
            }),
            Vec::new(),
        ));
    };
    let requester_peer_id = resolve_requester_peer_id(payload, remote_peer_id);

    let mut organizations = OrganizationService::read_all_organizations(storage)?;
    let mut applied_orgs: Vec<String> = Vec::new();

    // nodeInfoClaim：requesterRootId 是已知成员才应用；应用后重读记录
    if let Some(claim_value) = payload.get("nodeInfoClaim").filter(|v| !v.is_null())
        && let Some(root_id) = current_root_id
    {
        let is_known_member = organizations
            .iter()
            .any(|r| r.members.iter().any(|m| m.root_id == requester_root_id));
        if is_known_member {
            // claim 解析/校验/落库失败仅告警，不阻断响应（org-pull-sync.ts:177-183）
            let applied = serde_json::from_value::<NodeInfoClaim>(claim_value.clone())
                .ok()
                .map(|claim| {
                    OrganizationService::apply_node_info_claim(
                        storage,
                        &claim,
                        root_id,
                        remote_peer_id,
                        now_ms,
                    )
                });
            if let Some(Ok(ids)) = applied {
                applied_orgs = ids;
                if !applied_orgs.is_empty() {
                    organizations = OrganizationService::read_all_organizations(storage)?;
                }
            }
        }
    }

    let visible: Vec<Value> = organizations
        .iter()
        .filter(|record| member_auth_status(record, requester_root_id, requester_peer_id).is_ok())
        .map(|record| {
            let mut item = Map::new();
            item.insert("orgId".to_string(), Value::from(record.org_id.clone()));
            // TS `sync: record.sync?.versions`：undefined 被 JSON.stringify 丢键
            if let Some(sync) = &record.sync {
                item.insert(
                    "sync".to_string(),
                    serde_json::to_value(sync.versions).unwrap_or(Value::Null),
                );
            }
            Value::Object(item)
        })
        .collect();

    Ok((
        serde_json::json!({
            "ok": true,
            "type": "org-pull-list-response",
            "organizations": visible
        }),
        applied_orgs,
    ))
}

/// `org-pull-org` 响应（org-pull-sync.ts:200-241）。
///
/// - 组织不存在 / 成员校验失败 → `status:"removed"`（**与真删除不可区分**，
///   org.md §9.4，拉取方据此删除本地记录）
/// - 成功 → `normalizeIncomingSnapshot(record)` 重建快照（版本塌缩，spec
///   §13.3：pull 响应与 share 推送线形不同）+ pluginDocs
pub fn handle_pull_org_request<S: StorageBackend>(
    storage: &S,
    payload: &Value,
    remote_peer_id: Option<&str>,
) -> Result<Value> {
    let requester_root_id = payload
        .get("requesterRootId")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|r| !r.is_empty());
    let org_id_raw = payload
        .get("orgId")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let Some(requester_root_id) = requester_root_id else {
        return Ok(serde_json::json!({
            "ok": false,
            "type": "org-pull-org-response",
            "orgId": org_id_raw,
            "reason": "missing-requester-root"
        }));
    };
    let org_id = org_id_raw.trim();
    if org_id.is_empty() {
        return Ok(serde_json::json!({
            "ok": false,
            "type": "org-pull-org-response",
            "orgId": "",
            "reason": "missing-org-id"
        }));
    }
    let requester_peer_id = resolve_requester_peer_id(payload, remote_peer_id);

    let Some(record) = OrganizationService::get_record(storage, org_id)? else {
        return Ok(serde_json::json!({
            "ok": true,
            "type": "org-pull-org-response",
            "orgId": org_id,
            "status": "removed",
            "reason": "org-not-found"
        }));
    };

    if let Err(reason) = member_auth_status(&record, requester_root_id, requester_peer_id) {
        return Ok(serde_json::json!({
            "ok": true,
            "type": "org-pull-org-response",
            "orgId": org_id,
            "status": "removed",
            "reason": reason
        }));
    }

    // TS：snapshot = normalizeIncomingSnapshot(record)——原始记录线形重建
    // （四字段版本塌缩为 updatedAt，spec §13.3）
    let record_value = serde_json::to_value(&record)?;
    let snapshot = normalize_incoming_snapshot(&record_value)?;
    let plugin_docs = collect_syncable_plugin_docs(storage, org_id)?;
    Ok(serde_json::json!({
        "ok": true,
        "type": "org-pull-org-response",
        "orgId": org_id,
        "status": "member",
        "organization": serde_json::to_value(&snapshot)?,
        "pluginDocs": serde_json::to_value(&plugin_docs)?,
    }))
}

/// 拉取侧解析 org-pull-list 响应中的组织版本表（org-pull-sync.ts:321-326）。
///
/// 返回 `orgId → versions`（versions 缺失/畸形按 `None` 处理——TS 的
/// `Map<string, OrganizationSyncVersions | undefined>`）。
pub fn parse_pull_list_organizations(
    response: &Value,
) -> Vec<(String, Option<OrganizationSyncVersions>)> {
    let mut out = Vec::new();
    if response.get("type").and_then(Value::as_str) != Some("org-pull-list-response")
        || response.get("ok").and_then(Value::as_bool) != Some(true)
    {
        return out;
    }
    let Some(items) = response.get("organizations").and_then(Value::as_array) else {
        return out;
    };
    for item in items {
        let Some(org_id) = item.get("orgId").and_then(Value::as_str) else {
            continue;
        };
        let versions = item
            .get("sync")
            .filter(|v| !v.is_null())
            .and_then(|v| serde_json::from_value::<OrganizationSyncVersions>(v.clone()).ok());
        out.push((org_id.to_string(), versions));
    }
    out
}

/// 本地组织版本解析（org-pull-sync.ts:274-276 `resolveLocalVersions`）：
/// `record.sync.versions` 缺失时按重建快照兜底（版本塌缩到 updatedAt）。
pub fn resolve_local_versions(record: &OrganizationRecord) -> OrganizationSyncVersions {
    record
        .sync
        .as_ref()
        .map(|s| s.versions)
        .unwrap_or_else(|| super::snapshot::build_organization_sync_versions_default(record))
}

/// `validate_incoming_share_payload` 的成功提取结果：
/// `(targetRootId, organization, syncId, pluginDocs)`。
pub type ValidatedSharePayload = (String, Value, Option<String>, Vec<Value>);

/// org-share 推送的载荷校验与成员包含判定（org-share-sync.ts:187-224）。
///
/// 提取 `(targetRootId, organization, syncId, pluginDocs)`；`Err(reason)` 为
/// TS 的 console.warn 跳过语义（接收方静默忽略）。
pub fn validate_incoming_share_payload(
    payload: &Value,
    current_root_id: Option<&str>,
) -> std::result::Result<ValidatedSharePayload, &'static str> {
    let target_root_id = payload
        .get("targetRootId")
        .and_then(Value::as_str)
        .unwrap_or("");
    let organization = payload.get("organization").cloned().unwrap_or(Value::Null);
    let org_id = organization
        .get("orgId")
        .and_then(Value::as_str)
        .unwrap_or("");
    if target_root_id.is_empty() || org_id.is_empty() {
        return Err("invalid payload");
    }
    let sync_id = payload
        .get("syncId")
        .and_then(Value::as_str)
        .map(ToString::to_string);

    let current = current_root_id.ok_or("missing identity context")?;
    if current != target_root_id {
        return Err("target mismatch");
    }

    // 成员包含校验：顶层 members 优先，summary.members 防御性兜底
    // （org-share-sync.ts:216）；兜底命中时把 members 提升到顶层，对齐 TS
    // 传给 normalizeIncomingSnapshot 的 `{...organization, members}`
    let members = organization
        .get("members")
        .and_then(Value::as_array)
        .or_else(|| {
            organization
                .get("summary")
                .and_then(|s| s.get("members"))
                .and_then(Value::as_array)
        });
    let contains_current = members.is_some_and(|list| {
        list.iter()
            .any(|m| m.get("rootId").and_then(Value::as_str) == Some(current))
    });
    if !contains_current {
        return Err("current root not found in members");
    }
    let hoisted = if organization
        .get("members")
        .and_then(Value::as_array)
        .is_none()
    {
        members.cloned()
    } else {
        None
    };
    let mut organization = organization;
    if let Some(list) = hoisted {
        organization["members"] = Value::Array(list);
    }

    let plugin_docs = payload
        .get("pluginDocs")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    Ok((
        target_root_id.to_string(),
        organization,
        sync_id,
        plugin_docs,
    ))
}

/// 拉取侧 org-pull-org 响应分类（org-pull-sync.ts 的三分支消费）。
#[derive(Clone, Debug, PartialEq)]
pub enum PullOrgOutcome {
    /// `status:"removed"`（含 not-member/peer-mismatch/org-not-found）。
    Removed,
    /// `status:"member"`：重建快照 + pluginDocs。
    Member {
        /// 组织快照（snapshot 线形）。
        organization: Value,
        /// 随响应捎带的 pluginDocs。
        plugin_docs: Vec<Value>,
    },
    /// 无响应 / ok:false / 形状不符（TS 的 `continue` 跳过语义）。
    Unavailable,
}

/// 解析 org-pull-org 响应（org-pull-sync.ts:351-366、426-455 的判定）。
pub fn classify_pull_org_response(response: Option<&Value>) -> PullOrgOutcome {
    let Some(response) = response else {
        return PullOrgOutcome::Unavailable;
    };
    if response.get("type").and_then(Value::as_str) != Some("org-pull-org-response")
        || response.get("ok").and_then(Value::as_bool) != Some(true)
    {
        return PullOrgOutcome::Unavailable;
    }
    match response.get("status").and_then(Value::as_str) {
        Some("removed") => PullOrgOutcome::Removed,
        Some("member") => {
            let organization = response.get("organization").cloned().unwrap_or(Value::Null);
            if organization.is_null() {
                return PullOrgOutcome::Unavailable;
            }
            let plugin_docs = response
                .get("pluginDocs")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            PullOrgOutcome::Member {
                organization,
                plugin_docs,
            }
        }
        _ => PullOrgOutcome::Unavailable,
    }
}

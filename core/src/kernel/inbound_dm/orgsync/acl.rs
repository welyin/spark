//! acl 合入子模块（从 `orgsync` 拆出，文件长度硬线）：`org:acl:` 授权名单
//! 记录的验签 + whole 合并（O4 §20.7）。零逻辑变化。

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use ed25519_dalek::VerifyingKey;
use serde_json::Value;

use super::super::Result;
use crate::org::OrganizationRecord;
use crate::storage::StorageBackend;

/// O4 §20.7：授权名单（org:acl:）记录合入——acl_verify（签名者 ∈ 变更前本地
/// owners，创世除外；签名用成员表 accessKey 公钥）+ acl_merge（whole，updatedAt
/// 大者胜）。验签/合并失败 → 返回拒绝 reason（保留本地，不落库）。
///
/// 返回 `Ok(None)` = 已处理（合入或本地胜出保留）；`Ok(Some(reason))` = 拒绝，
/// 调用方整批拒收。墓碑（value 为 null）放行走普通路径（acl 不常规删除）。
///
/// **根绑定签名说明**：accessKey 仅本人可写、经 org:structure 集合同步传播
/// （自证归属），bind_sig 的根公钥验签需根公钥（root_id 不可逆），入站纯逻辑
/// 层无法独立复核，故此处以 accessKey 公钥验 acl 签名即达「签名者持有对应
/// 组织身份」的门槛；根绑定在发布路径由内核完成。
/// O1 acl 时间窗（org-orgsync.md §20.7）：合入 acl 的 `updatedAt` 与本地时钟
/// 偏差超窗拒绝——防陈旧/伪造时间戳的 acl 抢占。与 dm 信封新鲜度窗口同口径。
pub(super) const ACL_TS_WINDOW_MS: i64 = 10 * 60_000;

pub(super) fn apply_acl_record_verified<S: StorageBackend>(
    storage: &mut S,
    record: &OrganizationRecord,
    from: &str,
    org_id: &str,
    name: &str,
    version: &str,
    value: &Value,
    meta: &crate::sync::meta::DocMeta,
    now_ms: i64,
) -> Result<Option<String>> {
    if value.is_null() {
        return Ok(None);
    }
    let incoming: crate::sync::orgsync::AclRecord = match serde_json::from_value(value.clone()) {
        Ok(a) => a,
        Err(_) => return Ok(Some("invalid-acl".to_string())),
    };
    let acl_key = crate::sync::orgsync::acl_key(org_id, name, version);
    let current: crate::sync::orgsync::AclRecord = storage
        .get(&acl_key)
        .ok()
        .flatten()
        .and_then(|r| serde_json::from_str(&r).ok())
        .unwrap_or(crate::sync::orgsync::AclRecord {
            owners: Vec::new(),
            readers: Vec::new(),
            epoch: 0,
            updated_at: 0,
            reset_by: None,
            sig: String::new(),
        });
    let col_full = format!("{name}@v{version}");
    // 签名者 from ∈ 变更前本地 owners（创世例外：本地 acl 为空 → 走创世锚）。
    if !current.is_empty() && !current.is_owner(from) {
        return Ok(Some("acl-signer-not-owner".to_string()));
    }
    // O1：创世（本地 acl 为空）——签名者须 == 该集合声明的 declaredBy
    // （声明记录内核已强制 declaredBy=声明者 rootId，此处对齐；防任意成员
    // 抢先自签创世 acl 抢占 owner 权）。
    if current.is_empty() {
        let decl_key = crate::plugindata::org_decl_key(org_id, name, version);
        let declared_by = storage
            .get(&decl_key)
            .ok()
            .flatten()
            .and_then(|raw| {
                serde_json::from_str::<crate::plugindata::CollectionDeclaration>(&raw).ok()
            })
            .and_then(|d| d.declared_by);
        if declared_by.as_deref() != Some(from) {
            return Ok(Some("acl-genesis-signer-not-declared-by".to_string()));
        }
    }
    // 成员表 accessKey 公钥
    let signer_pk = record
        .find_member(from)
        .and_then(|m| m.access_key.as_ref())
        .and_then(|ak| {
            let Ok(bytes) = B64.decode(&ak.public_key) else {
                return None;
            };
            let Ok(arr) = <[u8; 32]>::try_from(bytes.as_slice()) else {
                return None;
            };
            VerifyingKey::from_bytes(&arr).ok()
        });
    let Some(signer_pk) = signer_pk else {
        log::info!("[ORGSYNC] acl signer has no accessKey from={from}");
        return Ok(Some("acl-signer-no-access-key".to_string()));
    };
    if !crate::sync::orgsync::acl_verify(&incoming, org_id, &col_full, &signer_pk) {
        return Ok(Some("acl-bad-signature".to_string()));
    }
    // O1：updatedAt 时间窗——incoming 时间戳与本地时钟偏差超窗拒绝
    // （防陈旧/伪造 acl 抢占；与 dm 信封新鲜度同口径）。用饱和算术防溢出。
    if now_ms.saturating_sub(incoming.updated_at).saturating_abs() > ACL_TS_WINDOW_MS {
        log::info!(
            "[ORGSYNC] acl ts out of window | org={org_id} col={col_full} updatedAt={}",
            incoming.updated_at
        );
        return Ok(Some("acl-ts-out-of-window".to_string()));
    }
    // O1：epoch 单调性——incoming epoch 低于本地当前 epoch（reset 例外已由
    // resetBy 标记显式化）→ 拒绝降级/回退。
    if !current.is_empty() && incoming.epoch < current.epoch && incoming.reset_by.is_none() {
        return Ok(Some("acl-epoch-regress".to_string()));
    }
    // whole 合并：incoming updatedAt 大者胜；本地胜出 → 保持本地（不重写 vv）
    let merged = crate::sync::orgsync::acl_merge(&current, &incoming);
    if merged.updated_at == current.updated_at && merged.sig == current.sig {
        return Ok(None);
    }
    let merged_str = serde_json::to_string(&merged)?;
    crate::sync::apply_personal_remote_no_dlog(storage, &acl_key, &merged_str, meta)?;
    log::info!("[ORGSYNC] acl merged | org={org_id} col={col_full}");
    Ok(None)
}


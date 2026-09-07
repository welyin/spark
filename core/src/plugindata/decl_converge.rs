//! org 集合声明（`org:coll:`）的确定性收敛合入（F2-P2，
//! org-acl-genesis-fix §2.2）：替代旧「保留先见者」规则——同 name@version
//! 内容不同的声明，策略一致时按 `(declaredAt, declaredBy)` 字典序**小者胜**
//! （全序与方向/到达顺序无关，各端收敛到同一记录），败方原位替换（值 +
//! 远端 pmeta 落库，**不 bump 本机分量**——无回声，对齐 sync-storm-fix §5
//! 结构不变量）；策略真实分歧（scope/accounts/merge 等）维持「保留先见者 +
//! 冲突日志」不自动调和（部署错误）。
//!
//! 收敛后的 declaredBy 是创世 acl 锚的判定基准（P3：签名方 == 本地收敛
//! 声明的 declaredBy，锚规则不变）。

use serde_json::Value;

use crate::storage::{BatchOperation, StorageBackend};
use crate::sync::SyncResult;
use crate::sync::meta::DocMeta;

use super::CollectionDeclaration;

/// 策略等价判定（与 `declare()` 幂等路径同一字段集：scope/devices/merge/
/// accounts/confidentiality/space/readPolicy；name/version/orgId 同键隐含一致）。
/// declaredAt/declaredBy/ts 不参与——它们是收敛排序键，不是策略分歧。
fn decl_strategy_eq(a: &CollectionDeclaration, b: &CollectionDeclaration) -> bool {
    a.scope == b.scope
        && a.devices == b.devices
        && a.merge == b.merge
        && a.accounts == b.accounts
        && a.confidentiality == b.confidentiality
        && a.space == b.space
        && a.read_policy == b.read_policy
}

/// orgsync-data 合入声明记录：确定性收敛。返回是否发生落库（写入或替换）。
///
/// - 本地无（或损坏）→ 以远端值 + 远端 meta 落库（远端语义，不 bump）；
/// - 策略冲突 → 保留先见者（不落库）+ 冲突日志；
/// - 同策略 → `(declaredAt, declaredBy)` 小者胜：incoming 小 → 原位替换；
///   相等（同一声明）→ 本地缺 pmeta 时补远端 meta（覆盖折叠面），否则不动；
///   incoming 大 → 保留本地。
pub fn apply_org_decl_convergent<S: StorageBackend>(
    storage: &mut S,
    decl_key: &str,
    value: &Value,
    remote_meta: &DocMeta,
) -> SyncResult<bool> {
    let Ok(incoming) = serde_json::from_value::<CollectionDeclaration>(value.clone()) else {
        log::info!("[ORGSYNC] decl malformed, skipped | key={decl_key}");
        return Ok(false);
    };
    let write_remote = |storage: &mut S| -> SyncResult<bool> {
        storage.batch(vec![
            BatchOperation::put(decl_key, serde_json::to_string(value)?),
            BatchOperation::put(
                crate::sync::personal_meta_key(decl_key),
                serde_json::to_string(remote_meta)?,
            ),
        ])?;
        Ok(true)
    };
    let local_raw = storage.get(decl_key)?;
    let existing = local_raw
        .as_deref()
        .and_then(|raw| serde_json::from_str::<CollectionDeclaration>(raw).ok());
    let Some(existing) = existing else {
        // 本地无声明（或损坏）→ 远端落库
        return write_remote(storage);
    };
    if !decl_strategy_eq(&existing, &incoming) {
        // 策略真实分歧：维持「保留先见者 + 冲突日志」（部署错误不自动调和）
        log::info!(
            "[ORGSYNC] decl conflict kept first-seen | key={decl_key} local declaredBy={:?} incoming declaredBy={:?}",
            existing.declared_by,
            incoming.declared_by
        );
        return Ok(false);
    }
    // 同策略：(declaredAt, declaredBy) 字典序小者胜（Option<rootId> 字典序：
    // None < Some，缺省声明者让位于具名声明者）
    let incoming_rank = (incoming.declared_at, incoming.declared_by.clone());
    let existing_rank = (existing.declared_at, existing.declared_by.clone());
    if incoming_rank < existing_rank {
        log::info!(
            "[ORGSYNC] decl converged (incoming wins) | key={decl_key} declaredBy={:?}",
            incoming.declared_by
        );
        return write_remote(storage);
    }
    if incoming_rank == existing_rank {
        // 同一声明：本地缺 pmeta 时补写远端 meta（不进折叠 = 不可同步的缺口）
        let has_pmeta = crate::sync::get_personal_meta(storage, decl_key)?.is_some();
        if !has_pmeta {
            storage.put(
                &crate::sync::personal_meta_key(decl_key),
                &serde_json::to_string(remote_meta)?,
            )?;
            return Ok(true);
        }
    }
    Ok(false)
}

/// 组织是否声明了 data-accounts 集合（overview K 口径适用性判定，batch2
/// §1.2：只有 data-accounts 集合参与 K=3 记账；纯 all-members 组织无 K）。
pub fn org_has_data_account_collections<S: StorageBackend>(storage: &S, org_id: &str) -> bool {
    let prefix = format!("org:coll:{org_id}:");
    storage
        .scan(&crate::storage::ScanOptions::prefix(&prefix))
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(_, raw)| serde_json::from_str::<CollectionDeclaration>(&raw).ok())
        .any(|decl| decl.accounts == super::Accounts::DataAccounts)
}

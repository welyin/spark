//! 事务本体 org-pinned 保留的组织侧 pin 接受声明入站合入（affair-model §4.2 /
//! A22；与 disclosure 同一模式）：
//!
//! - 发布键域 `org:pin:{orgId}:{affairId}`（org:structure@v1 内建集合，version
//!   单调 LWW）——发布即公示（随 orgsync 全员流动），生效由记录自带
//!   `effectiveAt` 门控（接受 = updatedAt + 24h 公示延迟；撤销即时）；
//! - 入站合入 [`adjudicate_incoming_pin`]：结构校验 → sigSet 必须存在且
//!   subject 绑定本记录 pinHash（防搬签）→ OrgSigSet 五步链（与
//!   trustDecl/policyDoc/disclosure 同一存储闭包背衬）→ version LWW 裁决。
//!
//! 线形与双条件求值（事务声明 ∧ 组织接受 ∧ 已生效）见
//! [`crate::affair::retention`]；本文件只是发布件进入同步键域的闸门。

use serde_json::Value;

use crate::affair::{ORG_PIN_PREFIX, OrgPinRecord, org_pin_key, parse_pin_record, pin_hash};
use crate::credential::OrgSigSetVerifier;
use crate::storage::StorageBackend;

use super::super::Result;
use super::super::sigset::OrgSigSetVerifyContext;
use super::verifiers::{load_policy_chain, sigset_storage_closures};

/// pin 接受声明入站合入裁决（与 disclosure 同三态口径）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PinMerge {
    /// 校验全过且版本更新——调用方落库。
    Accept,
    /// 校验全过但本地版本不旧（version LWW）——保留本地现状。
    KeepCurrent,
    /// 结构/sigSet 绑定/OrgSigSet 五步链任一失败——拒收（fail-closed）。
    Rejected,
}

/// 入站 pin 接受声明合入校验（orgsync-data 入站 `org:pin:` 键分支）。
/// 只裁决不落库——pmeta 记账归调用方（与邻键同口径）。
pub fn adjudicate_incoming_pin<S: StorageBackend>(
    storage: &S,
    org_id: &str,
    affair_id: &str,
    value: &Value,
) -> Result<PinMerge> {
    let Ok(incoming) = parse_pin_record(value) else {
        return Ok(PinMerge::Rejected);
    };
    // 键-文一致（防跨组织/跨事务挂载）。
    let Ok(expected_key) = org_pin_key(org_id, affair_id) else {
        return Ok(PinMerge::Rejected);
    };
    if incoming.key != expected_key {
        return Ok(PinMerge::Rejected);
    }
    // 发布件必须携带组织签名包，且 subject 绑定本记录哈希（防搬签）。
    let Ok(sig_set) =
        serde_json::from_value::<crate::credential::OrgSigSet>(value["sigSet"].clone())
    else {
        return Ok(PinMerge::Rejected);
    };
    if sig_set.subject != pin_hash(value) {
        return Ok(PinMerge::Rejected);
    }
    let policies = load_policy_chain(storage, org_id)?;
    let (anchor_matches, roster_lookup) = sigset_storage_closures(storage, org_id);
    let ctx = OrgSigSetVerifyContext {
        policies: &policies,
        anchor_matches: &anchor_matches,
        roster_lookup: &roster_lookup,
    };
    if !ctx.verify_org_sig_set(&sig_set) {
        return Ok(PinMerge::Rejected);
    }
    // version LWW：高版本胜；同版本保留本地现状（确定性收敛，与 disclosure 同口径）。
    let local: Option<OrgPinRecord> = storage
        .get(&format!("{ORG_PIN_PREFIX}{org_id}:{affair_id}"))?
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .and_then(|value| parse_pin_record(&value).ok());
    match local {
        Some(local) if local.version >= incoming.version => Ok(PinMerge::KeepCurrent),
        _ => Ok(PinMerge::Accept),
    }
}

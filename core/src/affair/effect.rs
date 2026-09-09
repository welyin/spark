//! 决议组织效力钩子（wiki/protocol/community/org-genesis.md §6 事先声明记录
//! + affair.md §6.2 第 4 条；总体方案 §3.2/§7.1「决议组织效力钩子」）。
//!
//! 组织效力 = 三条线形要素同时成立（缺一不可）：
//!
//! 1. **事先声明**：组织以 `org:effectgrant:{orgId}:{affairId}:{scope}` 键存放
//!    效力声明记录（all-members 集合的治理声明，org-genesis §6 线形）；
//! 2. **决议有效**：resolution 操作已过公示期且未达异议阈值（affair §6.2，
//!    求值在门面层用 [`super::resolution::resolution_state`]）；
//! 3. **事先性**：声明的存证锚定时刻**严格早于**决议的锚定时刻——事后追认
//!    无效（防治理偷袭；相等也不算早，须严格 <）。
//!
//! 三线齐备 → 产出待应用事件（[`EffectHookOutcome::Apply`]）：名册/策略变更
//! 的实际应用与回执属门面/编排层，本模块只产出事件、不做应用。
//!
//! 签名集合（`sigSet`）的结构存在性在本层校验；组织签名集合的五步验证链
//! 属 C3（org-signature），本层不重复实现。

use serde_json::{Value, json};

use super::actor::is_valid_identity_id;
use super::resolution::ResolutionState;
use crate::evidence::{normalize_object, sha256_hex};

/// 效力声明记录存储前缀（org-genesis §6 键域，与 `org:tx:` 审计日志分域）。
pub const EFFECT_GRANT_PREFIX: &str = "org:effectgrant:";
/// 效力回执存储前缀（最小可用线形，见 [`EffectReceipt`] 头注）。
pub const EFFECT_RECEIPT_PREFIX: &str = "org:effectrcpt:";
/// evi:resolution 存证条目集合名（domain = orgId，与 effectrcpt / roster
/// 承诺条目同族口径，affair.md §6.3）。
pub const RESOLUTION_ENTRY_COLLECTION: &str = "resolution";
/// evi:resolution 条目 kind 标记（载荷首键，affair.md §6.3 线形）。
pub const RESOLUTION_ENTRY_KIND: &str = "evi:resolution";

/// 组织 id 双形态（org-genesis §2：legacy 16hex / 创世哈希 64hex）。
pub fn is_valid_org_id(org_id: &str) -> bool {
    let Some(hex_part) = org_id.strip_prefix("org_") else {
        return false;
    };
    let len = hex_part.len();
    (len == 16 || len == 64)
        && hex_part
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// scope 安全编码：保留 `[a-zA-Z0-9_:-]`，其余字节按 UTF-8 逐字节 `%XX`
/// 大写十六进制转义（确定性、可逆、键注入安全——`%` 必转义故编码单射；
/// `:` 保留原文以维持 `budget:<标签>` 标签可读性，且 orgId/affairId 均为
/// 无冒号 hex，键段结构不受 scope 内冒号影响）。编码口径已回写
/// org-genesis §6，本函数即其线形落地。
pub fn encode_effect_scope(scope: &str) -> String {
    let mut out = String::with_capacity(scope.len());
    for byte in scope.bytes() {
        let keep = byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b':');
        if keep {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// 效力声明存储键：`org:effectgrant:{orgId}:{affairId}:{scope 安全编码}`。
/// orgId / affairId 形状非法即拒绝（fail-closed，不拼畸形键）。
pub fn effect_grant_key(
    org_id: &str,
    affair_id: &str,
    scope: &str,
) -> Result<String, EffectReject> {
    if !is_valid_org_id(org_id) {
        return Err(EffectReject::BadOrgId);
    }
    if !is_valid_identity_id(affair_id) {
        return Err(EffectReject::BadAffairId);
    }
    if scope.is_empty() || scope.chars().count() > 128 {
        return Err(EffectReject::BadScope);
    }
    Ok(format!(
        "{EFFECT_GRANT_PREFIX}{org_id}:{affair_id}:{}",
        encode_effect_scope(scope)
    ))
}

/// 效力声明记录解析失败原因（reason 字符串稳定）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EffectReject {
    /// 字段缺失或类型错误。
    Malformed,
    /// grantV 非 1。
    BadVersion,
    /// orgId 形状非法（双形态正则）。
    BadOrgId,
    /// affairId 非 64 hex。
    BadAffairId,
    /// scope 空或超长。
    BadScope,
    /// sigSet 缺失或非对象（组织签名集合，org-genesis §6 必填）。
    SigSetRequired,
}

impl EffectReject {
    /// 稳定 reason 字符串。
    pub fn reason(self) -> &'static str {
        match self {
            Self::Malformed => "malformed-effect-grant",
            Self::BadVersion => "bad-version",
            Self::BadOrgId => "bad-org-id",
            Self::BadAffairId => "bad-affair-id",
            Self::BadScope => "bad-scope",
            Self::SigSetRequired => "sig-set-required",
        }
    }
}

/// 效力声明记录（org-genesis §6 线形解析结果）。
#[derive(Clone, Debug, PartialEq)]
pub struct EffectGrant {
    /// 组织 id。
    pub org_id: String,
    /// 引用事务 id。
    pub affair_id: String,
    /// 效力范围：roster | policy | create | budget:<标签>。
    pub scope: String,
    /// 声明签署时刻（毫秒；仅展示，事先性以存证锚定时刻判定）。
    pub declared_at: i64,
    /// 撤销标记：新声明记录 `revoked: true` 覆盖同键（缺省 false）。
    pub revoked: bool,
    /// 存储键（解析时按输入原文复算，防键-文不符）。
    pub key: String,
}

/// 解析效力声明记录（结构校验；sigSet 只查存在性与对象形状，签验属 C3）。
pub fn parse_effect_grant(value: &Value) -> Result<EffectGrant, EffectReject> {
    let obj = value.as_object().ok_or(EffectReject::Malformed)?;
    if obj.get("grantV").and_then(Value::as_u64) != Some(1) {
        return Err(EffectReject::BadVersion);
    }
    let org_id = obj
        .get("orgId")
        .and_then(Value::as_str)
        .ok_or(EffectReject::Malformed)?
        .to_string();
    if !is_valid_org_id(&org_id) {
        return Err(EffectReject::BadOrgId);
    }
    let affair_id = obj
        .get("affairId")
        .and_then(Value::as_str)
        .ok_or(EffectReject::Malformed)?
        .to_string();
    if !is_valid_identity_id(&affair_id) {
        return Err(EffectReject::BadAffairId);
    }
    let scope = obj
        .get("scope")
        .and_then(Value::as_str)
        .ok_or(EffectReject::Malformed)?
        .to_string();
    if scope.is_empty() || scope.chars().count() > 128 {
        return Err(EffectReject::BadScope);
    }
    let declared_at = obj
        .get("declaredAt")
        .and_then(Value::as_i64)
        .ok_or(EffectReject::Malformed)?;
    if !obj.get("sigSet").is_some_and(Value::is_object) {
        return Err(EffectReject::SigSetRequired);
    }
    let revoked = obj.get("revoked").and_then(Value::as_bool).unwrap_or(false);
    let key = effect_grant_key(&org_id, &affair_id, &scope)?;
    Ok(EffectGrant {
        org_id,
        affair_id,
        scope,
        declared_at,
        revoked,
        key,
    })
}

/// 效力回执（编排层消费 [`PendingEffect`] 的留痕；协议未钉死回执线形，本模块
/// 登记最小可用实现）：
///
/// ```json
/// { "receiptV": 1, "orgId": "…", "affairId": "…", "scope": "roster",
///   "resolutionOpHash": "<64hex>", "grantKey": "org:effectgrant:…",
///   "state": "recorded", "recordedAtMs": 1720000000000 }
/// ```
///
/// - 存放 `org:effectrcpt:{orgId}:{affairId}:{scope 安全编码}`（编码同声明键），
///   同键覆盖语义：新生效决议取代旧回执，历史回执在存证链留痕；
/// - `state = "recorded"` = 内核已受理并留痕；**名册/策略内容的实际变更不在
///   内核**（决议 tally 是插件语义，内核只承诺字节，affair §6.1）——内容应用
///   归插件/组织侧，回执即「该决议对该组织此 scope 已生效」的机器可读凭据；
/// - 回执写入由门面层逐条存证锚定（domain = orgId，collection = `effectrcpt`）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EffectReceipt {
    /// 组织 id。
    pub org_id: String,
    /// 引用事务 id。
    pub affair_id: String,
    /// 效力范围。
    pub scope: String,
    /// 决议操作 opHash（决议 id）。
    pub resolution_op_hash: String,
    /// 生效的声明记录存储键。
    pub grant_key: String,
    /// 受理状态（当前唯一合法值 `recorded`）。
    pub state: String,
    /// 受理时刻（编排节点本地毫秒，仅展示；权威留痕 = 存证锚定）。
    pub recorded_at_ms: i64,
    /// 存储键（解析时按输入原文复算，防键-文不符）。
    pub key: String,
}

/// 效力回执存储键：`org:effectrcpt:{orgId}:{affairId}:{scope 安全编码}`。
pub fn effect_receipt_key(
    org_id: &str,
    affair_id: &str,
    scope: &str,
) -> Result<String, EffectReject> {
    if !is_valid_org_id(org_id) {
        return Err(EffectReject::BadOrgId);
    }
    if !is_valid_identity_id(affair_id) {
        return Err(EffectReject::BadAffairId);
    }
    if scope.is_empty() || scope.chars().count() > 128 {
        return Err(EffectReject::BadScope);
    }
    Ok(format!(
        "{EFFECT_RECEIPT_PREFIX}{org_id}:{affair_id}:{}",
        encode_effect_scope(scope)
    ))
}

/// 构造回执记录原文（编排层写入用；`recorded_at_ms` 由调用方注入）。
pub fn build_effect_receipt(event: &PendingEffect, recorded_at_ms: i64) -> Result<Value, EffectReject> {
    // 键形状先过校验（fail-closed，不拼畸形键）
    effect_receipt_key(&event.org_id, &event.affair_id, &event.scope)?;
    Ok(serde_json::json!({
        "receiptV": 1,
        "orgId": event.org_id,
        "affairId": event.affair_id,
        "scope": event.scope,
        "resolutionOpHash": event.resolution_op_hash,
        "grantKey": event.grant_key,
        "state": "recorded",
        "recordedAtMs": recorded_at_ms,
    }))
}

/// 解析效力回执记录（结构校验 + 键-文一致由调用方比对 `receipt.key`）。
pub fn parse_effect_receipt(value: &Value) -> Result<EffectReceipt, EffectReject> {
    let obj = value.as_object().ok_or(EffectReject::Malformed)?;
    if obj.get("receiptV").and_then(Value::as_u64) != Some(1) {
        return Err(EffectReject::BadVersion);
    }
    let org_id = obj
        .get("orgId")
        .and_then(Value::as_str)
        .ok_or(EffectReject::Malformed)?
        .to_string();
    let affair_id = obj
        .get("affairId")
        .and_then(Value::as_str)
        .ok_or(EffectReject::Malformed)?
        .to_string();
    let scope = obj
        .get("scope")
        .and_then(Value::as_str)
        .ok_or(EffectReject::Malformed)?
        .to_string();
    let resolution_op_hash = obj
        .get("resolutionOpHash")
        .and_then(Value::as_str)
        .filter(|s| is_valid_identity_id(s))
        .ok_or(EffectReject::Malformed)?
        .to_string();
    let grant_key = obj
        .get("grantKey")
        .and_then(Value::as_str)
        .filter(|s| s.starts_with(EFFECT_GRANT_PREFIX))
        .ok_or(EffectReject::Malformed)?
        .to_string();
    let state = obj
        .get("state")
        .and_then(Value::as_str)
        .filter(|s| *s == "recorded")
        .ok_or(EffectReject::Malformed)?
        .to_string();
    let recorded_at_ms = obj
        .get("recordedAtMs")
        .and_then(Value::as_i64)
        .ok_or(EffectReject::Malformed)?;
    let key = effect_receipt_key(&org_id, &affair_id, &scope)?;
    Ok(EffectReceipt {
        org_id,
        affair_id,
        scope,
        resolution_op_hash,
        grant_key,
        state,
        recorded_at_ms,
        key,
    })
}

/// 决议结论哈希（affair.md §6.3）：`sha256hex(normalizeObject(决议 §6.1
/// payload))`——本体消亡后凭此哈希 + 签名证明决议存在，持有决议原文时证
/// 内容绑定。
pub fn conclusion_hash(resolution_payload: &Value) -> String {
    sha256_hex(&normalize_object(resolution_payload))
}

/// evi:resolution 条目载荷（affair.md §6.3 线形）。输入全确定性：
/// - `subject` = 决议 id（resolution 操作的 opHash）；
/// - `sig_set` = 决议操作 `actor.orgSig` 原样（kind=org 的组织决议）；
///   kind=person → None → 载荷落 `null`（如实标注：未携带组织签名集合）；
/// - `effective_ts` = 生效判定所依据的存证锚时刻（决议在本副本链上的锚定
///   时刻，§7.2 时间源），由调用方注入——跨副本互异是 §7.1 既定诚实边界，
///   其余四字段全网逐字节一致。
pub fn resolution_entry_payload(
    affair_id: &str,
    resolution_op_hash: &str,
    resolution_payload: &Value,
    sig_set: Option<&Value>,
    effective_ts: i64,
) -> Value {
    json!({
        "kind": RESOLUTION_ENTRY_KIND,
        "affairId": affair_id,
        "subject": resolution_op_hash,
        "conclusionHash": conclusion_hash(resolution_payload),
        "sigSet": sig_set.cloned().unwrap_or(Value::Null),
        "effectiveTs": effective_ts,
    })
}

/// 待应用效力事件（决议生效 + 事先声明匹配 + 事先性的产物；应用与回执归门面层）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingEffect {
    /// 组织 id。
    pub org_id: String,
    /// 引用事务 id。
    pub affair_id: String,
    /// 决议操作 opHash（决议 id，affair §6.1）。
    pub resolution_op_hash: String,
    /// 效力范围。
    pub scope: String,
    /// 生效的声明记录存储键。
    pub grant_key: String,
}

/// 效力钩子判定结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EffectHookOutcome {
    /// 三线齐备：产出待应用事件。
    Apply(PendingEffect),
    /// 无事先声明记录（未声明 → 决议对该组织无效力）。
    NotDeclared,
    /// 最新声明记录为撤销（历史声明保留在存证链，现行效力已撤回）。
    Revoked,
    /// 决议未生效（公示期未满或已打回）。
    ResolutionNotEffective,
    /// 声明无存证锚定：事先性无法证明 → 不产生效力（诚实边界，fail-closed）。
    GrantNotAnchored,
    /// 决议无存证锚定：无法比对 → 不产生效力。
    ResolutionNotAnchored,
    /// 声明锚定不早于决议锚定（事后追认无效）。
    NotPrior,
}

/// 效力钩子判定（纯逻辑；§6.2-4 / org-genesis §6）。`grant` 为该
/// （orgId, affairId, scope）键下的现行声明记录（撤销 = 同键覆盖的
/// `revoked: true` 记录，历史在存证链）；两个锚定时刻由调用方从本副本
/// 已校验存证链注入（§7.2 时间源）。
pub fn evaluate_effect_hook(
    grant: Option<&EffectGrant>,
    grant_anchored_ms: Option<i64>,
    resolution_op_hash: &str,
    resolution_anchored_ms: Option<i64>,
    resolution_state: ResolutionState,
) -> EffectHookOutcome {
    let Some(grant) = grant else {
        return EffectHookOutcome::NotDeclared;
    };
    if grant.revoked {
        return EffectHookOutcome::Revoked;
    }
    if resolution_state != ResolutionState::Effective {
        return EffectHookOutcome::ResolutionNotEffective;
    }
    let Some(grant_anchored_ms) = grant_anchored_ms else {
        return EffectHookOutcome::GrantNotAnchored;
    };
    let Some(resolution_anchored_ms) = resolution_anchored_ms else {
        return EffectHookOutcome::ResolutionNotAnchored;
    };
    if grant_anchored_ms >= resolution_anchored_ms {
        return EffectHookOutcome::NotPrior;
    }
    EffectHookOutcome::Apply(PendingEffect {
        org_id: grant.org_id.clone(),
        affair_id: grant.affair_id.clone(),
        resolution_op_hash: resolution_op_hash.to_string(),
        scope: grant.scope.clone(),
        grant_key: grant.key.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample_resolution_payload() -> Value {
        json!({
            "result": "passed",
            "condition": { "type": "op-count", "opType": "content", "count": 1 },
            "countedOps": ["ab".repeat(32)],
            "rulesHash": "cd".repeat(32),
            "pubPeriod": { "delayMs": 86_400_000i64 },
        })
    }

    /// evi:resolution 条目载荷（affair.md §6.3）：字段语义与确定性——同输入
    /// 逐字节相同；conclusionHash 绑定 payload 内容；sigSet 原样内嵌 / 缺席
    /// 落 null；canonical 可复算（payloadHash 消费面）。
    #[test]
    fn resolution_entry_payload_deterministic_and_binding() {
        let affair_id = "11".repeat(32);
        let res_hash = "22".repeat(32);
        let payload = sample_resolution_payload();
        let sig_set = json!({ "sigSetV": 1, "orgId": format!("org_{}", "ab".repeat(32)),
            "subject": "33".repeat(32), "signatures": [] });

        let with_sig = resolution_entry_payload(&affair_id, &res_hash, &payload, Some(&sig_set), 1_720_000_010_000);
        assert_eq!(with_sig["kind"], json!(RESOLUTION_ENTRY_KIND));
        assert_eq!(with_sig["affairId"], json!(affair_id));
        assert_eq!(with_sig["subject"], json!(res_hash));
        assert_eq!(with_sig["effectiveTs"], json!(1_720_000_010_000i64));
        // sigSet 原样内嵌（逐字节，不改写签名包任何字段）
        assert_eq!(with_sig["sigSet"], sig_set);
        // conclusionHash 绑定 payload：同 payload 同哈希，改一字节即变
        assert_eq!(
            with_sig["conclusionHash"].as_str().unwrap(),
            conclusion_hash(&payload)
        );
        let mut tampered = payload.clone();
        tampered["result"] = json!("rejected");
        assert_ne!(conclusion_hash(&tampered), conclusion_hash(&payload));
        // 确定性：同输入逐字节相同（canonical 一致）
        let again = resolution_entry_payload(&affair_id, &res_hash, &payload, Some(&sig_set), 1_720_000_010_000);
        assert_eq!(normalize_object(&with_sig), normalize_object(&again));
        // kind=person 决议（无 orgSig）→ sigSet 落 null
        let no_sig = resolution_entry_payload(&affair_id, &res_hash, &payload, None, 1_720_000_010_000);
        assert_eq!(no_sig["sigSet"], Value::Null);
        assert_eq!(no_sig["conclusionHash"], with_sig["conclusionHash"]);
    }

    fn grant_json(org_id: &str, affair_id: &str, scope: &str) -> Value {
        json!({
            "grantV": 1,
            "orgId": org_id,
            "affairId": affair_id,
            "scope": scope,
            "declaredAt": 1_720_000_000_000i64,
            "sigSet": { "signatures": [] }
        })
    }

    #[test]
    fn receipt_key_parse_and_build() {
        let org = format!("org_{}", "ab".repeat(32));
        let affair = "cd".repeat(32);
        let event = PendingEffect {
            org_id: org.clone(),
            affair_id: affair.clone(),
            resolution_op_hash: "ef".repeat(32),
            scope: "budget:物业费".to_string(),
            grant_key: effect_grant_key(&org, &affair, "budget:物业费").unwrap(),
        };
        let value = build_effect_receipt(&event, 1_720_000_000_000).unwrap();
        let receipt = parse_effect_receipt(&value).unwrap();
        assert_eq!(
            receipt.key,
            effect_receipt_key(&org, &affair, "budget:物业费").unwrap()
        );
        assert_eq!(receipt.resolution_op_hash, "ef".repeat(32));
        assert_eq!(receipt.state, "recorded");
        // 键与声明键分域
        assert_ne!(receipt.key, receipt.grant_key);
        // 坏版本 / 未知状态 / 坏决议哈希均拒
        let mut bad = value.clone();
        bad["receiptV"] = json!(2);
        assert_eq!(parse_effect_receipt(&bad), Err(EffectReject::BadVersion));
        let mut bad = value.clone();
        bad["state"] = json!("applied");
        assert_eq!(parse_effect_receipt(&bad), Err(EffectReject::Malformed));
        let mut bad = value.clone();
        bad["resolutionOpHash"] = json!("zz");
        assert_eq!(parse_effect_receipt(&bad), Err(EffectReject::Malformed));
    }

    #[test]
    fn grant_key_scope_encoding_and_validation() {
        let org = format!("org_{}", "ab".repeat(32));
        let affair = "cd".repeat(32);
        let key = effect_grant_key(&org, &affair, "budget:物业费").unwrap();
        assert_eq!(
            key,
            format!("org:effectgrant:{org}:{affair}:budget:%E7%89%A9%E4%B8%9A%E8%B4%B9")
        );
        // 解码唯一性：不同 scope 不同键
        assert_ne!(
            effect_grant_key(&org, &affair, "a:b").unwrap(),
            effect_grant_key(&org, &affair, "a%3Ab").unwrap()
        );
        assert!(effect_grant_key("org_xx", &affair, "roster").is_err());
        assert!(effect_grant_key(&org, "zz", "roster").is_err());
        assert!(effect_grant_key(&org, &affair, "").is_err());
    }

    #[test]
    fn parse_grant_structure() {
        let org = format!("org_{}", "ab".repeat(32));
        let affair = "cd".repeat(32);
        let value = grant_json(&org, &affair, "roster");
        let grant = parse_effect_grant(&value).unwrap();
        assert_eq!(grant.org_id, org);
        assert!(!grant.revoked);
        // 缺 sigSet
        let mut no_sig = value.clone();
        no_sig.as_object_mut().unwrap().shift_remove("sigSet");
        assert_eq!(
            parse_effect_grant(&no_sig),
            Err(EffectReject::SigSetRequired)
        );
        // 坏版本
        let mut bad_v = value.clone();
        bad_v["grantV"] = json!(2);
        assert_eq!(parse_effect_grant(&bad_v), Err(EffectReject::BadVersion));
        // 撤销标记
        let mut revoked = value.clone();
        revoked["revoked"] = json!(true);
        assert!(parse_effect_grant(&revoked).unwrap().revoked);
    }

    #[test]
    fn hook_requires_declaration_effectiveness_and_priorness() {
        let org = format!("org_{}", "ab".repeat(32));
        let affair = "cd".repeat(32);
        let grant = parse_effect_grant(&grant_json(&org, &affair, "roster")).unwrap();
        let resolution_hash = "ef".repeat(32);

        // 未声明
        assert_eq!(
            evaluate_effect_hook(
                None,
                None,
                &resolution_hash,
                None,
                ResolutionState::Effective
            ),
            EffectHookOutcome::NotDeclared
        );
        // 已撤销
        let mut revoked_grant = grant.clone();
        revoked_grant.revoked = true;
        assert_eq!(
            evaluate_effect_hook(
                Some(&revoked_grant),
                Some(100),
                &resolution_hash,
                Some(200),
                ResolutionState::Effective
            ),
            EffectHookOutcome::Revoked
        );
        // 决议未生效（公示期未满）
        assert_eq!(
            evaluate_effect_hook(
                Some(&grant),
                Some(100),
                &resolution_hash,
                Some(200),
                ResolutionState::Pending
            ),
            EffectHookOutcome::ResolutionNotEffective
        );
        // 声明未锚定：事先性无法证明
        assert_eq!(
            evaluate_effect_hook(
                Some(&grant),
                None,
                &resolution_hash,
                Some(200),
                ResolutionState::Effective
            ),
            EffectHookOutcome::GrantNotAnchored
        );
        // 决议未锚定
        assert_eq!(
            evaluate_effect_hook(
                Some(&grant),
                Some(100),
                &resolution_hash,
                None,
                ResolutionState::Effective
            ),
            EffectHookOutcome::ResolutionNotAnchored
        );
        // 事后追认（声明锚定晚于决议）与同时刻均无效
        assert_eq!(
            evaluate_effect_hook(
                Some(&grant),
                Some(300),
                &resolution_hash,
                Some(200),
                ResolutionState::Effective
            ),
            EffectHookOutcome::NotPrior
        );
        assert_eq!(
            evaluate_effect_hook(
                Some(&grant),
                Some(200),
                &resolution_hash,
                Some(200),
                ResolutionState::Effective
            ),
            EffectHookOutcome::NotPrior
        );
        // 三线齐备 → 待应用事件
        assert_eq!(
            evaluate_effect_hook(
                Some(&grant),
                Some(100),
                &resolution_hash,
                Some(200),
                ResolutionState::Effective
            ),
            EffectHookOutcome::Apply(PendingEffect {
                org_id: grant.org_id.clone(),
                affair_id: affair.clone(),
                resolution_op_hash: resolution_hash.clone(),
                scope: "roster".to_string(),
                grant_key: grant.key.clone(),
            })
        );
    }
}

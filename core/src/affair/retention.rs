//! 事务本体保留策略的组织侧 pin 接受声明（A22 / affair-model §4.2）。
//!
//! 事务规则文档的 `retention: { policy: "org-pinned", orgs: [...] }` 只是
//! **意向声明**；复制组并集以「事务声明 ∧ 组织接受」双条件生效，防垃圾事务
//! 向任意组织摊派长期副本。组织接受 = `org:pin:{orgId}:{affairId}` 声明记录：
//!
//! ```json
//! { "pinV": 1, "orgId": "org_<…>", "affairId": "<64hex>", "version": 1,
//!   "updatedAt": 1720000000000, "effectiveAt": 1720086400000,
//!   "revoked": false, "sigSet": { /* OrgSigSet */ } }
//! ```
//!
//! - **组织级动作**（OrgSigSet 签名背书，与 org:disclosure 同族）：发布即公示
//!   （`org:pin:` 键域并入 orgsync `org:structure@v1` 内建集合全员流动），
//!   生效由记录自带 `effectiveAt` 门控；
//! - **公示延迟**：接受长期副本义务 = 副本摊派面扩大方向，`effectiveAt =
//!   updatedAt + 24h`；撤销（revoked）= 收窄，即时生效（与 disclosure 的
//!   扩大/收窄双方向口径一致）；
//! - **version 单调 LWW**：同键新版本覆盖旧版本；求值只消费**已生效**
//!   （effectiveAt <= now）且非撤销的最新记录；
//! - 内核只提供字段与复制组语义（双条件满足时 affairsync 复制组 ∪= 该组织
//!   数据节点），不评估「该不该保留」（Q10 拍板：组织自选，非内核强制）。
//!
//! `sigSet` 恒为最后一个字段且剔除出哈希（community 总约签名载荷统一口径）；
//! 签验五步链在入站合入点（`org::service::adjudicate_incoming_pin`）把关。

use serde_json::Value;

use super::actor::is_valid_identity_id;
use super::effect::is_valid_org_id;
use crate::evidence::{normalize_object, sha256_hex};

/// pin 接受声明记录版本（恒 1）。
pub const PIN_V: u32 = 1;
/// 公示延迟（接受 = 摊派面扩大方向）：24h（与 affair DEFAULT_PUB_PERIOD_MS
/// 同值；独立常量避免跨模块语义耦合）。
pub const PIN_PUB_PERIOD_MS: i64 = 24 * 60 * 60 * 1000;

/// 存储键前缀（org:structure@v1 键域，随 orgsync 全员流动 = 公示面）。
pub const ORG_PIN_PREFIX: &str = "org:pin:";

/// pin 接受声明存储键：`org:pin:{orgId}:{affairId}`。
/// orgId / affairId 形状非法即拒绝（fail-closed，不拼畸形键）。
pub fn org_pin_key(org_id: &str, affair_id: &str) -> Result<String, PinReject> {
    if !is_valid_org_id(org_id) {
        return Err(PinReject::BadOrgId);
    }
    if !is_valid_identity_id(affair_id) {
        return Err(PinReject::BadAffairId);
    }
    Ok(format!("{ORG_PIN_PREFIX}{org_id}:{affair_id}"))
}

/// pin 接受声明解析失败原因（reason 字符串稳定）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PinReject {
    /// 字段缺失或类型错误。
    Malformed,
    /// pinV 非 1。
    BadVersion,
    /// orgId 形状非法（双形态）。
    BadOrgId,
    /// affairId 非 64 hex。
    BadAffairId,
    /// version < 1。
    BadGeneration,
    /// effectiveAt < updatedAt。
    BadEffectiveAt,
    /// sigSet 缺失或非对象（组织签名集合必填）。
    SigSetRequired,
}

impl PinReject {
    /// 稳定 reason 字符串。
    pub fn reason(self) -> &'static str {
        match self {
            Self::Malformed => "malformed-pin",
            Self::BadVersion => "bad-version",
            Self::BadOrgId => "bad-org-id",
            Self::BadAffairId => "bad-affair-id",
            Self::BadGeneration => "bad-generation",
            Self::BadEffectiveAt => "bad-effective-at",
            Self::SigSetRequired => "sig-set-required",
        }
    }
}

/// pin 接受声明记录（解析结果）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OrgPinRecord {
    /// 组织 id。
    pub org_id: String,
    /// 被 pin 的事务 id。
    pub affair_id: String,
    /// 声明代际（单调递增，LWW 裁决键；首版 = 1）。
    pub version: u64,
    /// 发布时刻（Unix 毫秒）。
    pub updated_at: i64,
    /// 生效时刻（接受 = updatedAt + 24h 公示延迟；撤销 = updatedAt 即时）。
    pub effective_at: i64,
    /// 撤销标记（缺省 false；撤销记录同键覆盖，收窄即时生效）。
    pub revoked: bool,
    /// 存储键（解析时按输入原文复算，防键-文不符）。
    pub key: String,
}

/// 结构校验（不含 sigSet 存在性——发布路径签名前自检用）。
pub fn validate_pin_fields(value: &Value) -> Result<OrgPinRecord, PinReject> {
    let obj = value.as_object().ok_or(PinReject::Malformed)?;
    if obj.get("pinV").and_then(Value::as_u64) != Some(u64::from(PIN_V)) {
        return Err(PinReject::BadVersion);
    }
    let org_id = obj
        .get("orgId")
        .and_then(Value::as_str)
        .ok_or(PinReject::Malformed)?
        .to_string();
    if !is_valid_org_id(&org_id) {
        return Err(PinReject::BadOrgId);
    }
    let affair_id = obj
        .get("affairId")
        .and_then(Value::as_str)
        .ok_or(PinReject::Malformed)?
        .to_string();
    if !is_valid_identity_id(&affair_id) {
        return Err(PinReject::BadAffairId);
    }
    let version = obj
        .get("version")
        .and_then(Value::as_u64)
        .ok_or(PinReject::Malformed)?;
    if version < 1 {
        return Err(PinReject::BadGeneration);
    }
    let updated_at = obj
        .get("updatedAt")
        .and_then(Value::as_i64)
        .ok_or(PinReject::Malformed)?;
    let effective_at = obj
        .get("effectiveAt")
        .and_then(Value::as_i64)
        .ok_or(PinReject::Malformed)?;
    if effective_at < updated_at {
        return Err(PinReject::BadEffectiveAt);
    }
    let revoked = obj.get("revoked").and_then(Value::as_bool).unwrap_or(false);
    let key = org_pin_key(&org_id, &affair_id)?;
    Ok(OrgPinRecord {
        org_id,
        affair_id,
        version,
        updated_at,
        effective_at,
        revoked,
        key,
    })
}

/// 解析 pin 接受声明记录（结构校验 + sigSet 存在性；签验五步链属入站合入点）。
pub fn parse_pin_record(value: &Value) -> Result<OrgPinRecord, PinReject> {
    let record = validate_pin_fields(value)?;
    if !value.get("sigSet").is_some_and(Value::is_object) {
        return Err(PinReject::SigSetRequired);
    }
    Ok(record)
}

/// 声明哈希（剔除 sigSet 的全部字段 canonical 后 sha256hex；sigSet.subject
/// 绑定值，防搬签）。
pub fn pin_hash(value: &Value) -> String {
    let mut value = value.clone();
    if let Some(obj) = value.as_object_mut() {
        obj.shift_remove("sigSet");
    }
    sha256_hex(&normalize_object(&value))
}

/// 摊派面扩大判定（公示延迟触发条件；disclosure_widening 同族语义）：
/// 接受长期副本义务 = 扩大（首次接受 / 撤销后再接受）；撤销 = 收窄即时；
/// 重复接受（版本刷新）= 持平即时。
pub fn pin_widening(prev: Option<&OrgPinRecord>, next_revoked: bool) -> bool {
    if next_revoked {
        return false;
    }
    match prev {
        None => true,
        Some(prev) => prev.revoked,
    }
}

/// 记录是否现行有效（已生效且非撤销；now 注入，纯函数）。
pub fn pin_effective(record: &OrgPinRecord, now_ms: i64) -> bool {
    !record.revoked && record.effective_at <= now_ms
}

/// 双条件复制组并集求值（affair-model §4.2）：事务声明 ∧ 组织接受 ∧ 已生效。
/// `declared` = 事务规则 retention.orgs（意向）；`pins` = 各组织现行 pin 记录。
/// 返回生效 pin 的组织集合（字典序去重）。
pub fn effective_pin_orgs(
    declared: &[String],
    pins: &[&OrgPinRecord],
    now_ms: i64,
) -> Vec<String> {
    let mut out: Vec<String> = pins
        .iter()
        .filter(|pin| declared.contains(&pin.org_id) && pin_effective(pin, now_ms))
        .map(|pin| pin.org_id.clone())
        .collect();
    out.sort();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const NOW: i64 = 1_720_000_000_000;

    fn org_id(byte: &str) -> String {
        format!("org_{}", byte.repeat(32))
    }

    fn affair_id() -> String {
        "cc".repeat(32)
    }

    fn pin_value(org_id: &str, affair_id: &str) -> Value {
        json!({
            "pinV": 1, "orgId": org_id, "affairId": affair_id,
            "version": 1, "updatedAt": NOW, "effectiveAt": NOW + PIN_PUB_PERIOD_MS,
            "sigSet": { "signatures": [] },
        })
    }

    #[test]
    fn key_shape_fail_closed() {
        assert_eq!(
            org_pin_key(&org_id("aa"), &affair_id()).unwrap(),
            format!("org:pin:{}:{}", org_id("aa"), affair_id())
        );
        assert_eq!(org_pin_key("org_xyz", &affair_id()), Err(PinReject::BadOrgId));
        assert_eq!(org_pin_key(&org_id("aa"), "zz"), Err(PinReject::BadAffairId));
    }

    #[test]
    fn parse_roundtrip_and_key_consistency() {
        let value = pin_value(&org_id("aa"), &affair_id());
        let record = parse_pin_record(&value).unwrap();
        assert_eq!(record.org_id, org_id("aa"));
        assert_eq!(record.affair_id, affair_id());
        assert_eq!(record.version, 1);
        assert!(!record.revoked);
        assert_eq!(record.key, org_pin_key(&org_id("aa"), &affair_id()).unwrap());
    }

    #[test]
    fn parse_rejects_bad_shapes() {
        let good = pin_value(&org_id("aa"), &affair_id());
        let cases: Vec<Value> = vec![
            json!("not-object"),
            json!({ "pinV": 2 }),                            // 版本非法
            json!({ "pinV": 1, "orgId": "org_xyz" }),      // orgId 非法
            json!({ "pinV": 1, "orgId": org_id("aa"), "affairId": "zz" }), // affairId 非法
            json!({ "pinV": 1, "orgId": org_id("aa"), "affairId": affair_id(),
                    "version": 0, "updatedAt": NOW, "effectiveAt": NOW,
                    "sigSet": {} }),                          // version < 1
            json!({ "pinV": 1, "orgId": org_id("aa"), "affairId": affair_id(),
                    "version": 1, "updatedAt": NOW, "effectiveAt": NOW - 1,
                    "sigSet": {} }),                          // effectiveAt < updatedAt
        ];
        for (i, case) in cases.iter().enumerate() {
            assert!(parse_pin_record(case).is_err(), "case {i} 必败");
        }
        // 缺 sigSet → SigSetRequired
        let mut no_sig = good.clone();
        no_sig.as_object_mut().unwrap().shift_remove("sigSet");
        assert_eq!(
            parse_pin_record(&no_sig),
            Err(PinReject::SigSetRequired)
        );
        // 签名前自检不受 sigSet 约束
        assert!(validate_pin_fields(&no_sig).is_ok());
        // revoked 缺省 false
        assert!(!parse_pin_record(&good).unwrap().revoked);
    }

    #[test]
    fn hash_excludes_sig_set_and_is_deterministic() {
        let bare = pin_value(&org_id("aa"), &affair_id());
        let mut signed = bare.clone();
        signed["sigSet"] = json!({ "sigSetV": 1, "subject": "00".repeat(32), "signatures": [] });
        assert_eq!(pin_hash(&bare), pin_hash(&signed), "sigSet 剔除出哈希");
        assert_eq!(pin_hash(&bare).len(), 64);
        assert_eq!(pin_hash(&bare), pin_hash(&bare), "确定性");
    }

    #[test]
    fn widening_truth_table() {
        let accepted = parse_pin_record(&pin_value(&org_id("aa"), &affair_id())).unwrap();
        let mut revoked_value = pin_value(&org_id("aa"), &affair_id());
        revoked_value["revoked"] = json!(true);
        revoked_value["version"] = json!(2);
        revoked_value["effectiveAt"] = json!(NOW);
        let revoked = parse_pin_record(&revoked_value).unwrap();
        // 首次接受 = 扩大；撤销后再接受 = 扩大；重复接受 = 持平；撤销 = 收窄
        assert!(pin_widening(None, false));
        assert!(pin_widening(Some(&revoked), false));
        assert!(!pin_widening(Some(&accepted), false));
        assert!(!pin_widening(Some(&accepted), true));
        assert!(!pin_widening(None, true));
    }

    #[test]
    fn effective_gating() {
        let pending = parse_pin_record(&pin_value(&org_id("aa"), &affair_id())).unwrap();
        assert!(!pin_effective(&pending, NOW), "公示窗内未生效");
        assert!(pin_effective(&pending, NOW + PIN_PUB_PERIOD_MS));
        let mut revoked_value = pin_value(&org_id("aa"), &affair_id());
        revoked_value["revoked"] = json!(true);
        revoked_value["effectiveAt"] = json!(NOW);
        let revoked = parse_pin_record(&revoked_value).unwrap();
        assert!(!pin_effective(&revoked, NOW + PIN_PUB_PERIOD_MS), "撤销即时");
    }

    #[test]
    fn double_condition_union() {
        let declared = vec![org_id("aa"), org_id("bb")];
        let pin_a = parse_pin_record(&pin_value(&org_id("aa"), &affair_id())).unwrap();
        let pin_b_pending = parse_pin_record(&pin_value(&org_id("bb"), &affair_id())).unwrap();
        let pin_c = parse_pin_record(&pin_value(&org_id("cc"), &affair_id())).unwrap();
        let now = NOW + PIN_PUB_PERIOD_MS;
        // aa 声明 ∧ 接受 ∧ 生效 → 入并集；bb 声明但组织未 pin → 不入；
        // cc 接受但事务未声明 → 不入（防摊派双条件）
        let pins: Vec<&OrgPinRecord> = vec![&pin_a, &pin_c];
        assert_eq!(effective_pin_orgs(&declared, &pins, now), vec![org_id("aa")]);
        // bb pin 公示窗内未生效 → 不入
        let pins: Vec<&OrgPinRecord> = vec![&pin_a, &pin_b_pending];
        assert_eq!(
            effective_pin_orgs(&declared, &pins, NOW),
            Vec::<String>::new()
        );
        // 事务未声明任何组织（followers 档）→ 恒空
        assert!(effective_pin_orgs(&[], &pins, now).is_empty());
    }
}

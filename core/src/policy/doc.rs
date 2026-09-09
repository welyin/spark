//! 策略文档线形与哈希（policy §2）：B1 最小声明式规则集 schema。
//!
//! 受众阶梯与档位rank：org-members(0) < representatives(1) < public(2)——
//! 阶梯序只用于静态分析比较（analyze.rs），求值本身是集合成员判定。
//! `sigSet` 恒为最后一个字段且剔除出哈希（community 总约签名载荷统一口径）。

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::error::{PolicyError, Result};
use crate::credential::OrgSigSet;
use crate::evidence::{normalize_object, sha256_hex};

/// 策略文档版本（policy §2：恒 1）。
pub const POLICY_DOC_V: u32 = 1;
/// 当前唯一合法引擎标识（policy §1：B2 Cedar 升级时换值，求值器按 engine 分派）。
pub const ENGINE_B1: &str = "b1";

/// 名册可见性三档（policy §2 roster.tier；产品 §九第一层）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RosterTier {
    /// 仅组织：跨组织只见组织条目，无人员行。
    OrgOnly,
    /// 代表可见：成员组织的代表行对外可见。
    Representatives,
    /// 名册公开：全体人员行对外可见。
    Public,
}

impl std::fmt::Display for RosterTier {
    /// kebab-case 档位名（与序列化线形一致，analyze 说明文案用）。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::OrgOnly => "org-only",
            Self::Representatives => "representatives",
            Self::Public => "public",
        };
        f.write_str(name)
    }
}

/// 受众（policy §3）：字段级掩码的可见范围。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Audience {
    /// 本组织成员（域边界基线已保证其可见，声明即冗余——analyze 检出）。
    OrgMembers,
    /// 本组织成员 + 成员组织代表。
    Representatives,
    /// 任何节点（含无凭证外部）。
    Public,
}

impl std::fmt::Display for Audience {
    /// kebab-case 受众名（与序列化线形一致，analyze 说明文案用）。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::OrgMembers => "org-members",
            Self::Representatives => "representatives",
            Self::Public => "public",
        };
        f.write_str(name)
    }
}

/// 字段级掩码规则（policy §2 roster.fields[]）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldRule {
    /// 名册字段名（非空，≤64）。
    pub field: String,
    /// 该字段的可见受众。
    pub audience: Audience,
}

/// 名册可见性规则（policy §2 roster）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RosterRules {
    /// 名册可见性档位。
    pub tier: RosterTier,
    /// 逐字段掩码；缺省/未声明字段不对外暴露（fail-closed）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<FieldRule>,
}

/// 向上开放矩阵条目（policy §2 upward[]：数据集合 × 上级域）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpwardEntry {
    /// 开放的数据集合名。
    pub collection: String,
    /// 上级共同体域 orgId（双形态）；持该域已验证凭证者可读本集合。
    pub to: String,
}

/// B1 策略文档（policy §2 线形；读授权/可见性策略，区别于 org-genesis §5
/// 的组织签名策略文档）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PolicyDoc {
    /// 恒 1。
    pub policy_v: u32,
    /// 引擎标识（恒 "b1"；升级路径出口见模块头注）。
    pub engine: String,
    /// 策略主体组织（数据属主；orgId 双形态）。
    pub org_id: String,
    /// 名册可见性三档 + 字段级掩码。
    pub roster: RosterRules,
    /// 向上开放矩阵；无条目 = 不向任何上级开放。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub upward: Vec<UpwardEntry>,
    /// 更新时刻（Unix 毫秒；展示/LWW 用，无求值语义）。
    pub updated_at: i64,
    /// 组织签名包（可省，缺省键省略；合入校验归 C3，求值入口不验签——
    /// 文档按 policyDocHash 自认证）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sig_set: Option<OrgSigSet>,
}

/// orgId 双形态（org-genesis §2）：legacy `org_<16hex>` / 创世哈希型 `org_<64hex>`。
/// 统一实现归 affair 模块（全仓单一口径，防双形态判定漂移），此处转调。
pub(crate) fn is_valid_org_id(org_id: &str) -> bool {
    crate::affair::is_valid_org_id(org_id)
}

/// 字段名/集合名形状：非空、≤128、仅 ASCII 标识字符与集合常用符号。
pub(crate) fn is_valid_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b':' | b'@' | b'.'))
}

/// `canonical(剔除 sigSet 的全部字段)`（policy §2 policyDocHash 口径）。
fn canonical_sans_sig_set(doc: &PolicyDoc) -> Result<String> {
    let mut value = serde_json::to_value(doc)?;
    if let Value::Object(map) = &mut value {
        map.shift_remove("sigSet");
    }
    Ok(normalize_object(&value))
}

/// `policyDocHash = sha256hex(normalizeObject(策略文档剔除 sigSet))`（policy §2）。
pub fn policy_doc_hash(doc: &PolicyDoc) -> Result<String> {
    Ok(sha256_hex(&canonical_sans_sig_set(doc)?))
}

/// 结构校验（policy §2/§4 第 1 步）：版本/引擎/orgId 形状/字段名/集合名/to 形状。
pub fn validate_policy_doc(doc: &PolicyDoc) -> Result<()> {
    if doc.policy_v != POLICY_DOC_V {
        return Err(PolicyError::InvalidStructure("policyV must be 1"));
    }
    if doc.engine != ENGINE_B1 {
        return Err(PolicyError::UnsupportedEngine(doc.engine.clone()));
    }
    if !is_valid_org_id(&doc.org_id) {
        return Err(PolicyError::InvalidStructure("orgId dual-form"));
    }
    for rule in &doc.roster.fields {
        if !is_valid_name(&rule.field) {
            return Err(PolicyError::InvalidStructure("field name shape"));
        }
    }
    for entry in &doc.upward {
        if !is_valid_name(&entry.collection) {
            return Err(PolicyError::InvalidStructure("collection name shape"));
        }
        if !is_valid_org_id(&entry.to) {
            return Err(PolicyError::InvalidStructure("upward to orgId dual-form"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_doc() -> PolicyDoc {
        PolicyDoc {
            policy_v: 1,
            engine: ENGINE_B1.to_string(),
            org_id: format!("org_{}", "a".repeat(64)),
            roster: RosterRules {
                tier: RosterTier::Public,
                fields: vec![FieldRule {
                    field: "nickname".to_string(),
                    audience: Audience::Public,
                }],
            },
            upward: vec![UpwardEntry {
                collection: "finance:monthly@v1".to_string(),
                to: format!("org_{}", "b".repeat(64)),
            }],
            updated_at: 1_720_000_000_000,
            sig_set: None,
        }
    }

    #[test]
    fn org_id_dual_form() {
        assert!(is_valid_org_id(&format!("org_{}", "a".repeat(16))));
        assert!(is_valid_org_id(&format!("org_{}", "a".repeat(64))));
        assert!(!is_valid_org_id(&format!("org_{}", "a".repeat(15))));
        assert!(!is_valid_org_id(&format!("org_{}", "A".repeat(64))));
        assert!(!is_valid_org_id(&"a".repeat(64)));
    }

    #[test]
    fn name_shape() {
        assert!(is_valid_name("hoa:ledger@v1.0.0"));
        assert!(is_valid_name("nickname"));
        assert!(!is_valid_name(""));
        assert!(!is_valid_name("姓 名"));
        assert!(!is_valid_name(&"a".repeat(129)));
    }

    #[test]
    fn hash_excludes_sig_set() {
        // 手工在序列化值上注入 sigSet 键，验证哈希剔除口径（反序列化前比对）
        let doc = base_doc();
        let mut injected = serde_json::to_value(&doc).unwrap();
        injected["sigSet"] = Value::Bool(true);
        let mut sans = injected.clone();
        sans.as_object_mut().unwrap().shift_remove("sigSet");
        assert_eq!(
            policy_doc_hash(&doc).unwrap(),
            sha256_hex(&normalize_object(&sans))
        );
        let hash = policy_doc_hash(&doc).unwrap();
        assert_eq!(hash.len(), 64);
        assert!(hash.bytes().all(|b| b.is_ascii_hexdigit()));
    }

    #[test]
    fn validate_rejects_bad_engine_and_shapes() {
        let mut doc = base_doc();
        assert!(validate_policy_doc(&doc).is_ok());

        doc.engine = "cedar".to_string();
        let err = validate_policy_doc(&doc).unwrap_err();
        assert_eq!(err.kind(), "unsupported-engine");
        doc.engine = ENGINE_B1.to_string();

        doc.org_id = "org_xyz".to_string();
        assert_eq!(
            validate_policy_doc(&doc).unwrap_err().kind(),
            "invalid-structure"
        );
        doc.org_id = format!("org_{}", "a".repeat(64));

        doc.roster.fields[0].field = "bad name".to_string();
        assert_eq!(
            validate_policy_doc(&doc).unwrap_err().kind(),
            "invalid-structure"
        );
        doc.roster.fields[0].field = "nickname".to_string();

        doc.upward[0].to = "not-an-org".to_string();
        assert_eq!(
            validate_policy_doc(&doc).unwrap_err().kind(),
            "invalid-structure"
        );
    }

    #[test]
    fn serde_round_trip_kebab_case() {
        let doc = base_doc();
        let value = serde_json::to_value(&doc).unwrap();
        assert_eq!(value["roster"]["tier"], "public");
        assert_eq!(value["roster"]["fields"][0]["audience"], "public");
        assert!(value.get("sigSet").is_none());
        assert!(value.get("upward").is_some());
        let back: PolicyDoc = serde_json::from_value(value).unwrap();
        assert_eq!(back, doc);
    }
}

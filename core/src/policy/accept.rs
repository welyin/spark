//! 准入策略声明（membership §4.5，A17）：`org:accept:{orgId}`。
//!
//! 线形 = `{ acceptV, orgId, acceptCredentials[], version, updatedAt,
//! effectiveAt, sigSet }`——组织声明「接受哪些凭证可免预录入册」的**发布物**
//! （组织级动作，OrgSigSet 签名背书）：
//!
//! - `acceptCredentials: [{ credType, issuerTrust }]`：接受 `credType` 类型、
//!   且签发者信任按 `issuerTrust` 域 `org:verifiers:` 信任声明（credential
//!   §4）判定的凭证；空数组 = 不接受任何免预录入册（缺省最保守形态，存量
//!   组织零声明即全拒——预录-认领路径不受影响）；
//! - **公示延迟**（governance §4.1 准入策略声明已纳入适用范围）：准入面
//!   扩大方向（新增规则对，含首份非空声明）`effectiveAt = updatedAt + 24h`
//!   （发布即公示，生效门控）；收窄（移除规则）或持平即时生效；
//! - 免预录加入验证（org-join §8）只采信已生效（effectiveAt <= now）的
//!   现行记录；入站合入五步链 + version LWW（与 disclosure 同一模式）。
//!
//! `sigSet` 恒为最后一个字段且剔除出哈希（community 总约签名载荷统一口径）。

use serde::{Deserialize, Serialize};

use super::doc::is_valid_org_id;
use super::error::{PolicyError, Result};
use crate::credential::OrgSigSet;
use crate::evidence::{normalize_object, sha256_hex};

/// 声明记录版本（恒 1）。
pub const ACCEPT_POLICY_V: u32 = 1;
/// 公示延迟（扩大方向）：24h（与 DISCLOSURE_PUB_PERIOD_MS 同值；独立常量
/// 避免跨规则族语义耦合——开放声明与准入声明是两条规则族，数值同源治理）。
pub const ACCEPT_POLICY_PUB_PERIOD_MS: i64 = 24 * 60 * 60 * 1000;

/// 存储键前缀（org:structure@v1 键域，随 orgsync 全员流动 = 公示面）。
pub const ACCEPT_POLICY_PREFIX: &str = "org:accept:";

/// 声明存储键 `org:accept:{orgId}`（每组织一条，version LWW）。
pub fn accept_policy_key(org_id: &str) -> String {
    format!("{ACCEPT_POLICY_PREFIX}{org_id}")
}

/// 准入规则：`credType` × `issuerTrust`（签发者信任声明所在域）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcceptCredentialRule {
    /// 接受的凭证类型（credential §2 credType 形状）。
    pub cred_type: String,
    /// 签发者信任声明所在域（凭证 subjectDomain 必须等于它；orgId 双形态）。
    pub issuer_trust: String,
}

/// 准入策略声明记录（membership §4.5 线形，disclosure 同族形态）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcceptPolicyRecord {
    /// 恒 1。
    pub accept_v: u32,
    /// 声明主体组织（与键分量一致；orgId 双形态）。
    pub org_id: String,
    /// 准入规则集（空 = 不接受任何免预录入册）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub accept_credentials: Vec<AcceptCredentialRule>,
    /// 声明代际（单调递增，LWW 裁决键；首版 = 1）。
    pub version: u64,
    /// 发布时刻（Unix 毫秒）。
    pub updated_at: i64,
    /// 生效时刻（扩大 = updatedAt + 24h 公示延迟；收窄/持平 = updatedAt）。
    pub effective_at: i64,
    /// 组织签名包（subject = `accept_policy_hash(本记录)`；入站合入五步链把关）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sig_set: Option<OrgSigSet>,
}

/// 结构校验（fail-closed；未知/非法形态拒绝，不猜着执行）。
pub fn validate_accept_policy(record: &AcceptPolicyRecord) -> Result<()> {
    if record.accept_v != ACCEPT_POLICY_V {
        return Err(PolicyError::InvalidStructure("acceptV must be 1"));
    }
    if !is_valid_org_id(&record.org_id) {
        return Err(PolicyError::InvalidStructure("orgId dual-form"));
    }
    for rule in &record.accept_credentials {
        if !crate::credential::is_valid_cred_type(&rule.cred_type) {
            return Err(PolicyError::InvalidStructure("credType shape"));
        }
        if !is_valid_org_id(&rule.issuer_trust) {
            return Err(PolicyError::InvalidStructure("issuerTrust dual-form"));
        }
    }
    // (credType, issuerTrust) 对重复 = 规则冲突（fail-closed，analyze 同族）
    for (i, rule) in record.accept_credentials.iter().enumerate() {
        if record.accept_credentials[..i].contains(rule) {
            return Err(PolicyError::InvalidStructure("duplicate accept rule"));
        }
    }
    if record.version == 0 {
        return Err(PolicyError::InvalidStructure("version must be >= 1"));
    }
    if record.effective_at < record.updated_at {
        return Err(PolicyError::InvalidStructure("effectiveAt < updatedAt"));
    }
    Ok(())
}

/// 声明哈希（剔除 sigSet 的全部字段 canonical 后 sha256hex；sigSet.subject
/// 绑定值，防搬签）。
pub fn accept_policy_hash(record: &AcceptPolicyRecord) -> Result<String> {
    let mut value = serde_json::to_value(record)?;
    if let Some(obj) = value.as_object_mut() {
        obj.remove("sigSet");
    }
    Ok(sha256_hex(&normalize_object(&value)))
}

/// 准入面扩大判定（公示延迟触发条件）：`next` 存在 `prev` 没有的规则对
/// 即扩大；`prev = None`（首次声明）时任何非空 acceptCredentials 皆为扩大
/// （从无到有即准入面扩大）。收窄（移除规则）与持平不扩大。
pub fn accept_policy_widening(prev: Option<&AcceptPolicyRecord>, next: &AcceptPolicyRecord) -> bool {
    let Some(prev) = prev else {
        return !next.accept_credentials.is_empty();
    };
    next.accept_credentials
        .iter()
        .any(|rule| !prev.accept_credentials.contains(rule))
}

/// 生效门控（免预录验证的消费口径）：只采信已生效（effectiveAt <= now）
/// 的现行记录；无记录/未生效 → None（fail-closed 不接受免预录）。
/// 纯函数、不碰存储/时钟（now 注入）。
pub fn effective_accept_policy(
    record: Option<&AcceptPolicyRecord>,
    now_ms: i64,
) -> Option<&AcceptPolicyRecord> {
    record.filter(|r| r.effective_at <= now_ms)
}

/// 规则匹配：凭证类型与签发者信任域是否被声明接受（org-join §8 免预录
/// 验证的「类型匹配 + 信任域匹配」步）。
pub fn accept_policy_admits(record: &AcceptPolicyRecord, cred_type: &str, subject_domain: &str) -> bool {
    record
        .accept_credentials
        .iter()
        .any(|rule| rule.cred_type == cred_type && rule.issuer_trust == subject_domain)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_720_000_000_000;

    fn org_id() -> String {
        format!("org_{}", "aa".repeat(32))
    }

    fn trust_domain() -> String {
        format!("org_{}", "bb".repeat(32))
    }

    fn rule(cred_type: &str) -> AcceptCredentialRule {
        AcceptCredentialRule {
            cred_type: cred_type.to_string(),
            issuer_trust: trust_domain(),
        }
    }

    fn record() -> AcceptPolicyRecord {
        AcceptPolicyRecord {
            accept_v: ACCEPT_POLICY_V,
            org_id: org_id(),
            accept_credentials: vec![],
            version: 1,
            updated_at: NOW,
            effective_at: NOW,
            sig_set: None,
        }
    }

    // ── 结构校验（fail-closed） ─────────────────────────────────────────

    #[test]
    fn validate_accepts_minimal_and_full() {
        assert!(validate_accept_policy(&record()).is_ok());
        let full = AcceptPolicyRecord {
            accept_credentials: vec![rule("household-owner")],
            effective_at: NOW + ACCEPT_POLICY_PUB_PERIOD_MS,
            ..record()
        };
        assert!(validate_accept_policy(&full).is_ok());
    }

    #[test]
    fn validate_rejects_bad_shape() {
        let cases: Vec<AcceptPolicyRecord> = vec![
            AcceptPolicyRecord {
                accept_v: 2,
                ..record()
            },
            AcceptPolicyRecord {
                org_id: "org_xyz".to_string(),
                ..record()
            },
            AcceptPolicyRecord {
                accept_credentials: vec![AcceptCredentialRule {
                    cred_type: "bad type!".to_string(),
                    ..rule("x")
                }],
                ..record()
            },
            AcceptPolicyRecord {
                accept_credentials: vec![AcceptCredentialRule {
                    issuer_trust: "org_xyz".to_string(),
                    ..rule("x")
                }],
                ..record()
            },
            AcceptPolicyRecord {
                accept_credentials: vec![rule("a"), rule("a")],
                ..record()
            }, // 规则对重复
            AcceptPolicyRecord {
                version: 0,
                ..record()
            },
            AcceptPolicyRecord {
                effective_at: NOW - 1,
                ..record()
            },
        ];
        for (i, c) in cases.iter().enumerate() {
            assert!(validate_accept_policy(c).is_err(), "case {i} 必败");
        }
    }

    // ── 哈希自认证 ──────────────────────────────────────────────────────

    #[test]
    fn hash_excludes_sig_set_and_is_deterministic() {
        let bare = record();
        let signed = AcceptPolicyRecord {
            sig_set: Some(crate::credential::OrgSigSet {
                sig_set_v: 1,
                org_id: org_id(),
                subject: "00".repeat(32),
                policy_hash: "00".repeat(32),
                roster: crate::credential::RosterCommitment {
                    member_set_hash: "00".repeat(32),
                    anchor: crate::credential::RosterAnchor {
                        org_id: org_id(),
                        anchor_root: "00".repeat(32),
                        ts: 0,
                    },
                    snapshot: None,
                },
                signed_at: 0,
                signatures: vec![],
            }),
            ..record()
        };
        let h1 = accept_policy_hash(&bare).unwrap();
        let h2 = accept_policy_hash(&signed).unwrap();
        assert_eq!(h1, h2, "sigSet 剔除出哈希（签名绑定不进入自认证值）");
        assert_eq!(h1.len(), 64);
        assert_eq!(h1, accept_policy_hash(&bare).unwrap(), "确定性");
    }

    // ── 准入面扩大判定（公示延迟触发条件） ──────────────────────────────

    #[test]
    fn widening_truth_table() {
        // 首份声明：空规则不算扩大；任何非空规则集皆扩大（从无到有）
        assert!(!accept_policy_widening(None, &record()));
        assert!(accept_policy_widening(
            None,
            &AcceptPolicyRecord {
                accept_credentials: vec![rule("household-owner")],
                ..record()
            }
        ));

        let prev = AcceptPolicyRecord {
            accept_credentials: vec![rule("household-owner")],
            ..record()
        };
        // 持平不扩大
        assert!(!accept_policy_widening(Some(&prev), &prev));
        // 移除规则 = 收窄即时
        assert!(!accept_policy_widening(Some(&prev), &record()));
        // 新增规则对 = 扩大
        assert!(accept_policy_widening(
            Some(&prev),
            &AcceptPolicyRecord {
                accept_credentials: vec![rule("household-owner"), rule("resident")],
                ..prev.clone()
            }
        ));
        // 同 credType 不同 issuerTrust = 新规则对 = 扩大
        assert!(accept_policy_widening(
            Some(&prev),
            &AcceptPolicyRecord {
                accept_credentials: vec![
                    rule("household-owner"),
                    AcceptCredentialRule {
                        cred_type: "household-owner".to_string(),
                        issuer_trust: format!("org_{}", "cc".repeat(32)),
                    },
                ],
                ..prev.clone()
            }
        ));
    }

    // ── 生效门控与规则匹配 ──────────────────────────────────────────────

    #[test]
    fn effective_gate_and_rule_match() {
        let pending = AcceptPolicyRecord {
            accept_credentials: vec![rule("household-owner")],
            effective_at: NOW + 1, // 公示延迟窗口内
            ..record()
        };
        assert_eq!(
            effective_accept_policy(Some(&pending), NOW),
            None,
            "未生效声明不采信（发布即公示 ≠ 即时生效）"
        );
        let effective = AcceptPolicyRecord {
            effective_at: NOW,
            ..pending.clone()
        };
        let got = effective_accept_policy(Some(&effective), NOW).expect("生效记录采信");
        assert!(accept_policy_admits(got, "household-owner", &trust_domain()));
        assert!(!accept_policy_admits(got, "resident", &trust_domain()), "类型不匹配");
        assert!(
            !accept_policy_admits(got, "household-owner", &format!("org_{}", "cc".repeat(32))),
            "信任域不匹配"
        );
        assert_eq!(effective_accept_policy(None, NOW), None, "无声明 = 不接受");
    }
}

//! 名册开放声明（membership §4.3，A15）：`org:disclosure:{orgId}:{targetDomain}`。
//!
//! 线形 = `{ 开放档位(tier), 字段授权(fields[]), 开放集合(collections[]),
//! 版本(version), updatedAt, effectiveAt, sigSet }`——下级组织对自己名册与
//! 数据集合向上级域的**发布物**（组织级动作，OrgSigSet 签名背书）：
//!
//! - **名册语义与集合开放正交**：tier/fields 管名册视图（档位 + 字段授权），
//!   collections 管数据集合向上开放（B1 upward 矩阵按 targetDomain 并入本
//!   记录——`to` 由键隐含，线形去冗）；
//! - **公示延迟**（本篇裁定）：暴露面扩大方向（档位升 / 新增字段授权 /
//!   字段受众升 / 新增开放集合）`effectiveAt = updatedAt + 24h`（公示 +
//!   延迟生效）；收窄或持平即时生效。防管理员瞬间开放名册造成不可逆暴露，
//!   与 governance「开放面扩大必须公示」同原则；
//! - 求值只消费**已生效**（effectiveAt <= now）记录；无声明 = 仅组织默认档
//!   （最保守，存量组织零声明即全隐）。
//!
//! `sigSet` 恒为最后一个字段且剔除出哈希（community 总约签名载荷统一口径）。

use serde::{Deserialize, Serialize};

use super::doc::{FieldRule, RosterTier, is_valid_name, is_valid_org_id};
use super::error::{PolicyError, Result};
use crate::credential::OrgSigSet;
use crate::evidence::{normalize_object, sha256_hex};

/// 声明记录版本（恒 1）。
pub const DISCLOSURE_V: u32 = 1;
/// 公示延迟（扩大方向）：24h（与 affair DEFAULT_PUB_PERIOD_MS 同值；独立
/// 常量避免跨模块语义耦合——事务公示期与开放声明公示期是两条规则族）。
pub const DISCLOSURE_PUB_PERIOD_MS: i64 = 24 * 60 * 60 * 1000;

/// 存储键前缀（org:structure@v1 键域，随 orgsync 全员流动 = 公示面）。
pub const DISCLOSURE_PREFIX: &str = "org:disclosure:";

/// 声明存储键 `org:disclosure:{orgId}:{targetDomain}`。
pub fn disclosure_key(org_id: &str, target_domain: &str) -> String {
    format!("{DISCLOSURE_PREFIX}{org_id}:{target_domain}")
}

/// 名册开放声明记录（membership §4.3 线形 + collections 向上开放并入）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DisclosureRecord {
    /// 恒 1。
    pub disclosure_v: u32,
    /// 下级组织（名册/数据属主；orgId 双形态）。
    pub org_id: String,
    /// 上级目标域 orgId（键分量；与 orgId 不得相同）。
    pub target_domain: String,
    /// 开放档位（仅组织 / 代表可见 / 名册公开；复用 policy §2 档位线形）。
    pub tier: RosterTier,
    /// 字段授权[]（复用 policy §2 FieldRule 线形；未声明字段不对外）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<FieldRule>,
    /// 向上开放的数据集合名列表（全名 `name@v{version}`；空 = 不开放任何
    /// 数据集合——名册开放不蕴含数据开放）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub collections: Vec<String>,
    /// 声明代际（单调递增，LWW 裁决键；首版 = 1）。
    pub version: u64,
    /// 发布时刻（Unix 毫秒）。
    pub updated_at: i64,
    /// 生效时刻（扩大 = updatedAt + 24h 公示延迟；收窄/持平 = updatedAt）。
    pub effective_at: i64,
    /// 组织签名包（subject = `disclosure_hash(本记录)`；入站合入五步链把关）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sig_set: Option<OrgSigSet>,
}

/// 结构校验（fail-closed；未知/非法形态拒绝，不猜着执行）。
pub fn validate_disclosure(record: &DisclosureRecord) -> Result<()> {
    if record.disclosure_v != DISCLOSURE_V {
        return Err(PolicyError::InvalidStructure("disclosureV must be 1"));
    }
    if !is_valid_org_id(&record.org_id) || !is_valid_org_id(&record.target_domain) {
        return Err(PolicyError::InvalidStructure("orgId/targetDomain shape"));
    }
    if record.org_id == record.target_domain {
        return Err(PolicyError::InvalidStructure("self-disclosure"));
    }
    for rule in &record.fields {
        if !is_valid_name(&rule.field) {
            return Err(PolicyError::InvalidStructure("field name shape"));
        }
    }
    if record.collections.iter().any(|c| !is_valid_name(c)) {
        return Err(PolicyError::InvalidStructure("collections entry shape"));
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
pub fn disclosure_hash(record: &DisclosureRecord) -> Result<String> {
    let mut value = serde_json::to_value(record)?;
    if let Some(obj) = value.as_object_mut() {
        obj.remove("sigSet");
    }
    Ok(sha256_hex(&normalize_object(&value)))
}

/// 暴露面扩大判定（公示延迟触发条件；analyze.rs widening 同族语义）：
/// 档位 rank 升 || 新增字段授权 || 字段受众 rank 升 || 新增开放集合。
/// `prev = None`（首次声明）时：任何非「仅组织 + 空授权 + 空集合」形态皆
/// 为扩大（从无到有即暴露面扩大）。
pub fn disclosure_widening(prev: Option<&DisclosureRecord>, next: &DisclosureRecord) -> bool {
    let Some(prev) = prev else {
        return next.tier != RosterTier::OrgOnly
            || !next.fields.is_empty()
            || !next.collections.is_empty();
    };
    if next.tier > prev.tier {
        return true;
    }
    for rule in &next.fields {
        match prev.fields.iter().find(|p| p.field == rule.field) {
            Some(prev_rule) if rule.audience > prev_rule.audience => return true,
            None => return true, // 新增字段授权（上一版该字段缺省不对外）
            _ => {}
        }
    }
    next.collections
        .iter()
        .any(|c| !prev.collections.contains(c))
}

/// 求值视图（eval_disclosure 输出）：某目标域当前可见的名册档位/字段授权/
/// 开放集合（已生效最新版声明的投影）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DisclosureView {
    /// 名册开放档位（默认 OrgOnly = 无声明/未生效）。
    pub tier: RosterTier,
    /// 字段授权（默认空 = 无字段对外）。
    pub fields: Vec<FieldRule>,
    /// 开放集合（默认空 = 无数据集合开放）。
    pub collections: Vec<String>,
}

impl Default for DisclosureView {
    fn default() -> Self {
        Self {
            tier: RosterTier::OrgOnly,
            fields: Vec::new(),
            collections: Vec::new(),
        }
    }
}

/// 开放声明求值（interpretation §4.2 `eval_disclosure`）：从声明集装配
/// 「对 target_domain 当前可见什么」——取 target_domain 匹配且
/// `effectiveAt <= now` 的最高 version 记录；无生效声明 → 仅组织默认档
/// （fail-closed 最保守）。纯函数、确定性、不碰存储/时钟（now 注入）。
pub fn eval_disclosure(
    disclosures: &[&DisclosureRecord],
    target_domain: &str,
    now_ms: i64,
) -> DisclosureView {
    let effective = disclosures
        .iter()
        .filter(|d| d.target_domain == target_domain && d.effective_at <= now_ms)
        .max_by_key(|d| d.version);
    match effective {
        Some(d) => DisclosureView {
            tier: d.tier,
            fields: d.fields.clone(),
            collections: d.collections.clone(),
        },
        None => DisclosureView::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::doc::Audience;

    const NOW: i64 = 1_720_000_000_000;

    fn org_id() -> String {
        format!("org_{}", "aa".repeat(32))
    }

    fn target() -> String {
        format!("org_{}", "bb".repeat(32))
    }

    fn record() -> DisclosureRecord {
        DisclosureRecord {
            disclosure_v: DISCLOSURE_V,
            org_id: org_id(),
            target_domain: target(),
            tier: RosterTier::OrgOnly,
            fields: vec![],
            collections: vec![],
            version: 1,
            updated_at: NOW,
            effective_at: NOW,
            sig_set: None,
        }
    }

    fn field(name: &str, audience: Audience) -> FieldRule {
        FieldRule {
            field: name.to_string(),
            audience,
        }
    }

    // ── 结构校验（fail-closed） ─────────────────────────────────────────

    #[test]
    fn validate_accepts_minimal_and_full() {
        assert!(validate_disclosure(&record()).is_ok());
        let full = DisclosureRecord {
            tier: RosterTier::Public,
            fields: vec![field("phone", Audience::Representatives)],
            collections: vec!["finance:monthly@v1".to_string()],
            effective_at: NOW + DISCLOSURE_PUB_PERIOD_MS,
            ..record()
        };
        assert!(validate_disclosure(&full).is_ok());
    }

    #[test]
    fn validate_rejects_bad_shape() {
        let cases: Vec<DisclosureRecord> = vec![
            DisclosureRecord {
                disclosure_v: 2,
                ..record()
            },
            DisclosureRecord {
                org_id: "org_xyz".to_string(),
                ..record()
            },
            DisclosureRecord {
                target_domain: target(),
                org_id: target(),
                ..record()
            }, // self-disclosure
            DisclosureRecord {
                fields: vec![field("bad field!", Audience::Public)],
                ..record()
            },
            DisclosureRecord {
                collections: vec!["bad name!".to_string()],
                ..record()
            },
            DisclosureRecord {
                version: 0,
                ..record()
            },
            DisclosureRecord {
                effective_at: NOW - 1,
                ..record()
            },
        ];
        for (i, c) in cases.iter().enumerate() {
            assert!(validate_disclosure(c).is_err(), "case {i} 必败");
        }
    }

    // ── 哈希自认证 ──────────────────────────────────────────────────────

    #[test]
    fn hash_excludes_sig_set_and_is_deterministic() {
        let bare = record();
        let signed = DisclosureRecord {
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
        let h1 = disclosure_hash(&bare).unwrap();
        let h2 = disclosure_hash(&signed).unwrap();
        assert_eq!(h1, h2, "sigSet 剔除出哈希（签名绑定不进入自认证值）");
        assert_eq!(h1.len(), 64);
        assert_eq!(h1, disclosure_hash(&bare).unwrap(), "确定性");
    }

    // ── 暴露面扩大判定（公示延迟触发条件） ──────────────────────────────

    #[test]
    fn widening_truth_table() {
        // 首份声明：全隐形态不算扩大；任何非全隐形态皆扩大（从无到有）
        assert!(!disclosure_widening(None, &record()));
        assert!(!disclosure_widening(
            None,
            &DisclosureRecord {
                effective_at: NOW + DISCLOSURE_PUB_PERIOD_MS,
                ..record()
            } // 全隐但延迟生效字段不左右 widening 判定
        ));
        assert!(disclosure_widening(
            None,
            &DisclosureRecord {
                tier: RosterTier::Representatives,
                ..record()
            }
        ));
        assert!(disclosure_widening(
            None,
            &DisclosureRecord {
                fields: vec![field("phone", Audience::Public)],
                ..record()
            }
        ));
        assert!(disclosure_widening(
            None,
            &DisclosureRecord {
                collections: vec!["c@v1".to_string()],
                ..record()
            }
        ));

        let prev = DisclosureRecord {
            tier: RosterTier::Representatives,
            fields: vec![field("phone", Audience::Representatives)],
            collections: vec!["a@v1".to_string()],
            ..record()
        };
        // 持平不扩大
        assert!(!disclosure_widening(Some(&prev), &prev));
        // 档位升
        assert!(disclosure_widening(
            Some(&prev),
            &DisclosureRecord {
                tier: RosterTier::Public,
                ..prev.clone()
            }
        ));
        // 档位降 = 收窄即时
        assert!(!disclosure_widening(
            Some(&prev),
            &DisclosureRecord {
                tier: RosterTier::OrgOnly,
                ..prev.clone()
            }
        ));
        // 新增字段授权
        assert!(disclosure_widening(
            Some(&prev),
            &DisclosureRecord {
                fields: vec![
                    field("phone", Audience::Representatives),
                    field("email", Audience::Public)
                ],
                ..prev.clone()
            }
        ));
        // 字段受众升
        assert!(disclosure_widening(
            Some(&prev),
            &DisclosureRecord {
                fields: vec![field("phone", Audience::Public)],
                ..prev.clone()
            }
        ));
        // 字段受众降 = 收窄
        assert!(!disclosure_widening(
            Some(&prev),
            &DisclosureRecord {
                fields: vec![field("phone", Audience::OrgMembers)],
                ..prev.clone()
            }
        ));
        // 移除字段授权 = 收窄
        assert!(!disclosure_widening(
            Some(&prev),
            &DisclosureRecord {
                fields: vec![],
                ..prev.clone()
            }
        ));
        // 新增开放集合
        assert!(disclosure_widening(
            Some(&prev),
            &DisclosureRecord {
                collections: vec!["a@v1".to_string(), "b@v1".to_string()],
                ..prev.clone()
            }
        ));
        // 移除开放集合 = 收窄
        assert!(!disclosure_widening(
            Some(&prev),
            &DisclosureRecord {
                collections: vec![],
                ..prev.clone()
            }
        ));
    }

    // ── 求值（eval_disclosure） ─────────────────────────────────────────

    #[test]
    fn eval_defaults_org_only_without_effective_record() {
        // 无声明 → 仅组织默认档（存量组织零声明即全隐）
        assert_eq!(eval_disclosure(&[], &target(), NOW), DisclosureView::default());
        // 公示延迟窗口内（effectiveAt > now）→ 不装配
        let pending = DisclosureRecord {
            tier: RosterTier::Public,
            effective_at: NOW + 1,
            ..record()
        };
        assert_eq!(
            eval_disclosure(&[&pending], &target(), NOW),
            DisclosureView::default(),
            "未生效声明不装配（发布即公示 ≠ 即时生效）"
        );
        // 他域声明与本域无关
        let other = DisclosureRecord {
            target_domain: format!("org_{}", "cc".repeat(32)),
            tier: RosterTier::Public,
            ..record()
        };
        assert_eq!(
            eval_disclosure(&[&other], &target(), NOW),
            DisclosureView::default()
        );
    }

    #[test]
    fn eval_picks_highest_effective_version() {
        let v1 = DisclosureRecord {
            tier: RosterTier::Representatives,
            version: 1,
            ..record()
        };
        let v2 = DisclosureRecord {
            tier: RosterTier::OrgOnly, // v2 收窄回仅组织
            version: 2,
            ..record()
        };
        let view = eval_disclosure(&[&v1, &v2], &target(), NOW);
        assert_eq!(view.tier, RosterTier::OrgOnly, "最高 version 生效版胜出");
        // 生效视图投影字段与集合
        let v3 = DisclosureRecord {
            version: 3,
            tier: RosterTier::Public,
            fields: vec![field("phone", Audience::Public)],
            collections: vec!["c@v1".to_string()],
            ..record()
        };
        let view = eval_disclosure(&[&v1, &v2, &v3], &target(), NOW);
        assert_eq!(view.tier, RosterTier::Public);
        assert_eq!(view.fields, vec![field("phone", Audience::Public)]);
        assert_eq!(view.collections, vec!["c@v1".to_string()]);
    }
}

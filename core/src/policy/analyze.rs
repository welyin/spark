//! 静态分析（policy §5）：保存策略时检测规则冲突与意外扩大暴露面。
//!
//! `analyze(doc, prev)` 是求值器之外的纯函数：`prev` 为上一版策略文档
//! （同 orgId 链，首版传 `None`）。Error 阻断保存（规则冲突/不可达），
//! Warning 需显式确认（暴露面较上一版扩大）；收窄（档位降低、字段/条目
//! 移除）不告警。code 逐字稳定（golden vectors 与跨层上报按此对齐）。

use std::collections::BTreeSet;

use super::doc::{Audience, PolicyDoc, RosterTier, validate_policy_doc};

/// 严重级（policy §5）：Error 阻断保存；Warning 需显式确认。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// 阻断保存。
    Error,
    /// 需显式确认。
    Warning,
}

/// 一条分析结论：级别 + 稳定 code + 人读说明（说明不逐字稳定，code 稳定）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnalysisFinding {
    /// 严重级。
    pub severity: Severity,
    /// 稳定机器可读码（policy §5 表，逐字稳定）。
    pub code: &'static str,
    /// 人读说明（带冲突字段/条目名，便于保存界面展示）。
    pub detail: String,
}

/// 静态分析入口（policy §5）：冲突/不可达为 Error，暴露面扩大为 Warning。
///
/// 结构/引擎校验失败时不做后续检查（规则集本身不可用，`prev` 比较无意义）。
pub fn analyze(doc: &PolicyDoc, prev: Option<&PolicyDoc>) -> Vec<AnalysisFinding> {
    if let Err(err) = validate_policy_doc(doc) {
        let code = match err.kind() {
            "unsupported-engine" => "unsupported-engine",
            _ => "invalid-structure",
        };
        return vec![finding(Severity::Error, code, err.to_string())];
    }
    let mut findings = conflict_checks(doc);
    if let Some(prev) = prev {
        widening_checks(doc, prev, &mut findings);
    }
    findings
}

/// 本版文档自身检查：重复声明、无效规则、不可达规则（§5 Error 组）。
fn conflict_checks(doc: &PolicyDoc) -> Vec<AnalysisFinding> {
    let mut findings = Vec::new();

    let mut seen_fields = BTreeSet::new();
    for rule in &doc.roster.fields {
        if !seen_fields.insert(rule.field.as_str()) {
            findings.push(finding(
                Severity::Error,
                "duplicate-field-rule",
                format!("field '{}' declared more than once", rule.field),
            ));
        }
        if rule.audience == Audience::OrgMembers {
            // 域边界基线已保证成员可见，org-members 受众规则无效
            findings.push(finding(
                Severity::Error,
                "field-rule-redundant",
                format!(
                    "field '{}' audience org-members is covered by the domain baseline",
                    rule.field
                ),
            ));
        }
    }

    let mut seen_entries = BTreeSet::new();
    for entry in &doc.upward {
        let key = (entry.collection.as_str(), entry.to.as_str());
        if !seen_entries.insert(key) {
            findings.push(finding(
                Severity::Error,
                "duplicate-upward-entry",
                format!(
                    "upward entry ({}, {}) declared more than once",
                    entry.collection, entry.to
                ),
            ));
        }
    }

    if doc.roster.tier == RosterTier::OrgOnly && !doc.roster.fields.is_empty() {
        // 行对外全隐时一切字段规则不可达（§4 第 4 条字段判定不回看档位的兜底）
        findings.push(finding(
            Severity::Error,
            "field-rule-shadowed",
            format!(
                "{} field rule(s) unreachable under roster tier org-only",
                doc.roster.fields.len()
            ),
        ));
    }
    findings
}

/// 与上一版比较：暴露面扩大告警（§5 Warning 组）；收窄不告警。
fn widening_checks(doc: &PolicyDoc, prev: &PolicyDoc, findings: &mut Vec<AnalysisFinding>) {
    if doc.roster.tier > prev.roster.tier {
        findings.push(finding(
            Severity::Warning,
            "roster-tier-raised",
            format!(
                "roster tier raised from {} to {}",
                prev.roster.tier, doc.roster.tier
            ),
        ));
    }

    for rule in &doc.roster.fields {
        match prev.roster.fields.iter().find(|p| p.field == rule.field) {
            Some(prev_rule) if rule.audience > prev_rule.audience => findings.push(finding(
                Severity::Warning,
                "field-audience-widened",
                format!(
                    "field '{}' audience widened from {} to {}",
                    rule.field, prev_rule.audience, rule.audience
                ),
            )),
            Some(_) => {}
            // 新增字段规则且受众非 org-members：上一版该字段缺省不对外
            None if rule.audience != Audience::OrgMembers => findings.push(finding(
                Severity::Warning,
                "field-exposed",
                format!(
                    "new field rule '{}' exposes to {}",
                    rule.field, rule.audience
                ),
            )),
            None => {}
        }
    }

    for entry in &doc.upward {
        let is_new = !prev
            .upward
            .iter()
            .any(|p| p.collection == entry.collection && p.to == entry.to);
        if is_new {
            findings.push(finding(
                Severity::Warning,
                "new-upward-entry",
                format!("new upward opening ({}, {})", entry.collection, entry.to),
            ));
        }
    }
}

fn finding(severity: Severity, code: &'static str, detail: String) -> AnalysisFinding {
    AnalysisFinding {
        severity,
        code,
        detail,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::doc::{ENGINE_B1, FieldRule, RosterRules, UpwardEntry};

    const ORG: &str = "org_a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0";
    const UPSTREAM: &str = "org_b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1";

    fn doc(
        tier: RosterTier,
        fields: Vec<(&str, Audience)>,
        upward: Vec<(&str, &str)>,
    ) -> PolicyDoc {
        PolicyDoc {
            policy_v: 1,
            engine: ENGINE_B1.to_string(),
            org_id: ORG.to_string(),
            roster: RosterRules {
                tier,
                fields: fields
                    .into_iter()
                    .map(|(field, audience)| FieldRule {
                        field: field.to_string(),
                        audience,
                    })
                    .collect(),
            },
            upward: upward
                .into_iter()
                .map(|(collection, to)| UpwardEntry {
                    collection: collection.to_string(),
                    to: to.to_string(),
                })
                .collect(),
            updated_at: 1_720_000_000_000,
            sig_set: None,
        }
    }

    fn codes(findings: &[AnalysisFinding]) -> Vec<&'static str> {
        findings.iter().map(|f| f.code).collect()
    }

    #[test]
    fn clean_first_version_has_no_findings() {
        let d = doc(
            RosterTier::Representatives,
            vec![("nickname", Audience::Public)],
            vec![],
        );
        assert!(analyze(&d, None).is_empty());
    }

    #[test]
    fn structure_error_short_circuits() {
        let mut bad = doc(RosterTier::Public, vec![], vec![]);
        bad.org_id = "bad".to_string();
        assert_eq!(codes(&analyze(&bad, None)), vec!["invalid-structure"]);
        bad.org_id = ORG.to_string();
        bad.engine = "cedar".to_string();
        assert_eq!(codes(&analyze(&bad, None)), vec!["unsupported-engine"]);
    }

    #[test]
    fn conflict_errors() {
        let dup_fields = doc(
            RosterTier::Public,
            vec![
                ("nickname", Audience::Public),
                ("nickname", Audience::Representatives),
            ],
            vec![],
        );
        assert_eq!(
            codes(&analyze(&dup_fields, None)),
            vec!["duplicate-field-rule"]
        );

        let redundant = doc(
            RosterTier::Public,
            vec![("phone", Audience::OrgMembers)],
            vec![],
        );
        assert_eq!(
            codes(&analyze(&redundant, None)),
            vec!["field-rule-redundant"]
        );

        let dup_upward = doc(
            RosterTier::Public,
            vec![],
            vec![
                ("finance:monthly@v1", UPSTREAM),
                ("finance:monthly@v1", UPSTREAM),
            ],
        );
        assert_eq!(
            codes(&analyze(&dup_upward, None)),
            vec!["duplicate-upward-entry"]
        );

        // org-only 档下字段规则不可达
        let shadowed = doc(
            RosterTier::OrgOnly,
            vec![("nickname", Audience::Public)],
            vec![],
        );
        assert_eq!(
            codes(&analyze(&shadowed, None)),
            vec!["field-rule-shadowed"]
        );
    }

    #[test]
    fn widening_warnings() {
        let prev = doc(
            RosterTier::OrgOnly,
            vec![
                ("nickname", Audience::Representatives),
                ("phone", Audience::Public),
            ],
            vec![("finance:monthly@v1", UPSTREAM)],
        );
        let next = doc(
            RosterTier::Public,
            vec![
                ("nickname", Audience::Public), // 放宽 → field-audience-widened
                ("phone", Audience::Public),    // 不变 → 不告警
                ("address", Audience::Public),  // 新增 → field-exposed
            ],
            vec![
                ("finance:monthly@v1", UPSTREAM), // 既有 → 不告警
                ("finance:annual@v1", UPSTREAM),  // 新增 → new-upward-entry
            ],
        );
        assert_eq!(
            codes(&analyze(&next, Some(&prev))),
            vec![
                "roster-tier-raised",
                "field-audience-widened",
                "field-exposed",
                "new-upward-entry"
            ]
        );
    }

    #[test]
    fn narrowing_is_silent() {
        let prev = doc(
            RosterTier::Public,
            vec![("nickname", Audience::Public)],
            vec![("finance:monthly@v1", UPSTREAM)],
        );
        let next = doc(RosterTier::OrgOnly, vec![], vec![]);
        assert!(analyze(&next, Some(&prev)).is_empty());
    }
}

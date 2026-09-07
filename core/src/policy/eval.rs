//! 求值入口（policy §3–§4）：read-gate §4 第 5 步消费，fail-closed。
//!
//! 求值只消费「已通过验证链的凭证摘要」——read-gate 第 1–4 步（结构/验证链/
//! 类型匹配/holderProof）是前置，本模块不重复验签。档位管行可见性、字段规则
//! 管字段可见性，两条正交（§4 第 4 条：字段判定不回看档位）。

use super::doc::{Audience, PolicyDoc, RosterTier, policy_doc_hash, validate_policy_doc};
use super::error::{PolicyError, Result};

/// 已验证凭证呈现的摘要（read-gate §4 第 2–4 步通过后的凭证）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PresentedCredential {
    /// 凭证类型（credential §2 credType）。
    pub cred_type: String,
    /// 对象域 orgId（凭证 subjectDomain）。
    pub subject_domain: String,
}

/// 请求者上下文（policy §3；由数据账号侧装配，关系判定不在本模块）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RequesterContext {
    /// 请求者是数据属主组织成员（域边界基线：总见本名册）。
    pub is_org_member: bool,
    /// 请求者是属主（共同体）某成员组织的代表。
    pub is_representative: bool,
    /// 已通过验证链的呈现凭证摘要。
    pub credentials: Vec<PresentedCredential>,
}

/// 读请求分类（policy §3）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadRequest<'a> {
    /// 读名册人员行；`row_is_representative` = 该行的代表标记。
    RosterRow {
        /// 该行人员是否为所在成员组织的代表。
        row_is_representative: bool,
    },
    /// 读名册某字段。
    RosterField {
        /// 字段名。
        field: &'a str,
    },
    /// 读某数据集合（向上开放矩阵）。
    Collection {
        /// 集合名。
        collection: &'a str,
    },
}

/// 求值结论：放行 / 拒绝（含稳定原因名）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadVerdict {
    /// 放行。
    Allow,
    /// 拒绝（fail-closed 的规则内拒绝；文档级错误走 Err 通道）。
    Deny(DenyReason),
}

impl ReadVerdict {
    /// 是否放行。
    pub fn is_allow(&self) -> bool {
        matches!(self, Self::Allow)
    }

    /// 稳定结论名（vectors `expect` 口径）：`allow` / `row-hidden` /
    /// `field-hidden` / `not-covered`。
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Deny(DenyReason::RowHidden) => "row-hidden",
            Self::Deny(DenyReason::FieldHidden) => "field-hidden",
            Self::Deny(DenyReason::NotCovered) => "not-covered",
        }
    }
}

/// 规则内拒绝原因（policy §4 第 3–5 条）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DenyReason {
    /// 名册行对外不可见（档位拒绝）。
    RowHidden,
    /// 字段未声明或受众不含请求者。
    FieldHidden,
    /// 集合无向上开放条目（或呈现凭证域不符）。
    NotCovered,
}

/// read-gate 求值入口（policy §4，fail-closed）：
///
/// 1. `policy_ref` 复算 ≠ `policyDocHash` → `policy-ref-mismatch`（文档篡改/引用错位）；
/// 2. 结构/引擎校验（`engine != "b1"` → `unsupported-engine`）；
/// 3. 规则求值（档位/字段掩码/向上开放矩阵），未覆盖一律拒绝。
pub fn evaluate_read(
    policy_ref: &str,
    doc: &PolicyDoc,
    requester: &RequesterContext,
    request: &ReadRequest,
) -> Result<ReadVerdict> {
    if policy_doc_hash(doc)? != policy_ref {
        return Err(PolicyError::PolicyRefMismatch);
    }
    validate_policy_doc(doc)?;
    Ok(eval_rules(doc, requester, request))
}

/// 规则求值本体（无 policyRef 校验；保存期/本地渲染用）。
pub fn eval_rules(
    doc: &PolicyDoc,
    requester: &RequesterContext,
    request: &ReadRequest,
) -> ReadVerdict {
    match request {
        ReadRequest::RosterRow {
            row_is_representative,
        } => eval_roster_row(doc, requester, *row_is_representative),
        ReadRequest::RosterField { field } => eval_roster_field(doc, requester, field),
        ReadRequest::Collection { collection } => eval_collection(doc, requester, collection),
    }
}

/// 请求者 ∈ 受众（policy §3）。
fn in_audience(requester: &RequesterContext, audience: Audience) -> bool {
    match audience {
        Audience::OrgMembers => requester.is_org_member,
        Audience::Representatives => requester.is_org_member || requester.is_representative,
        Audience::Public => true,
    }
}

/// §4 第 3 条：名册行。本组织成员走域边界基线恒放行；档位决定对外行可见性。
fn eval_roster_row(
    doc: &PolicyDoc,
    requester: &RequesterContext,
    row_is_representative: bool,
) -> ReadVerdict {
    if requester.is_org_member {
        return ReadVerdict::Allow;
    }
    let visible = match doc.roster.tier {
        RosterTier::OrgOnly => false,
        RosterTier::Representatives => row_is_representative,
        RosterTier::Public => true,
    };
    if visible {
        ReadVerdict::Allow
    } else {
        ReadVerdict::Deny(DenyReason::RowHidden)
    }
}

/// §4 第 4 条：名册字段。基线成员恒放行；未声明字段 fail-closed；已声明按受众。
fn eval_roster_field(doc: &PolicyDoc, requester: &RequesterContext, field: &str) -> ReadVerdict {
    if requester.is_org_member {
        return ReadVerdict::Allow;
    }
    match doc.roster.fields.iter().find(|r| r.field == field) {
        Some(rule) if in_audience(requester, rule.audience) => ReadVerdict::Allow,
        _ => ReadVerdict::Deny(DenyReason::FieldHidden),
    }
}

/// §4 第 5 条：数据集合。向上开放矩阵命中且呈现凭证域匹配才放行。
fn eval_collection(doc: &PolicyDoc, requester: &RequesterContext, collection: &str) -> ReadVerdict {
    let open = doc.upward.iter().any(|entry| {
        entry.collection == collection
            && requester
                .credentials
                .iter()
                .any(|c| c.subject_domain == entry.to)
    });
    if open {
        ReadVerdict::Allow
    } else {
        ReadVerdict::Deny(DenyReason::NotCovered)
    }
}

/// 行渲染掩码：对外请求者在一行上可见的字段名列表（行不可见 → 空）。
///
/// 本组织成员不受掩码约束（调用方直接放行全字段），故只对外部请求者有意义。
pub fn visible_fields(
    doc: &PolicyDoc,
    requester: &RequesterContext,
    row_is_representative: bool,
) -> Vec<String> {
    if !matches!(
        eval_roster_row(doc, requester, row_is_representative),
        ReadVerdict::Allow
    ) {
        return Vec::new();
    }
    doc.roster
        .fields
        .iter()
        .filter(|rule| in_audience(requester, rule.audience))
        .map(|rule| rule.field.clone())
        .collect()
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

    fn requester(member: bool, rep: bool, domains: &[&str]) -> RequesterContext {
        RequesterContext {
            is_org_member: member,
            is_representative: rep,
            credentials: domains
                .iter()
                .map(|d| PresentedCredential {
                    cred_type: "household-owner".to_string(),
                    subject_domain: d.to_string(),
                })
                .collect(),
        }
    }

    fn tier_doc() -> PolicyDoc {
        doc(
            RosterTier::Representatives,
            vec![("nickname", Audience::Representatives)],
            vec![],
        )
    }

    #[test]
    fn roster_row_tiers() {
        let reps = tier_doc();
        let member = requester(true, false, &[]);
        let rep = requester(false, true, &[]);
        let outsider = requester(false, false, &[]);

        // 基线：本组织成员任何档位恒放行
        let row = ReadRequest::RosterRow {
            row_is_representative: false,
        };
        assert!(
            evaluate_read(&policy_doc_hash(&reps).unwrap(), &reps, &member, &row)
                .unwrap()
                .is_allow()
        );

        // representatives 档：代表行对外放行（成员/代表/外部皆可见），非代表行对外拒绝
        let rep_row = ReadRequest::RosterRow {
            row_is_representative: true,
        };
        assert!(eval_rules(&reps, &rep, &rep_row).is_allow());
        assert_eq!(eval_rules(&reps, &rep, &row).kind(), "row-hidden");
        assert!(eval_rules(&reps, &outsider, &rep_row).is_allow());

        // org-only 档：对外全部行拒绝
        let org_only = doc(RosterTier::OrgOnly, vec![], vec![]);
        assert_eq!(eval_rules(&org_only, &rep, &rep_row).kind(), "row-hidden");
        // public 档：任何行对外放行
        let public = doc(RosterTier::Public, vec![], vec![]);
        assert!(eval_rules(&public, &outsider, &row).is_allow());
    }

    #[test]
    fn roster_field_mask() {
        let d = doc(
            RosterTier::Public,
            vec![
                ("nickname", Audience::Public),
                ("household", Audience::Representatives),
            ],
            vec![],
        );
        let member = requester(true, false, &[]);
        let rep = requester(false, true, &[]);
        let resident = requester(false, false, &[ORG]);

        // 成员基线：声明与否都放行
        assert!(eval_rules(&d, &member, &ReadRequest::RosterField { field: "phone" }).is_allow());
        // 代表：public + representatives 字段放行
        assert!(eval_rules(&d, &rep, &ReadRequest::RosterField { field: "nickname" }).is_allow());
        assert!(eval_rules(&d, &rep, &ReadRequest::RosterField { field: "household" }).is_allow());
        // 普通住户：public 放行，representatives 与未声明字段拒绝
        assert!(
            eval_rules(
                &d,
                &resident,
                &ReadRequest::RosterField { field: "nickname" }
            )
            .is_allow()
        );
        assert_eq!(
            eval_rules(
                &d,
                &resident,
                &ReadRequest::RosterField { field: "household" }
            )
            .kind(),
            "field-hidden"
        );
        assert_eq!(
            eval_rules(&d, &resident, &ReadRequest::RosterField { field: "phone" }).kind(),
            "field-hidden"
        );
    }

    #[test]
    fn collection_matrix() {
        let d = doc(
            RosterTier::OrgOnly,
            vec![],
            vec![("finance:monthly@v1", UPSTREAM)],
        );
        let holder = requester(false, false, &[UPSTREAM]);
        let wrong_domain = requester(false, false, &[ORG]);
        let none = requester(false, false, &[]);

        let monthly = ReadRequest::Collection {
            collection: "finance:monthly@v1",
        };
        assert!(eval_rules(&d, &holder, &monthly).is_allow());
        // 域不符 / 无凭证 → not-covered
        assert_eq!(
            eval_rules(&d, &wrong_domain, &monthly).kind(),
            "not-covered"
        );
        assert_eq!(eval_rules(&d, &none, &monthly).kind(), "not-covered");
        // 集合无条目 → not-covered
        let annual = ReadRequest::Collection {
            collection: "finance:annual@v1",
        };
        assert_eq!(eval_rules(&d, &holder, &annual).kind(), "not-covered");
    }

    #[test]
    fn fail_closed_doc_errors() {
        let d = tier_doc();
        let member = requester(true, false, &[]);
        let row = ReadRequest::RosterRow {
            row_is_representative: true,
        };
        // policyRef 篡改
        let err = evaluate_read(&"f".repeat(64), &d, &member, &row).unwrap_err();
        assert_eq!(err.kind(), "policy-ref-mismatch");
        // 引擎不匹配（B2 升级路径出口）
        let mut cedar = d.clone();
        cedar.engine = "cedar".to_string();
        let err =
            evaluate_read(&policy_doc_hash(&cedar).unwrap(), &cedar, &member, &row).unwrap_err();
        assert_eq!(err.kind(), "unsupported-engine");
    }

    #[test]
    fn visible_fields_masking() {
        let d = doc(
            RosterTier::Representatives,
            vec![
                ("nickname", Audience::Public),
                ("household", Audience::Representatives),
                ("phone", Audience::OrgMembers),
            ],
            vec![],
        );
        let rep = requester(false, true, &[]);
        let outsider = requester(false, false, &[]);
        // 代表行上：代表可见 nickname + household（phone 受众 org-members 不含代表）
        assert_eq!(
            visible_fields(&d, &rep, true),
            vec!["nickname", "household"]
        );
        // 非代表行对代表不可见 → 空
        assert!(visible_fields(&d, &rep, false).is_empty());
        // 代表行对任何外部请求者可见（档位语义：代表行对外可见），字段按受众掩码
        assert_eq!(visible_fields(&d, &outsider, true), vec!["nickname"]);
    }
}

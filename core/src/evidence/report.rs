//! 打印友好核验报告（evidence §4.2，EV2 落点）：单页可打印纯文本——
//! 材料摘要、签名清单（签名者 identity + 当时角色 + 有效性）、名册快照
//! 摘要、三层结论（分层如实分列，不作「总体通过」模糊判定）、存证锚引用。
//!
//! 设计约束：纯文本等宽友好、不依赖 Spark 语义注释（街道办人员读得懂）；
//! 里程碑二路径 = 懂技术成员跑 CLI → 打印 → 相关人员手工签字 → 提交
//! （patterns 配方五既定排序①）。

use super::export::{EvidenceExportPackage, MembershipOutcome, VerifyReport};

/// epoch 毫秒 → `YYYY-MM-DD HH:MM:SS UTC`（Howard Hinnant civil_from_days，
/// 零依赖）。
pub fn format_utc(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    let (h, m, s) = (tod / 3600, (tod % 3600) / 60, tod % 60);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let yr = if mo <= 2 { y + 1 } else { y };
    format!("{yr:04}-{mo:02}-{d:02} {h:02}:{m:02}:{s:02} UTC")
}

/// 标识缩短（前 `n` 位 + 省略号；报表人读用）。
fn short(id: &str, n: usize) -> String {
    if id.chars().count() <= n {
        id.to_string()
    } else {
        format!("{}…", id.chars().take(n).collect::<String>())
    }
}

/// 角色人读标签（不引入 Spark 术语）。
fn role_label(role: Option<&str>) -> String {
    match role {
        Some("admin") => "管理员".to_string(),
        Some("member") => "成员".to_string(),
        Some(other) => other.to_string(),
        None => "（不在名册）".to_string(),
    }
}

/// 签名类别人读标签。
fn kind_label(kind: &str) -> String {
    if kind == "exporter" {
        "材料导出".to_string()
    } else if let Some(node) = kind.strip_prefix("anchor:") {
        format!("存证锚（{}）", short(node, 10))
    } else {
        kind.to_string()
    }
}

const RULE: &str = "============================================================";

/// 渲染单页可打印报告（package 解析成功时调用；`report` 为六步核验结果）。
pub fn render_printable_report(
    package: &EvidenceExportPackage,
    report: &VerifyReport,
) -> String {
    let mut out = String::new();
    let mut line = |s: String| {
        out.push_str(&s);
        out.push('\n');
    };

    line(RULE.to_string());
    line("                  存证材料核验报告".to_string());
    line(RULE.to_string());

    // 一、材料摘要
    line("一、材料摘要".to_string());
    line(format!("  导出时间    : {}", format_utc(report.exporter_ts)));
    line(format!("  导出人标识  : {}", short(&report.exporter_root_id, 16)));
    let scope = &package.scope;
    let scope_text = match (
        scope.org_id.as_deref(),
        scope.domain.as_deref(),
        scope.collection.as_deref(),
    ) {
        (org, d, c) => {
            let mut parts = Vec::new();
            if let Some(org) = org {
                parts.push(format!("组织 {org}"));
            }
            if let Some(d) = d {
                parts.push(format!("数据域 {d}"));
            }
            if let Some(c) = c {
                parts.push(format!("集合 {c}"));
            }
            if parts.is_empty() {
                "全部存证（未声明范围）".to_string()
            } else {
                parts.join("，")
            }
        }
    };
    line(format!("  材料范围    : {scope_text}"));
    line(format!("  存证条目数  : {} 条", package.entries.len()));
    line(format!("  组织锚记录数: {} 条", report.anchor_count));

    // 二、签名清单
    line(String::new());
    line("二、签名清单".to_string());
    if report.signer_checks.is_empty() {
        line("  （本包签名未随名册回查——v1 格式仅含导出签名）".to_string());
        line(format!(
            "  {:<4}  {:<18}  {:<14}  {:<8}  {}",
            "序号", "签名者标识", "签名类型", "当时角色", "有效性"
        ));
        line(format!(
            "  {:<4}  {:<18}  {:<14}  {:<8}  {}",
            1,
            short(&report.exporter_root_id, 16),
            "材料导出",
            "（未回查）",
            if report.integrity_failures.is_empty() {
                "有效"
            } else {
                "无效"
            }
        ));
    } else {
        line(format!(
            "  {:<4}  {:<18}  {:<14}  {:<8}  {}",
            "序号", "签名者标识", "签名类型", "当时角色", "有效性"
        ));
        for (i, check) in report.signer_checks.iter().enumerate() {
            let validity = if check.ok {
                "有效".to_string()
            } else {
                format!("无效（{}）", check.reason.clone().unwrap_or_default())
            };
            line(format!(
                "  {:<4}  {:<18}  {:<14}  {:<8}  {}",
                i + 1,
                short(&check.identity, 16),
                kind_label(&check.kind),
                role_label(check.role.as_deref()),
                validity
            ));
        }
    }

    // 三、名册快照摘要
    line(String::new());
    line("三、名册快照摘要".to_string());
    match &report.roster_summary {
        Some(summary) => {
            line(format!(
                "  快照人数    : {} 人（其中管理员 {} 人）",
                summary.member_count, summary.admin_count
            ));
            line(format!(
                "  名册指纹    : {}",
                short(&summary.member_set_hash, 24)
            ));
            line(format!(
                "  名册锚定时间: {}",
                format_utc(summary.anchor_ts)
            ));
            line(format!(
                "  锚根指纹    : {}",
                short(&summary.anchor_root, 24)
            ));
        }
        None => {
            line("  本包不含名册快照。".to_string());
            line("  成员资格（签名者当时是否为组织成员/有何角色）无法经本包核验。".to_string());
        }
    }

    // 四、核验结论（分层如实分列）
    line(String::new());
    line("四、核验结论（分层如实分列，不作总体判定）".to_string());
    let integrity = if report.integrity_failures.is_empty() {
        "通过".to_string()
    } else {
        format!("不通过（{} 项）", report.integrity_failures.len())
    };
    line(format!("  ① 完整性（材料未被篡改、签名有效）: {integrity}"));
    let membership = match report.membership {
        MembershipOutcome::Pass => "通过".to_string(),
        MembershipOutcome::Fail => format!("不通过（{} 项）", report.membership_notes.len()),
        MembershipOutcome::NotCovered => "未覆盖（本包不含名册快照）".to_string(),
    };
    line(format!(
        "  ② 成员资格（签名者当时在册与角色）: {membership}"
    ));
    line(format!("  ③ 业务资格（验证人凭证等）        : {}", report.business_note));

    // 五、失败明细（无失败省略本节）
    if !report.failures.is_empty() {
        line(String::new());
        line("五、失败明细".to_string());
        for failure in &report.failures {
            line(format!("  - {failure}"));
        }
    }

    // 六、存证锚引用
    line(String::new());
    line("六、存证锚引用".to_string());
    if report.member_heads.is_empty() {
        line("  （无）".to_string());
    } else {
        for (node_id, seq, hash) in &report.member_heads {
            line(format!(
                "  节点 {}  序号 {}  哈希 {}",
                short(node_id, 16),
                seq,
                short(hash, 16)
            ));
        }
    }

    line(RULE.to_string());
    line("本报告由 evidence-verify 离线生成，结论仅依赖导出包文件本身，".to_string());
    line("不依赖任何在线服务。打印后请核对人手工签字确认。".to_string());
    line(RULE.to_string());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evidence::export::{
        ExportScope, Exporter, ExportHead, SignerCheck, SpecRef, MembershipOutcome as Outcome,
    };
    use serde_json::Map;

    #[test]
    fn format_utc_known_values() {
        assert_eq!(format_utc(0), "1970-01-01 00:00:00 UTC");
        assert_eq!(format_utc(1_720_000_000_000), "2024-07-03 09:46:40 UTC");
        // 负值（epoch 前）不 panic
        assert_eq!(format_utc(-1_000), "1969-12-31 23:59:59 UTC");
    }

    fn sample_package() -> EvidenceExportPackage {
        EvidenceExportPackage {
            format_version: 2,
            scope: ExportScope {
                domain: Some("plugin:vote".to_string()),
                collection: Some("ballots".to_string()),
                org_id: Some("org_0000000000000001".to_string()),
            },
            exporter: Exporter {
                root_id: "ab".repeat(32),
                public_key: String::new(),
                ts: 1_720_000_000_000,
                sig: String::new(),
            },
            head: ExportHead {
                seq: 4,
                hash: "cd".repeat(32),
            },
            entries: vec![],
            anchors: vec![],
            anchor_root: None,
            anchor_proofs: Map::new(),
            roster: None,
            spec: SpecRef {
                canonical_json: String::new(),
                spec_ref: String::new(),
            },
        }
    }

    #[test]
    fn report_contains_all_sections() {
        let pkg = sample_package();
        let mut report = VerifyReport {
            height: 4,
            anchor_count: 1,
            exporter_root_id: "ab".repeat(32),
            exporter_ts: 1_720_000_000_000,
            membership: Outcome::Pass,
            member_heads: vec![("12D3KooWNodeA".to_string(), 4, "cd".repeat(32))],
            ..Default::default()
        };
        report.signer_checks.push(SignerCheck {
            kind: "exporter".to_string(),
            identity: "ab".repeat(32),
            role: Some("admin".to_string()),
            ok: true,
            reason: None,
        });
        report.roster_summary = Some(crate::evidence::export::RosterSummary {
            member_count: 3,
            admin_count: 2,
            member_set_hash: "ef".repeat(32),
            anchor_root: "01".repeat(32),
            anchor_ts: 1_720_000_000_000,
        });
        report.business_note = crate::evidence::BUSINESS_LAYER_NOTE.to_string();
        let text = render_printable_report(&pkg, &report);
        for section in [
            "存证材料核验报告",
            "一、材料摘要",
            "二、签名清单",
            "三、名册快照摘要",
            "四、核验结论",
            "六、存证锚引用",
            "2024-07-03 09:46:40 UTC",
            "管理员",
            "① 完整性",
            "② 成员资格",
            "③ 业务资格",
            "手工签字",
        ] {
            assert!(text.contains(section), "缺 section: {section}\n{text}");
        }
        // 无失败明细节（无失败时省略）
        assert!(!text.contains("五、失败明细"));
    }

    #[test]
    fn report_v1_and_failure_rendering() {
        let pkg = sample_package();
        let mut report = VerifyReport {
            exporter_root_id: "ab".repeat(32),
            membership: Outcome::NotCovered,
            ..Default::default()
        };
        report
            .failures
            .push("② 成员资格: memberSetHash 复算不符".to_string());
        report.business_note = crate::evidence::BUSINESS_LAYER_NOTE.to_string();
        let text = render_printable_report(&pkg, &report);
        assert!(text.contains("本包不含名册快照"));
        assert!(text.contains("未覆盖（本包不含名册快照）"));
        assert!(text.contains("五、失败明细"));
        // 分层如实：不作总体判定话术
        assert!(!text.contains("总体通过"));
    }
}

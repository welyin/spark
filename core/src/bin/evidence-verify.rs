//! 存证导出包独立核验工具（阶段四F，evidence-anchoring-export §3 +
//! evidence §4.1 名册段 / §4.2 打印报告）。
//!
//! 用法：`evidence-verify <package.json> [--report]`——不依赖运行中节点、
//! 不联网；六步核验（sync-evidence §9 五步 + evidence §4.1 名册回查）后
//! 输出人读报告，退出码 0 = 通过 / 1 = 失败（各项原因逐条列出）/
//! 2 = 用法或读文件错误。`--report` 输出单页可打印文本（案件摘要、签名
//! 清单、名册快照摘要、三层结论、锚引用——街道办可读，打印后手工签字）。
//!
//! 红线 5：canonical/哈希/验签全部复用 `spark_core::evidence` 同 crate
//! 实现（golden vectors 锁死逐字节行为），本文件只做 IO 与报告排版。

use std::process::ExitCode;

use spark_core::evidence::{
    EvidenceExportPackage, MembershipOutcome, VerifyReport, render_printable_report,
    verify_export_package,
};

fn print_report(report: &VerifyReport) {
    println!("=== 存证导出包核验报告 ===");
    println!("链高（head.seq）   : {}", report.height);
    println!("锚点数            : {}", report.anchor_count);
    println!(
        "导出者 rootId     : {}",
        if report.exporter_root_id.is_empty() {
            "(未知)"
        } else {
            &report.exporter_root_id
        }
    );
    if !report.member_heads.is_empty() {
        println!("各成员链头：");
        for (node_id, seq, hash) in &report.member_heads {
            let short_hash: String = hash.chars().take(16).collect();
            println!("  {node_id}  seq={seq}  hash={short_hash}…");
        }
    }
    // 分层诚实三层分列（不作「总体通过」模糊话术）
    println!("分层结论：");
    println!(
        "  ① 完整性（链 + 锚 + 签名）: {}",
        if report.integrity_failures.is_empty() {
            "通过".to_string()
        } else {
            format!("失败（{} 项）", report.integrity_failures.len())
        }
    );
    println!(
        "  ② 成员资格（名册快照回查）: {}",
        match report.membership {
            MembershipOutcome::Pass => "通过".to_string(),
            MembershipOutcome::Fail => format!("失败（{} 项）", report.membership_notes.len()),
            MembershipOutcome::NotCovered => "未覆盖（本包不含名册快照）".to_string(),
        }
    );
    println!("  ③ 业务资格（验证人凭证）  : {}", report.business_note);
    if !report.failures.is_empty() {
        println!("失败明细（{} 项）：", report.failures.len());
        for failure in &report.failures {
            println!("  ✗ {failure}");
        }
    }
}

fn main() -> ExitCode {
    let mut args = std::env::args();
    let bin = args.next().unwrap_or_else(|| "evidence-verify".to_string());
    let Some(path) = args.next() else {
        eprintln!("用法: {bin} <package.json> [--report]");
        return ExitCode::from(2);
    };
    let report_mode = match (args.next(), args.next()) {
        (None, None) => false,
        (Some(flag), None) if flag == "--report" => true,
        _ => {
            eprintln!("用法: {bin} <package.json> [--report]");
            return ExitCode::from(2);
        }
    };
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(e) => {
            eprintln!("读取 {path} 失败: {e}");
            return ExitCode::from(2);
        }
    };
    let report = verify_export_package(&raw);
    if report_mode {
        match serde_json::from_str::<EvidenceExportPackage>(&raw) {
            Ok(package) => print!("{}", render_printable_report(&package, &report)),
            Err(e) => eprintln!("包 JSON 解析失败，无法生成打印报告: {e}"),
        }
    } else {
        print_report(&report);
    }
    if report.ok() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

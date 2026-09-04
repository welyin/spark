//! 存证导出包独立核验工具（阶段四F，evidence-anchoring-export §3）。
//!
//! 用法：`evidence-verify <package.json>`——不依赖运行中节点、不联网；
//! 五步核验（sync-evidence §9 / 设计 §3）后输出人读报告，退出码
//! 0 = 通过 / 1 = 失败（各项原因逐条列出）/ 2 = 用法或读文件错误。
//!
//! 红线 5：canonical/哈希/验签全部复用 `spark_core::evidence` 同 crate
//! 实现（golden vectors 锁死逐字节行为），本文件只做 IO 与报告排版。
//!
//! 分层诚实标注：本工具验证「密码学完整性与签名有效」；「签名者当时是
//! 组织成员/角色」需成员表存证锚快照（后续增强项，设计 §3）。

use std::process::ExitCode;

use spark_core::evidence::{VerifyReport, verify_export_package};

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
    println!(
        "分层说明          : 本报告覆盖密码学完整性与签名有效性；成员资格/角色核验需成员表存证锚快照（后续增强）。"
    );
    if report.ok() {
        println!("结论              : 通过（五步核验全部命中）");
    } else {
        println!("结论              : 失败（{} 项）", report.failures.len());
        for failure in &report.failures {
            println!("  ✗ {failure}");
        }
    }
}

fn main() -> ExitCode {
    let mut args = std::env::args();
    let bin = args.next().unwrap_or_else(|| "evidence-verify".to_string());
    let Some(path) = args.next() else {
        eprintln!("用法: {bin} <package.json>");
        return ExitCode::from(2);
    };
    if args.next().is_some() {
        eprintln!("用法: {bin} <package.json>（只接受一个参数）");
        return ExitCode::from(2);
    }
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(e) => {
            eprintln!("读取 {path} 失败: {e}");
            return ExitCode::from(2);
        }
    };
    let report = verify_export_package(&raw);
    print_report(&report);
    if report.ok() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

// 编译期注入构建时间戳，用于运行时确认二进制新鲜度（联调排障用）。
// 每次编译生成 SPARK_BUILD_TIME，配合 P2P 启动日志打印。
use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    println!("cargo:rustc-env=SPARK_BUILD_UNIX={secs}");

    // 人类可读 UTC（不依赖 chrono，手算 Y-M-D H:M:S）
    println!(
        "cargo:rustc-env=SPARK_BUILD_TIME={}",
        format_unix_utc(secs)
    );

    // 任何源码变化都应触发重跑 build.rs（默认即如此：无 cargo:rerun-if 时每次全跑）。
    // 显式监听核心目录，保证增量编译也刷新时间戳。
    println!("cargo:rerun-if-changed=src");
}

fn format_unix_utc(secs: u64) -> String {
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let (y, mo, d) = civil_from_days(days as i64);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

// Howard Hinnant 的 civil_from_days 算法（days since 1970-01-01 → 公历日期）
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let mo = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    (if mo <= 2 { y + 1 } else { y }, mo, d)
}

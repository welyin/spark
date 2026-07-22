//! 语义化版本比较（对齐 TS desktop/src/main/updater/semver.ts 的 compareSemver）。
//!
//! 只支持 `x.y.z[-pre.release]`；非法输入报错（TS 同样 throw，调用方兜底）。

#[derive(Clone, Debug, PartialEq, Eq)]
struct ParsedSemver {
    major: u64,
    minor: u64,
    patch: u64,
    pre_release: Vec<String>,
}

fn parse_number(value: &str) -> Result<u64, String> {
    if value.is_empty() || !value.chars().all(|c| c.is_ascii_digit()) {
        return Err(format!("Invalid semver segment: {value}"));
    }
    value
        .parse::<u64>()
        .map_err(|_| format!("Invalid semver segment: {value}"))
}

fn parse_semver(version: &str) -> Result<ParsedSemver, String> {
    let trimmed = version.trim();
    // TS `trimmed.split('-', 2)`：首个 '-' 切出 prerelease，其余保留在 prerelease 内
    let (main, pre) = match trimmed.split_once('-') {
        Some((main, pre)) => (main, Some(pre)),
        None => (trimmed, None),
    };
    let parts: Vec<&str> = main.split('.').collect();
    if parts.len() != 3 {
        return Err(format!("Invalid semver: {version}"));
    }
    Ok(ParsedSemver {
        major: parse_number(parts[0])?,
        minor: parse_number(parts[1])?,
        patch: parse_number(parts[2])?,
        pre_release: pre
            .map(|p| p.split('.').filter(|item| !item.is_empty()).map(str::to_string).collect())
            .unwrap_or_default(),
    })
}

fn is_numeric(value: &str) -> bool {
    !value.is_empty() && value.chars().all(|c| c.is_ascii_digit())
}

fn compare_pre_release(left: &[String], right: &[String]) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (left.is_empty(), right.is_empty()) {
        (true, true) => return Ordering::Equal,
        (true, false) => return Ordering::Greater,
        (false, true) => return Ordering::Less,
        (false, false) => {}
    }
    let count = left.len().max(right.len());
    for index in 0..count {
        let (Some(l), Some(r)) = (left.get(index), right.get(index)) else {
            // TS：短的一侧小（undefined 分支）
            return if left.get(index).is_none() {
                Ordering::Less
            } else {
                Ordering::Greater
            };
        };
        let (l_num, r_num) = (is_numeric(l), is_numeric(r));
        let ord = match (l_num, r_num) {
            (true, true) => l.parse::<u64>().unwrap_or(0).cmp(&r.parse::<u64>().unwrap_or(0)),
            (true, false) => Ordering::Less,
            (false, true) => Ordering::Greater,
            // TS localeCompare 对 ASCII 标识符等价于字节序
            (false, false) => l.cmp(r),
        };
        if ord != Ordering::Equal {
            return ord;
        }
    }
    Ordering::Equal
}

/// 比较两个语义化版本：left > right 返回 1，相等 0，小于 -1（TS `compareSemver`）。
pub fn compare_semver(left: &str, right: &str) -> Result<i32, String> {
    let l = parse_semver(left)?;
    let r = parse_semver(right)?;
    let ord = (l.major, l.minor, l.patch)
        .cmp(&(r.major, r.minor, r.patch))
        .then_with(|| compare_pre_release(&l.pre_release, &r.pre_release));
    Ok(match ord {
        std::cmp::Ordering::Greater => 1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Less => -1,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compare_basic_and_prerelease() {
        assert_eq!(compare_semver("0.2.0", "0.1.0").unwrap(), 1);
        assert_eq!(compare_semver("0.1.0", "0.1.0").unwrap(), 0);
        assert_eq!(compare_semver("0.1.0", "0.2.0").unwrap(), -1);
        assert_eq!(compare_semver("1.0.0", "0.9.9").unwrap(), 1);
        assert_eq!(compare_semver("0.1.1", "0.1.0").unwrap(), 1);
        // prerelease < 正式版；数字段按数值、字母段按字典序、短序列小
        assert_eq!(compare_semver("1.0.0-alpha", "1.0.0").unwrap(), -1);
        assert_eq!(compare_semver("1.0.0-alpha.1", "1.0.0-alpha").unwrap(), 1);
        assert_eq!(compare_semver("1.0.0-2", "1.0.0-10").unwrap(), -1);
        assert_eq!(compare_semver("1.0.0-alpha", "1.0.0-1").unwrap(), 1);
    }

    #[test]
    fn invalid_versions_error() {
        assert!(compare_semver("1.0", "1.0.0").is_err());
        assert!(compare_semver("1.0.x", "1.0.0").is_err());
        assert!(compare_semver("", "1.0.0").is_err());
    }
}

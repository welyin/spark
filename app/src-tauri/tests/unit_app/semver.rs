//! 插件市场语义化版本比较单测。

use spark_app_lib::market::semver::*;

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

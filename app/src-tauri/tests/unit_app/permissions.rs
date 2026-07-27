//! 插件市场权限归一化/授权解析单测。

use spark_app_lib::market::permissions::*;

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

#[test]
fn normalize_filters_invalid_and_dedupes() {
    let declared = strings(&["org:sync", "bogus", "org:sync", "identity:sign"]);
    assert_eq!(
        normalize_declared_permissions(&declared),
        strings(&["org:sync", "identity:sign"])
    );
    assert!(normalize_declared_permissions(&[]).is_empty());
}

#[test]
fn granted_is_basic_union_declared_advanced() {
    // 声明高级权限 → 基础 + 高级
    let granted = resolve_granted_permissions(&strings(&["org:sync"]));
    assert_eq!(
        granted,
        strings(&["storage:read", "storage:write", "org:read", "proof:verify", "org:sync"])
    );
    // 声明基础权限（本已恒授予）不重复、声明非法项不授予
    let granted = resolve_granted_permissions(&strings(&["storage:read", "bogus"]));
    assert_eq!(
        granted,
        strings(&["storage:read", "storage:write", "org:read", "proof:verify"])
    );
}

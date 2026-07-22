//! 插件权限模型（对齐 TS desktop/src/main/plugins/permissions.ts）。
//!
//! 基础权限默认授予；高级权限须声明并经安装授权。授权结果持久化在
//! `InstalledPluginState.granted_permissions`，渲染进程无法自报或修改。
//!
//! 权限一律以字符串表示（对齐 TS 的 `PluginPermission` 字符串联合）；
//! 集合运算保持 TS Set 的插入序语义（基础权限在前，高级权限按声明序追加）。

/// 全部合法权限（TS `PLUGIN_PERMISSIONS`）。
pub const PLUGIN_PERMISSIONS: [&str; 7] = [
    "storage:read",
    "storage:write",
    "org:read",
    "org:sync",
    "network:broadcast",
    "proof:verify",
    "identity:sign",
];

/// 基础权限：默认授予所有插件，无需声明（TS `BASIC_PERMISSIONS`）。
pub const BASIC_PERMISSIONS: [&str; 4] = ["storage:read", "storage:write", "org:read", "proof:verify"];

/// 高级权限：必须声明并经安装时授权（TS `ADVANCED_PERMISSIONS`）。
pub const ADVANCED_PERMISSIONS: [&str; 3] = ["org:sync", "network:broadcast", "identity:sign"];

pub fn is_plugin_permission(value: &str) -> bool {
    PLUGIN_PERMISSIONS.contains(&value)
}

/// 规范化插件声明的权限列表：过滤非法项、去重（TS `normalizeDeclaredPermissions`）。
pub fn normalize_declared_permissions(declared: &[String]) -> Vec<String> {
    let mut result: Vec<String> = Vec::new();
    for item in declared {
        if is_plugin_permission(item) && !result.contains(item) {
            result.push(item.clone());
        }
    }
    result
}

/// 计算插件实际获得的权限：基础权限恒授予；高级权限仅声明后授予
/// （TS `resolveGrantedPermissions`）。
pub fn resolve_granted_permissions(declared: &[String]) -> Vec<String> {
    let mut granted: Vec<String> = BASIC_PERMISSIONS.iter().map(|p| p.to_string()).collect();
    for permission in declared {
        if ADVANCED_PERMISSIONS.contains(&permission.as_str()) && !granted.contains(permission) {
            granted.push(permission.clone());
        }
    }
    granted
}

/// 仅基础权限（TS `[...BASIC_PERMISSIONS]`，未知域/无目录项时的回退）。
pub fn basic_permissions() -> Vec<String> {
    BASIC_PERMISSIONS.iter().map(|p| p.to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

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
}

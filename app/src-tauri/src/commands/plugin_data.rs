//! P6 声明式数据命令（plugin.data*：iframe 桥侧入口；QuickJS 侧走 host capability）。

use serde_json::Value;
use spark_core::kernel::Kernel;

use super::dto::SuccessResult;
use super::{err, lock_kernel};
use crate::KernelState;

// ------------------------------------------------------------------
// 核心实现（测试直调）
// ------------------------------------------------------------------

/// nit：枚举轴解析泛型辅助——scope/space/accounts/devices/confidentiality/merge
/// 六轴结构一致（读字符串字段 → serde_json 反序列化枚举），收敛为一个函数。
fn parse_enum_axis<T: serde::de::DeserializeOwned>(
    declaration: &Value,
    field: &str,
) -> Result<Option<T>, String> {
    match declaration.get(field).and_then(Value::as_str) {
        None => Ok(None),
        Some(s) => serde_json::from_value(Value::String(s.to_string()))
            .map(Some)
            .map_err(|e| e.to_string()),
    }
}

pub(crate) fn data_declare_collection_inner(
    kernel: &mut Kernel,
    domain: &str,
    declaration: Value,
) -> Result<Value, String> {
    // B5：透传 space/accounts/confidentiality/orgId（org scope 由内核按运行
    // 空间解析并校验调用方为该组织成员）。name/version/scope/devices/merge
    // 从声明字面量解析，其余轴补 Default。
    let axis = |field: &str| -> Option<String> {
        declaration.get(field).and_then(Value::as_str).map(str::to_string)
    };
    let input = spark_core::plugindata::DeclareInput {
        name: declaration
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| "missing declaration.name".to_string())?
            .to_string(),
        version: axis("version"),
        scope: parse_enum_axis::<spark_core::plugindata::Scope>(&declaration, "scope")?,
        space: parse_enum_axis::<spark_core::plugindata::Space>(&declaration, "space")?,
        accounts: parse_enum_axis::<spark_core::plugindata::Accounts>(&declaration, "accounts")?,
        devices: parse_enum_axis::<spark_core::plugindata::Devices>(&declaration, "devices")?,
        confidentiality: parse_enum_axis::<spark_core::plugindata::Confidentiality>(&declaration, "confidentiality")?,
        merge: parse_enum_axis::<spark_core::plugindata::MergeRule>(&declaration, "merge")?,
        declared_by: axis("declaredBy"),
    };
    // org space：payload 中可携带 orgId（由内核按插件运行空间传入）
    let org_id = declaration.get("orgId").and_then(Value::as_str);
    let decl = kernel
        .data_declare_collection(domain, input, org_id)
        .map_err(err)?;
    serde_json::to_value(&decl).map_err(|e| e.to_string())
}

pub(crate) fn data_save_inner(
    kernel: &mut Kernel,
    domain: &str,
    name: &str,
    key: &str,
    value: Value,
    version: Option<&str>,
    org_id: Option<&str>,
) -> Result<SuccessResult, String> {
    kernel
        .data_save(domain, name, key, value, version, org_id)
        .map_err(err)?;
    Ok(SuccessResult::ok())
}

pub(crate) fn data_delete_inner(
    kernel: &mut Kernel,
    domain: &str,
    name: &str,
    key: &str,
    version: Option<&str>,
    org_id: Option<&str>,
) -> Result<SuccessResult, String> {
    kernel
        .data_delete(domain, name, key, version, org_id)
        .map_err(err)?;
    Ok(SuccessResult::ok())
}

pub(crate) fn data_get_inner(
    kernel: &Kernel,
    domain: &str,
    name: &str,
    key: &str,
    version: Option<&str>,
    org_id: Option<&str>,
) -> Result<Option<Value>, String> {
    kernel
        .data_get(domain, name, key, version, org_id)
        .map_err(err)
}

pub(crate) fn data_query_inner(
    kernel: &Kernel,
    domain: &str,
    name: &str,
    prefix: Option<&str>,
    limit: Option<usize>,
    cursor: Option<&str>,
    version: Option<&str>,
    org_id: Option<&str>,
) -> Result<Value, String> {
    let page = kernel
        .data_query(domain, name, prefix, limit, cursor, version, org_id)
        .map_err(err)?;
    let mut out = serde_json::json!({
        "items": page.items.iter().map(|(key, raw)| {
            serde_json::json!({
                "key": key,
                "value": serde_json::from_str::<Value>(raw).unwrap_or(Value::String(raw.clone())),
            })
        }).collect::<Vec<_>>(),
    });
    if let Some(next) = page.next_cursor {
        out["nextCursor"] = Value::String(next);
    }
    Ok(out)
}

pub(crate) fn data_drop_version_inner(
    kernel: &mut Kernel,
    domain: &str,
    name: &str,
    version: &str,
) -> Result<SuccessResult, String> {
    kernel.data_drop_version(domain, name, version).map_err(err)?;
    Ok(SuccessResult::ok())
}

pub(crate) fn data_save_blob_inner(
    kernel: &mut Kernel,
    data_base64: &str,
) -> Result<Value, String> {
    let info = kernel.data_save_blob(data_base64).map_err(err)?;
    serde_json::to_value(&info).map_err(|e| e.to_string())
}

pub(crate) fn data_read_blob_inner(kernel: &mut Kernel, hash: &str) -> Result<Value, String> {
    match kernel.data_read_blob(hash).map_err(err)? {
        Some(data) => Ok(serde_json::json!({ "status": "ready", "data": data })),
        None => Ok(serde_json::json!({ "status": "pending" })),
    }
}

// ------------------------------------------------------------------
// O4 encrypted 授权名单（plugin-data-api §5.2）：grantAccess / revokeAccess /
// listAccess（owner 侧）。内核按 acl owner 验签，无额外权限项；非 owner →
// AccessDenied。
// ------------------------------------------------------------------

pub(crate) fn data_grant_access_inner(
    kernel: &mut Kernel,
    org_id: &str,
    name: &str,
    version: &str,
    members: Vec<String>,
) -> Result<Value, String> {
    let acl = kernel
        .data_grant_access(org_id, name, version, &members)
        .map_err(err)?;
    serde_json::to_value(&acl).map_err(|e| e.to_string())
}

pub(crate) fn data_revoke_access_inner(
    kernel: &mut Kernel,
    org_id: &str,
    name: &str,
    version: &str,
    members: Vec<String>,
) -> Result<Value, String> {
    let acl = kernel
        .data_revoke_access(org_id, name, version, &members)
        .map_err(err)?;
    serde_json::to_value(&acl).map_err(|e| e.to_string())
}

pub(crate) fn data_list_access_inner(
    kernel: &Kernel,
    org_id: &str,
    name: &str,
    version: &str,
) -> Result<Value, String> {
    let acl = kernel.data_list_access(org_id, name, version).map_err(err)?;
    Ok(serde_json::json!({
        "owners": acl.owners,
        "readers": acl.readers,
        "epoch": acl.epoch,
    }))
}

// ------------------------------------------------------------------
// Tauri 命令
// ------------------------------------------------------------------

#[tauri::command]
pub fn data_declare_collection(
    state: tauri::State<'_, KernelState>,
    domain: String,
    declaration: Value,
) -> Result<Value, String> {
    data_declare_collection_inner(&mut *lock_kernel(&state)?, &domain, declaration)
}

#[tauri::command]
pub fn data_save(
    state: tauri::State<'_, KernelState>,
    domain: String,
    name: String,
    key: String,
    value: Value,
    version: Option<String>,
    org_id: Option<String>,
) -> Result<SuccessResult, String> {
    data_save_inner(
        &mut *lock_kernel(&state)?,
        &domain,
        &name,
        &key,
        value,
        version.as_deref(),
        org_id.as_deref(),
    )
}

#[tauri::command]
pub fn data_delete(
    state: tauri::State<'_, KernelState>,
    domain: String,
    name: String,
    key: String,
    version: Option<String>,
    org_id: Option<String>,
) -> Result<SuccessResult, String> {
    data_delete_inner(
        &mut *lock_kernel(&state)?,
        &domain,
        &name,
        &key,
        version.as_deref(),
        org_id.as_deref(),
    )
}

#[tauri::command]
pub fn data_get(
    state: tauri::State<'_, KernelState>,
    domain: String,
    name: String,
    key: String,
    version: Option<String>,
    org_id: Option<String>,
) -> Result<Option<Value>, String> {
    data_get_inner(
        &*lock_kernel(&state)?,
        &domain,
        &name,
        &key,
        version.as_deref(),
        org_id.as_deref(),
    )
}

#[tauri::command]
pub fn data_query(
    state: tauri::State<'_, KernelState>,
    domain: String,
    name: String,
    prefix: Option<String>,
    limit: Option<u32>,
    cursor: Option<String>,
    version: Option<String>,
    org_id: Option<String>,
) -> Result<Value, String> {
    data_query_inner(
        &*lock_kernel(&state)?,
        &domain,
        &name,
        prefix.as_deref(),
        limit.map(|n| n as usize),
        cursor.as_deref(),
        version.as_deref(),
        org_id.as_deref(),
    )
}

#[tauri::command]
pub fn data_drop_version(
    state: tauri::State<'_, KernelState>,
    domain: String,
    name: String,
    version: String,
) -> Result<SuccessResult, String> {
    data_drop_version_inner(&mut *lock_kernel(&state)?, &domain, &name, &version)
}

#[tauri::command]
pub fn data_save_blob(
    state: tauri::State<'_, KernelState>,
    data_base64: String,
) -> Result<Value, String> {
    data_save_blob_inner(&mut *lock_kernel(&state)?, &data_base64)
}

#[tauri::command]
pub fn data_read_blob(
    state: tauri::State<'_, KernelState>,
    hash: String,
) -> Result<Value, String> {
    data_read_blob_inner(&mut *lock_kernel(&state)?, &hash)
}

#[tauri::command]
pub fn data_grant_access(
    state: tauri::State<'_, KernelState>,
    org_id: String,
    name: String,
    version: String,
    members: Vec<String>,
) -> Result<Value, String> {
    data_grant_access_inner(
        &mut *lock_kernel(&state)?,
        &org_id,
        &name,
        &version,
        members,
    )
}

#[tauri::command]
pub fn data_revoke_access(
    state: tauri::State<'_, KernelState>,
    org_id: String,
    name: String,
    version: String,
    members: Vec<String>,
) -> Result<Value, String> {
    data_revoke_access_inner(
        &mut *lock_kernel(&state)?,
        &org_id,
        &name,
        &version,
        members,
    )
}

#[tauri::command]
pub fn data_list_access(
    state: tauri::State<'_, KernelState>,
    org_id: String,
    name: String,
    version: String,
) -> Result<Value, String> {
    data_list_access_inner(&*lock_kernel(&state)?, &org_id, &name, &version)
}

// ------------------------------------------------------------------
// 单元测试
// ------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use spark_core::kernel::KernelConfig;

    const PASSWORD: &str = "correct-horse-battery";

    fn unlocked_kernel() -> (tempfile::TempDir, Kernel) {
        let dir = tempfile::tempdir().unwrap();
        let mut kernel = Kernel::init(KernelConfig {
            data_dir: dir.path().to_path_buf(),
            app_version: "0.0.0-test".to_string(),
            p2p: None,
        })
        .unwrap();
        kernel.init_identity(PASSWORD, "alice", None).unwrap();
        (dir, kernel)
    }

    #[test]
    fn declare_save_get_query_delete_roundtrip() {
        let (_dir, mut kernel) = unlocked_kernel();
        data_declare_collection_inner(
            &mut kernel,
            "plugin:ai-chat",
            json!({ "name": "ai-chat:conversations" }),
        )
        .unwrap();
        data_save_inner(
            &mut kernel,
            "plugin:ai-chat",
            "ai-chat:conversations",
            "c1",
            json!({"title": "一"}),
            None,
            None,
        )
        .unwrap();
        let got = data_get_inner(
            &kernel,
            "plugin:ai-chat",
            "ai-chat:conversations",
            "c1",
            None,
            None,
        )
        .unwrap()
        .unwrap();
        assert_eq!(got["title"], json!("一"));
        let page = data_query_inner(
            &kernel,
            "plugin:ai-chat",
            "ai-chat:conversations",
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        assert_eq!(page["items"].as_array().unwrap().len(), 1);
        assert_eq!(page["items"][0]["key"], json!("c1"), "返回集合内相对键");
        data_delete_inner(&mut kernel, "plugin:ai-chat", "ai-chat:conversations", "c1", None, None)
            .unwrap();
        assert!(
            data_get_inner(
                &kernel,
                "plugin:ai-chat",
                "ai-chat:conversations",
                "c1",
                None,
                None
            )
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn cross_plugin_access_rejected() {
        let (_dir, mut kernel) = unlocked_kernel();
        assert!(
            data_declare_collection_inner(
                &mut kernel,
                "plugin:ai-chat",
                json!({ "name": "other:x" }),
            )
            .is_err(),
            "声明他插件前缀被拒"
        );
        data_declare_collection_inner(
            &mut kernel,
            "plugin:ai-chat",
            json!({ "name": "ai-chat:c" }),
        )
        .unwrap();
        assert!(
            data_get_inner(&kernel, "plugin:evil", "ai-chat:c", "k", None, None).is_err(),
            "跨插件读取被拒"
        );
    }

    /// F9：org space 声明须校验调用方为该组织成员（对齐 host_env.rs）——
    /// 成员可声明，非成员 orgId 被拒。
    #[test]
    fn org_declare_requires_membership() {
        let (_dir, mut kernel) = unlocked_kernel();
        // 创建组织（alice 为唯一 admin/成员）。
        let org = kernel
            .create_org(spark_core::org::service::CreateOrganizationInput {
                name: "测试组织".to_string(),
                description: None,
                avatar: None,
                base_plugin_domain: None,
            })
            .unwrap();
        let org_id = org.record.org_id.clone();
        // 成员声明 org 集合 → 成功。
        data_declare_collection_inner(
            &mut kernel,
            "plugin:ai-chat",
            json!({ "name": "ai-chat:orgdocs", "space": "org", "orgId": org_id }),
        )
        .unwrap();
        // 非成员 orgId → 拒绝（成员资格校验）。
        let fake_org = format!("org_{}", "0".repeat(16));
        let err = data_declare_collection_inner(
            &mut kernel,
            "plugin:ai-chat",
            json!({ "name": "ai-chat:evil", "space": "org", "orgId": fake_org }),
        )
        .unwrap_err();
        assert!(
            err.contains("not a member"),
            "非成员 org space 声明被拒：{err}"
        );
        // orgId 缺失（org space）→ 拒绝。
        assert!(
            data_declare_collection_inner(
                &mut kernel,
                "plugin:ai-chat",
                json!({ "name": "ai-chat:nomissing", "space": "org" }),
            )
            .is_err(),
            "org space 声明缺 orgId 被拒"
        );
    }

    #[test]
    fn blob_roundtrip_and_pending() {
        let (_dir, mut kernel) = unlocked_kernel();
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD.encode(b"blob-bytes");
        let info = data_save_blob_inner(&mut kernel, &b64).unwrap();
        let hash = info["hash"].as_str().unwrap().to_string();
        let read = data_read_blob_inner(&mut kernel, &hash).unwrap();
        assert_eq!(read["status"], json!("ready"));
        assert_eq!(read["data"], json!(b64));
        // 未命中 → pending（want 标记已置）
        let missing = data_read_blob_inner(&mut kernel, &"0".repeat(64)).unwrap();
        assert_eq!(missing["status"], json!("pending"));
    }

    /// O4 工作项 5（iframe 桥通路）：encrypted 授权名单三命令——grantAccess
    /// （创世，owner 自签）→ listAccess 反映 → revokeAccess 轮换 epoch。无额外
    /// 权限项，内核按 acl owner 验签。
    #[test]
    fn access_grant_revoke_list_commands() {
        use spark_core::plugindata::{Accounts, Confidentiality, DeclareInput, Scope, Space};
        let (_dir, mut kernel) = unlocked_kernel();
        let org = kernel
            .create_org(spark_core::org::service::CreateOrganizationInput {
                name: "测试组织".to_string(),
                description: None,
                avatar: None,
                base_plugin_domain: None,
            })
            .unwrap();
        let org_id = org.record.org_id.clone();
        const BOB: &str = "b0b0000000000000000000000000000000000000000000000000000000000000";
        kernel.org_add_member(&org_id, BOB, None).unwrap();
        kernel
            .data_declare_collection(
                "plugin:ai-chat",
                DeclareInput {
                    name: "ai-chat:payroll".to_string(),
                    version: Some("1.0.0".to_string()),
                    space: Some(Space::Org),
                    accounts: Some(Accounts::DataAccounts),
                    confidentiality: Some(Confidentiality::Encrypted),
                    scope: Some(Scope::Sync),
                    ..Default::default()
                },
                Some(&org_id),
            )
            .unwrap();
        // grant（创世，owner 自签）
        let granted = data_grant_access_inner(
            &mut kernel,
            &org_id,
            "ai-chat:payroll",
            "1.0.0",
            vec![BOB.to_string()],
        )
        .unwrap();
        assert_eq!(granted["epoch"], json!(1));
        // list
        let listed = data_list_access_inner(&kernel, &org_id, "ai-chat:payroll", "1.0.0").unwrap();
        assert!(listed["readers"].as_array().unwrap().iter().any(|r| r == BOB));
        // revoke → epoch+1
        let revoked = data_revoke_access_inner(
            &mut kernel,
            &org_id,
            "ai-chat:payroll",
            "1.0.0",
            vec![BOB.to_string()],
        )
        .unwrap();
        assert_eq!(revoked["epoch"], json!(2));
        assert!(revoked["readers"].as_array().unwrap().is_empty());
    }
}

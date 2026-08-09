//! P6 声明式数据命令（plugin.data*：iframe 桥侧入口；QuickJS 侧走 host capability）。

use serde_json::Value;
use spark_core::kernel::Kernel;

use super::dto::SuccessResult;
use super::{err, lock_kernel};
use crate::KernelState;

// ------------------------------------------------------------------
// 核心实现（测试直调）
// ------------------------------------------------------------------

pub(crate) fn data_declare_collection_inner(
    kernel: &mut Kernel,
    domain: &str,
    declaration: Value,
) -> Result<Value, String> {
    let input = spark_core::plugindata::DeclareInput {
        name: declaration
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| "missing declaration.name".to_string())?
            .to_string(),
        version: declaration
            .get("version")
            .and_then(Value::as_str)
            .map(str::to_string),
        scope: declaration
            .get("scope")
            .and_then(Value::as_str)
            .map(|s| serde_json::from_value(Value::String(s.to_string())))
            .transpose()
            .map_err(|e| e.to_string())?,
        devices: declaration
            .get("devices")
            .and_then(Value::as_str)
            .map(|s| serde_json::from_value(Value::String(s.to_string())))
            .transpose()
            .map_err(|e| e.to_string())?,
        merge: declaration
            .get("merge")
            .and_then(Value::as_str)
            .map(|s| serde_json::from_value(Value::String(s.to_string())))
            .transpose()
            .map_err(|e| e.to_string())?,
    };
    let decl = kernel
        .data_declare_collection(domain, input)
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
) -> Result<SuccessResult, String> {
    kernel
        .data_save(domain, name, key, value, version)
        .map_err(err)?;
    Ok(SuccessResult::ok())
}

pub(crate) fn data_delete_inner(
    kernel: &mut Kernel,
    domain: &str,
    name: &str,
    key: &str,
    version: Option<&str>,
) -> Result<SuccessResult, String> {
    kernel.data_delete(domain, name, key, version).map_err(err)?;
    Ok(SuccessResult::ok())
}

pub(crate) fn data_get_inner(
    kernel: &Kernel,
    domain: &str,
    name: &str,
    key: &str,
    version: Option<&str>,
) -> Result<Option<Value>, String> {
    kernel.data_get(domain, name, key, version).map_err(err)
}

pub(crate) fn data_query_inner(
    kernel: &Kernel,
    domain: &str,
    name: &str,
    prefix: Option<&str>,
    limit: Option<usize>,
    cursor: Option<&str>,
    version: Option<&str>,
) -> Result<Value, String> {
    let page = kernel
        .data_query(domain, name, prefix, limit, cursor, version)
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
) -> Result<SuccessResult, String> {
    data_save_inner(
        &mut *lock_kernel(&state)?,
        &domain,
        &name,
        &key,
        value,
        version.as_deref(),
    )
}

#[tauri::command]
pub fn data_delete(
    state: tauri::State<'_, KernelState>,
    domain: String,
    name: String,
    key: String,
    version: Option<String>,
) -> Result<SuccessResult, String> {
    data_delete_inner(
        &mut *lock_kernel(&state)?,
        &domain,
        &name,
        &key,
        version.as_deref(),
    )
}

#[tauri::command]
pub fn data_get(
    state: tauri::State<'_, KernelState>,
    domain: String,
    name: String,
    key: String,
    version: Option<String>,
) -> Result<Option<Value>, String> {
    data_get_inner(&*lock_kernel(&state)?, &domain, &name, &key, version.as_deref())
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
) -> Result<Value, String> {
    data_query_inner(
        &*lock_kernel(&state)?,
        &domain,
        &name,
        prefix.as_deref(),
        limit.map(|n| n as usize),
        cursor.as_deref(),
        version.as_deref(),
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
        )
        .unwrap();
        let got = data_get_inner(&kernel, "plugin:ai-chat", "ai-chat:conversations", "c1", None)
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
        )
        .unwrap();
        assert_eq!(page["items"].as_array().unwrap().len(), 1);
        assert_eq!(page["items"][0]["key"], json!("c1"), "返回集合内相对键");
        data_delete_inner(&mut kernel, "plugin:ai-chat", "ai-chat:conversations", "c1", None)
            .unwrap();
        assert!(
            data_get_inner(&kernel, "plugin:ai-chat", "ai-chat:conversations", "c1", None)
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
            data_get_inner(&kernel, "plugin:evil", "ai-chat:c", "k", None).is_err(),
            "跨插件读取被拒"
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
}

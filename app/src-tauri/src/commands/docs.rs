//! 文档命令（plugin.doc* / 集合本地读写路径）。

use serde_json::Value;
use spark_core::kernel::Kernel;
use spark_core::schema::{CollectionSchemaDeclaration, CollectionSchemaRecord};

use super::dto::{
    CollectionConfigDto, QueryOptionsDto, QueryResultDto, SuccessResult,
};
use super::{err, lock_kernel};
use crate::KernelState;

// ------------------------------------------------------------------
// 核心实现（测试直调）
// ------------------------------------------------------------------

pub(crate) fn doc_get_inner(
    kernel: &Kernel,
    domain: &str,
    collection: &str,
    id: &str,
) -> Result<Option<Value>, String> {
    kernel.doc_get(domain, collection, id).map_err(err)
}

pub(crate) fn doc_put_inner(
    kernel: &mut Kernel,
    domain: &str,
    collection: &str,
    id: &str,
    doc: Value,
    config: CollectionConfigDto,
) -> Result<SuccessResult, String> {
    kernel
        .doc_put(domain, collection, id, doc, config.into_config()?)
        .map_err(err)?;
    Ok(SuccessResult::ok())
}

pub(crate) fn doc_delete_inner(
    kernel: &mut Kernel,
    domain: &str,
    collection: &str,
    id: &str,
    config: CollectionConfigDto,
) -> Result<SuccessResult, String> {
    kernel
        .doc_delete(domain, collection, id, config.into_config()?)
        .map_err(err)?;
    // TS `delete` 对不存在文档为空操作且仍返回 success: true
    Ok(SuccessResult::ok())
}

pub(crate) fn doc_query_inner(
    kernel: &Kernel,
    domain: &str,
    collection: &str,
    config: CollectionConfigDto,
    options: QueryOptionsDto,
) -> Result<QueryResultDto, String> {
    let result = kernel
        .doc_query(
            domain,
            collection,
            config.into_config()?,
            options.into_options()?,
        )
        .map_err(err)?;
    Ok(QueryResultDto::from(result))
}

pub(crate) fn doc_declare_collection_inner(
    kernel: &mut Kernel,
    domain: &str,
    collection: &str,
    declaration: CollectionSchemaDeclaration,
) -> Result<CollectionSchemaRecord, String> {
    kernel
        .declare_collection(domain, collection, declaration)
        .map_err(err)
}

// ------------------------------------------------------------------
// Tauri 命令
// ------------------------------------------------------------------

#[tauri::command]
pub fn doc_get(
    state: tauri::State<'_, KernelState>,
    domain: String,
    collection: String,
    id: String,
) -> Result<Option<Value>, String> {
    doc_get_inner(&*lock_kernel(&state)?, &domain, &collection, &id)
}

#[tauri::command]
pub fn doc_put(
    state: tauri::State<'_, KernelState>,
    domain: String,
    collection: String,
    id: String,
    doc: Value,
    config: Option<CollectionConfigDto>,
) -> Result<SuccessResult, String> {
    doc_put_inner(
        &mut *lock_kernel(&state)?,
        &domain,
        &collection,
        &id,
        doc,
        config.unwrap_or_default(),
    )
}

#[tauri::command]
pub fn doc_delete(
    state: tauri::State<'_, KernelState>,
    domain: String,
    collection: String,
    id: String,
    config: Option<CollectionConfigDto>,
) -> Result<SuccessResult, String> {
    doc_delete_inner(
        &mut *lock_kernel(&state)?,
        &domain,
        &collection,
        &id,
        config.unwrap_or_default(),
    )
}

#[tauri::command]
pub fn doc_query(
    state: tauri::State<'_, KernelState>,
    domain: String,
    collection: String,
    config: Option<CollectionConfigDto>,
    options: Option<QueryOptionsDto>,
) -> Result<QueryResultDto, String> {
    doc_query_inner(
        &*lock_kernel(&state)?,
        &domain,
        &collection,
        config.unwrap_or_default(),
        options.unwrap_or_default(),
    )
}

#[tauri::command]
pub fn doc_declare_collection(
    state: tauri::State<'_, KernelState>,
    domain: String,
    collection: String,
    declaration: CollectionSchemaDeclaration,
) -> Result<CollectionSchemaRecord, String> {
    doc_declare_collection_inner(&mut *lock_kernel(&state)?, &domain, &collection, declaration)
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
    const DOMAIN: &str = "plugin:test";

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
    fn doc_commands_require_storage() {
        let dir = tempfile::tempdir().unwrap();
        let kernel = Kernel::init(KernelConfig {
            data_dir: dir.path().to_path_buf(),
            app_version: "0.0.0-test".to_string(),
            p2p: None,
        })
        .unwrap();
        // 无活动身份 → 存储未打开
        assert_eq!(
            doc_get_inner(&kernel, DOMAIN, "posts", "1").unwrap_err(),
            "storage is not open: no active identity"
        );
    }

    #[test]
    fn doc_crud_roundtrip() {
        let (_dir, mut kernel) = unlocked_kernel();
        // 缺省策略为 append-only（内核默认）；可覆盖写/删的集合显式声明 lww
        let config: CollectionConfigDto =
            serde_json::from_value(json!({"syncStrategy": "lww"})).unwrap();

        // get 不存在 → null
        assert_eq!(doc_get_inner(&kernel, DOMAIN, "posts", "p1").unwrap(), None);

        // put → get 回读
        doc_put_inner(
            &mut kernel,
            DOMAIN,
            "posts",
            "p1",
            json!({"title": "hello", "kind": "post"}),
            config.clone(),
        )
        .unwrap();
        let got = doc_get_inner(&kernel, DOMAIN, "posts", "p1")
            .unwrap()
            .unwrap();
        assert_eq!(got["title"], json!("hello"));

        // query：filter 命中
        let result = doc_query_inner(
            &kernel,
            DOMAIN,
            "posts",
            config.clone(),
            serde_json::from_value::<QueryOptionsDto>(json!({
                "filter": [{"field": "kind", "value": "post"}]
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(result.items.len(), 1);
        assert_eq!(result.items[0].id, "p1");

        // delete → get 回空；重复 delete 仍 success（lww 集合上的幂等空操作）
        doc_delete_inner(&mut kernel, DOMAIN, "posts", "p1", config.clone()).unwrap();
        assert_eq!(doc_get_inner(&kernel, DOMAIN, "posts", "p1").unwrap(), None);
        doc_delete_inner(&mut kernel, DOMAIN, "posts", "p1", config).unwrap();
    }

    #[test]
    fn append_only_collection_rejects_overwrite_and_delete() {
        let (_dir, mut kernel) = unlocked_kernel();
        let config = CollectionConfigDto::default(); // 缺省 append-only
        doc_put_inner(&mut kernel, DOMAIN, "ledger", "e1", json!({"v": 1}), config.clone()).unwrap();
        // 覆盖同 id → 拒绝
        assert!(doc_put_inner(&mut kernel, DOMAIN, "ledger", "e1", json!({"v": 2}), config.clone()).is_err());
        // 删除 → 拒绝
        assert!(doc_delete_inner(&mut kernel, DOMAIN, "ledger", "e1", config).is_err());
    }

    #[test]
    fn declare_collection_roundtrip() {
        let (_dir, mut kernel) = unlocked_kernel();
        let declaration: CollectionSchemaDeclaration = serde_json::from_value(json!({
            "syncStrategy": "append-only",
            "governance": true,
            "enableEvidence": true
        }))
        .unwrap();
        let record =
            doc_declare_collection_inner(&mut kernel, DOMAIN, "ledger", declaration).unwrap();
        let text = serde_json::to_string(&record).unwrap();
        assert!(text.contains("append-only"));
    }
}

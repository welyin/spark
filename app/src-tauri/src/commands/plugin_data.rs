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
        sensitivity: parse_enum_axis::<spark_core::plugindata::Sensitivity>(&declaration, "sensitivity")?,
        merge: parse_enum_axis::<spark_core::plugindata::MergeRule>(&declaration, "merge")?,
        declared_by: axis("declaredBy"),
        // readPolicy（read-gate §2，org scope 专有）：按内核 ReadPolicy 线形
        // 透传；缺省/显式 null → None（members 缺省，向后兼容）；取值合法性
        // 由内核 declare 录入校验兜底。
        read_policy: match declaration.get("readPolicy") {
            None | Some(Value::Null) => None,
            Some(v) => serde_json::from_value::<spark_core::plugindata::ReadPolicy>(v.clone())
                .map(Some)
                .map_err(|e| format!("invalid readPolicy: {e}"))?,
        },
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
    domain: &str,
    data_base64: &str,
) -> Result<Value, String> {
    let info = kernel.data_save_blob(data_base64).map_err(err)?;
    // 命名空间隔离（A34）：保存即登记调用域为归属者（幂等）
    blob_ns_add_owner(kernel, &info.hash, domain)?;
    serde_json::to_value(&info).map_err(|e| e.to_string())
}

pub(crate) fn data_read_blob_inner(
    kernel: &mut Kernel,
    domain: &str,
    hash: &str,
) -> Result<Value, String> {
    // 命名空间隔离（A34）：登记在册的 blob 仅归属域可读；无登记记录的历史
    // blob（迁移前保存）放行（存量「哈希即能力」语义不变，向后兼容）
    if let Some(owners) = blob_ns_owners(kernel, hash)? {
        if !owners.iter().any(|owner| owner == domain) {
            return Err(format!(
                "Access denied: blob {hash} is not owned by domain {domain}"
            ));
        }
    }
    match kernel.data_read_blob(hash).map_err(err)? {
        Some(data) => Ok(serde_json::json!({ "status": "ready", "data": data })),
        None => Ok(serde_json::json!({ "status": "pending" })),
    }
}

// ------------------------------------------------------------------
// blob 插件命名空间隔离（A34）
//
// blob 本体仍按内容哈希存全局池（跨插件去重语义保留），但归属按调用方
// 域登记：data_save_blob 把调用域追加进归属簿记；data_read_blob 仅对
// 在册域放行。簿记存 plugindata 本地 scope 集合（ldoc: 键域，不进任何
// 同步流量），归属域为保留域 `spark:blob-registry`，key = blob 哈希。
// （集合名 `spark:blob-registry:owners` 的前缀形态是内核校验要求：
// plugindata 强制集合前缀 == 域去 plugin: 前缀后的 plugin_id。）
//
// 簿记域刻意取**非 `plugin:` 可推导形态**（A34 评审问题 1）：桥绑定域恒
// 为 `plugin:{pluginId}`（PluginIframeHost 按插件 id 推导，插件不可自报），
// 若簿记域也是该形态，名为 blob-registry 的插件（域 plugin:blob-registry）
// 即可经普通 data_save 在自域冒写归属簿记（追加自己为归属域突破越域门禁，
// 或清空他人归属 DoS）；`spark:` 前缀与一切插件绑定域结构性错开，封死此面。
//
// 边界如实说明：本隔离覆盖 iframe 桥面（桥按绑定身份注入域，插件不可
// 自报）；内核 QuickJS 后台运行时的 host_env blob 面在 core/ 内，本期
// 不改 core/，该面仍为全局池（见任务报告遗留）。
// ------------------------------------------------------------------

const BLOB_NS_DOMAIN: &str = "spark:blob-registry";
const BLOB_NS_COLLECTION: &str = "spark:blob-registry:owners";

/// 归属集合声明（幂等；本地 scope，不随 pdsync 扩散）
fn blob_ns_ensure_declared(kernel: &mut Kernel) -> Result<(), String> {
    kernel
        .data_declare_collection(
            BLOB_NS_DOMAIN,
            spark_core::plugindata::DeclareInput {
                name: BLOB_NS_COLLECTION.to_string(),
                version: None,
                scope: Some(spark_core::plugindata::Scope::Local),
                space: None,
                accounts: None,
                devices: None,
                confidentiality: None,
                sensitivity: None,
                merge: None,
                declared_by: None,
                read_policy: None,
            },
            None,
        )
        .map_err(err)?;
    Ok(())
}

/// 读某 blob 的归属域清单（无登记记录 → None）
fn blob_ns_owners(kernel: &Kernel, hash: &str) -> Result<Option<Vec<String>>, String> {
    let record = kernel
        .data_get(BLOB_NS_DOMAIN, BLOB_NS_COLLECTION, hash, None, None)
        .map_err(err)?;
    let Some(record) = record else {
        return Ok(None);
    };
    let owners = record
        .get("owners")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Ok(Some(owners))
}

/// 追加归属域（已登记则幂等跳过）
fn blob_ns_add_owner(kernel: &mut Kernel, hash: &str, domain: &str) -> Result<(), String> {
    blob_ns_ensure_declared(kernel)?;
    let mut owners = blob_ns_owners(kernel, hash)?.unwrap_or_default();
    if owners.iter().any(|owner| owner == domain) {
        return Ok(());
    }
    owners.push(domain.to_string());
    kernel
        .data_save(
            BLOB_NS_DOMAIN,
            BLOB_NS_COLLECTION,
            hash,
            serde_json::json!({ "owners": owners }),
            None,
            None,
        )
        .map_err(err)
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
    domain: String,
    data_base64: String,
) -> Result<Value, String> {
    data_save_blob_inner(&mut *lock_kernel(&state)?, &domain, &data_base64)
}

#[tauri::command]
pub fn data_read_blob(
    state: tauri::State<'_, KernelState>,
    domain: String,
    hash: String,
) -> Result<Value, String> {
    data_read_blob_inner(&mut *lock_kernel(&state)?, &domain, &hash)
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
                ..Default::default()
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

    /// readPolicy 透传（read-gate §2）：org space 声明携带 credential 门禁
    /// → 声明记录落 readPolicy；缺省不带 → 记录无 readPolicy 键（members
    /// 缺省，旧线形一字节不变）；personal space 带 readPolicy → 拒绝。
    #[test]
    fn declare_read_policy_passthrough() {
        let (_dir, mut kernel) = unlocked_kernel();
        let org = kernel
            .create_org(spark_core::org::service::CreateOrganizationInput {
                name: "门禁组织".to_string(),
                description: None,
                avatar: None,
                base_plugin_domain: None,
                ..Default::default()
            })
            .unwrap();
        let org_id = org.record.org_id.clone();
        // credential 门禁声明透传
        let decl = data_declare_collection_inner(
            &mut kernel,
            "plugin:ai-chat",
            json!({
                "name": "ai-chat:ledger",
                "space": "org",
                "orgId": org_id,
                "readPolicy": {
                    "kind": "credential",
                    "credTypes": ["member"],
                    "verifierDomain": org_id,
                    "policyRef": null
                }
            }),
        )
        .unwrap();
        assert_eq!(decl["readPolicy"]["kind"], json!("credential"));
        assert_eq!(decl["readPolicy"]["credTypes"], json!(["member"]));
        assert_eq!(decl["readPolicy"]["verifierDomain"], json!(org_id));
        // 缺省不带 readPolicy → 记录无该键（members 缺省）
        let plain = data_declare_collection_inner(
            &mut kernel,
            "plugin:ai-chat",
            json!({ "name": "ai-chat:plain", "space": "org", "orgId": org_id }),
        )
        .unwrap();
        assert!(
            plain.get("readPolicy").is_none(),
            "缺省 readPolicy 键省略（members 现状）"
        );
        // personal space 带 readPolicy → 拒绝（org scope 专有轴）
        assert!(
            data_declare_collection_inner(
                &mut kernel,
                "plugin:ai-chat",
                json!({
                    "name": "ai-chat:bad",
                    "readPolicy": { "kind": "public" }
                }),
            )
            .is_err(),
            "personal space 声明 readPolicy 被拒"
        );
        // readPolicy 结构非法（缺 kind）→ 拒绝
        assert!(
            data_declare_collection_inner(
                &mut kernel,
                "plugin:ai-chat",
                json!({
                    "name": "ai-chat:bad2",
                    "space": "org",
                    "orgId": org_id,
                    "readPolicy": { "credTypes": ["member"] }
                }),
            )
            .is_err(),
            "readPolicy 结构非法被拒"
        );
    }

    #[test]
    fn blob_roundtrip_and_pending() {
        let (_dir, mut kernel) = unlocked_kernel();
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD.encode(b"blob-bytes");
        let info = data_save_blob_inner(&mut kernel, "plugin:spark-chat", &b64).unwrap();
        let hash = info["hash"].as_str().unwrap().to_string();
        let read = data_read_blob_inner(&mut kernel, "plugin:spark-chat", &hash).unwrap();
        assert_eq!(read["status"], json!("ready"));
        assert_eq!(read["data"], json!(b64));
        // 未命中 → pending（want 标记已置；未登记归属的哈希不设门禁）
        let missing = data_read_blob_inner(&mut kernel, "plugin:spark-chat", &"0".repeat(64)).unwrap();
        assert_eq!(missing["status"], json!("pending"));
    }

    /// 命名空间隔离（A34）：登记在册的 blob 仅归属域可读；越域读取拒绝；
    /// 归属幂等（同域重复保存不重复登记）；第二持有方保存后获得读取权。
    #[test]
    fn blob_namespace_isolation() {
        let (_dir, mut kernel) = unlocked_kernel();
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD.encode(b"chat-image");
        let info = data_save_blob_inner(&mut kernel, "plugin:spark-chat", &b64).unwrap();
        let hash = info["hash"].as_str().unwrap().to_string();

        // 归属域可读
        let read = data_read_blob_inner(&mut kernel, "plugin:spark-chat", &hash).unwrap();
        assert_eq!(read["status"], json!("ready"));
        // 越域读取拒绝（ Access denied 文案与桥权限中间件同前缀）
        let denied = data_read_blob_inner(&mut kernel, "plugin:spark-moments", &hash).unwrap_err();
        assert!(
            denied.starts_with("Access denied:"),
            "越域读取须拒绝，实际：{denied}"
        );
        // 同域重复保存幂等（归属清单不膨胀）
        data_save_blob_inner(&mut kernel, "plugin:spark-chat", &b64).unwrap();
        assert_eq!(
            blob_ns_owners(&kernel, &hash).unwrap(),
            Some(vec!["plugin:spark-chat".to_string()])
        );
        // 第二插件持有同内容并保存 → 追加为归属域后可读（内容即持有证明）
        data_save_blob_inner(&mut kernel, "plugin:spark-moments", &b64).unwrap();
        let read2 = data_read_blob_inner(&mut kernel, "plugin:spark-moments", &hash).unwrap();
        assert_eq!(read2["status"], json!("ready"));
        // 无关第三方仍被拒
        assert!(data_read_blob_inner(&mut kernel, "plugin:evil", &hash).is_err());
    }

    /// 冒名面负例（A34 评审问题 1）：簿记域是非 `plugin:` 可推导形态
    /// （spark:blob-registry），与桥绑定域（恒 plugin:{id}）结构性错开——
    /// 名为 blob-registry 的插件在自域写同名集合不污染簿记、不获得读取权。
    #[test]
    fn blob_namespace_registry_not_spoofable() {
        let (_dir, mut kernel) = unlocked_kernel();
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD.encode(b"chat-image");
        let info = data_save_blob_inner(&mut kernel, "plugin:spark-chat", &b64).unwrap();
        let hash = info["hash"].as_str().unwrap().to_string();

        // 前提断言：簿记域不可由任何插件 id 推导（桥绑定域恒为 plugin:{id}）
        assert!(!BLOB_NS_DOMAIN.starts_with("plugin:"));

        // 攻击重放（旧方案形态）：恶意 .spkg 以 pluginId "blob-registry" 侧载，
        // 其绑定域 plugin:blob-registry 即旧簿记域——在自域声明旧簿记集合名
        // （"blob-registry:owners" 前缀校验在自域通过）并伪造自己为归属域；
        // 新簿记域 spark:blob-registry 与该域结构性错开，写入落在无关记录上
        kernel
            .data_declare_collection(
                "plugin:blob-registry",
                spark_core::plugindata::DeclareInput {
                    name: "blob-registry:owners".to_string(),
                    version: None,
                    scope: Some(spark_core::plugindata::Scope::Local),
                    space: None,
                    accounts: None,
                    devices: None,
                    confidentiality: None,
                    sensitivity: None,
                    merge: None,
                    declared_by: None,
                    read_policy: None,
                },
                None,
            )
            .unwrap();
        kernel
            .data_save(
                "plugin:blob-registry",
                "blob-registry:owners",
                &hash,
                serde_json::json!({ "owners": ["plugin:blob-registry"] }),
                None,
                None,
            )
            .unwrap();

        // 簿记不受自域写入污染：真实归属仍只有保存域
        assert_eq!(
            blob_ns_owners(&kernel, &hash).unwrap(),
            Some(vec!["plugin:spark-chat".to_string()])
        );
        // 冒名者读取仍被拒
        let denied = data_read_blob_inner(&mut kernel, "plugin:blob-registry", &hash).unwrap_err();
        assert!(
            denied.starts_with("Access denied:"),
            "冒名插件读他人 blob 须拒绝，实际：{denied}"
        );
    }
}

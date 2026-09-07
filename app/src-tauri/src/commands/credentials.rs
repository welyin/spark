//! 资格凭证命令（community-affairs §7.2：`sdk.credentials` 壳层薄壳）。
//!
//! 薄壳透传直通内核 `credential_ops` 门面，只做 err 映射与锁内核；权限
//! （`credentials:read`）在桥 dispatcher 层强制（对齐既有插件命令惯例：
//! 壳层命令薄壳不做权限）。`presentHolderProof` 的签名域由桥绑定的插件域
//! 传入（`plugin_domain`），不信插件自报——凭证持有者的公钥必须与该域
//! 身份一致（内核校验）。无签发接口（签发走验证插件的人机流程）。

use serde_json::Value;
use spark_core::kernel::Kernel;

use super::{err, lock_kernel};
use crate::KernelState;

/// 本机持有的凭证列表（`[{credId, credential}]`，credId 字典序）。
pub(crate) fn credentials_list_held_inner(kernel: &Kernel) -> Result<Value, String> {
    let items = kernel.credential_list_held().map_err(err)?;
    serde_json::to_value(&items).map_err(|e| e.to_string())
}

/// 呈现 holderProof：静态校验持有凭证后用插件域身份签 read-gate §3 载荷。
/// 返回 `{credential, holderProof, presentedAt}`。
pub(crate) fn credentials_present_holder_proof_inner(
    kernel: &Kernel,
    plugin_domain: &str,
    cred_id: &str,
    request_id: &str,
    org_id: &str,
    collection: &str,
) -> Result<Value, String> {
    kernel
        .credential_present_holder_proof(plugin_domain, cred_id, request_id, org_id, collection)
        .map_err(err)
}

/// 查询某组织的验证人信任声明（缺失返回空集）。
pub(crate) fn credentials_query_verifiers_inner(kernel: &Kernel, org_id: &str) -> Result<Value, String> {
    kernel.credential_query_verifiers(org_id).map_err(err)
}

/// 凭证验证（credential §6 第 1–5 步结构化裁决；holderProof 绑定归 read-gate）。
pub(crate) fn credentials_verify_inner(kernel: &Kernel, credential: &Value) -> Result<Value, String> {
    kernel.credential_verify(credential).map_err(err)
}

/// 注销查询（按 issuer identity 读本地注销快照；缺失如实报 available:false）。
pub(crate) fn credentials_query_revocations_inner(kernel: &Kernel, issuer: &str) -> Result<Value, String> {
    kernel.credential_query_revocations(issuer).map_err(err)
}

// ------------------------------------------------------------------
// Tauri 命令
// ------------------------------------------------------------------

#[tauri::command]
pub fn plugin_credentials_list_held(
    state: tauri::State<'_, KernelState>,
) -> Result<Value, String> {
    credentials_list_held_inner(&*lock_kernel(&state)?)
}

#[tauri::command]
pub fn plugin_credentials_present_holder_proof(
    state: tauri::State<'_, KernelState>,
    plugin_domain: String,
    cred_id: String,
    request_id: String,
    org_id: String,
    collection: String,
) -> Result<Value, String> {
    credentials_present_holder_proof_inner(
        &*lock_kernel(&state)?,
        &plugin_domain,
        &cred_id,
        &request_id,
        &org_id,
        &collection,
    )
}

#[tauri::command]
pub fn plugin_credentials_query_verifiers(
    state: tauri::State<'_, KernelState>,
    org_id: String,
) -> Result<Value, String> {
    credentials_query_verifiers_inner(&*lock_kernel(&state)?, &org_id)
}

#[tauri::command]
pub fn plugin_credentials_verify(
    state: tauri::State<'_, KernelState>,
    credential: Value,
) -> Result<Value, String> {
    credentials_verify_inner(&*lock_kernel(&state)?, &credential)
}

#[tauri::command]
pub fn plugin_credentials_query_revocations(
    state: tauri::State<'_, KernelState>,
    issuer: String,
) -> Result<Value, String> {
    credentials_query_revocations_inner(&*lock_kernel(&state)?, &issuer)
}

// ------------------------------------------------------------------
// 单元测试：直调 *_inner，不依赖 WebView
// ------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use spark_core::kernel::KernelConfig;

    const DOMAIN: &str = "plugin:spark-example";

    fn unlocked_kernel() -> (tempfile::TempDir, Kernel) {
        let dir = tempfile::tempdir().unwrap();
        let mut kernel = Kernel::init(KernelConfig {
            data_dir: dir.path().to_path_buf(),
            app_version: "0.0.0-test".to_string(),
            p2p: None,
        })
        .unwrap();
        kernel.init_identity("pw-test-creds", "alice", None).unwrap();
        (dir, kernel)
    }

    #[test]
    fn list_held_empty_and_query_verifiers_missing_decl() {
        let (_dir, kernel) = unlocked_kernel();

        // 无持有凭证 → 空数组
        let items = credentials_list_held_inner(&kernel).unwrap();
        assert_eq!(items, serde_json::json!([]));

        // 无信任声明 → 空集（不报错，fail-open 仅对缺失声明）
        let set = credentials_query_verifiers_inner(&kernel, "org_0123456789abcdef").unwrap();
        assert_eq!(set["orgId"], "org_0123456789abcdef");
        assert_eq!(set["verifiers"], serde_json::json!([]));
    }

    #[test]
    fn present_holder_proof_requires_existing_held_credential() {
        let (_dir, kernel) = unlocked_kernel();

        // 不存在的凭证 → 内核拒绝
        let err = credentials_present_holder_proof_inner(
            &kernel,
            DOMAIN,
            "cred_missing",
            "req-1",
            "org_0123456789abcdef",
            "docs",
        )
        .unwrap_err();
        assert!(err.contains("held credential not found"), "unexpected: {err}");
    }

    #[test]
    fn verify_malformed_returns_structured_invalid() {
        let (_dir, kernel) = unlocked_kernel();

        // 非协议线形 → 结构化 invalid（不整体报错）
        let out = credentials_verify_inner(&kernel, &serde_json::json!({ "credV": 1 })).unwrap();
        assert_eq!(out["valid"], serde_json::json!(false));
        assert_eq!(out["checks"]["static"], serde_json::json!(false));
    }

    #[test]
    fn query_revocations_validates_issuer_and_reports_absence() {
        let (_dir, kernel) = unlocked_kernel();

        let err = credentials_query_revocations_inner(&kernel, "not-an-issuer").unwrap_err();
        assert!(err.contains("invalid issuer"), "unexpected: {err}");

        // 合法 issuer 但无快照 → available:false（不冒充「无注销」）
        let out = credentials_query_revocations_inner(&kernel, &"ab".repeat(32)).unwrap();
        assert_eq!(out["available"], serde_json::json!(false));
    }
}

//! 策略命令（community-affairs §7.2：`sdk.policy` 壳层薄壳）。
//!
//! 薄壳透传直通内核 `policy_ops` 门面，只做 err 映射与锁内核；权限
//! （`policy:read` / `policy:write`）在桥 dispatcher 层强制（对齐既有插件
//! 命令惯例：壳层命令薄壳不做权限）。策略插件只产出声明式文档（B1 求值器
//! 在内核）；本面只有本地草稿（`policy:draft:` 键域）的读取与提交，签名
//! 合入（OrgSigSet）不在本命令层。

use serde_json::Value;
use spark_core::kernel::Kernel;

use super::{err, lock_kernel};
use crate::KernelState;

/// 读本组织最新本地策略草稿（`{doc, policyDocHash, savedAt}`；无草稿返回 null）。
pub(crate) fn policy_read_inner(kernel: &Kernel, org_id: &str) -> Result<Option<Value>, String> {
    kernel.policy_read(org_id).map_err(err)
}

/// 提交策略文档草稿：结构/引擎校验 + §5 静态分析 + 落本地草稿键。
/// 返回 `{policyDocHash, findings}`。
pub(crate) fn policy_submit_draft_inner(kernel: &mut Kernel, doc: &Value) -> Result<Value, String> {
    kernel.policy_submit_draft(doc).map_err(err)
}

/// 发布策略草稿：草稿附组织签名包（OrgSigSet）落 `org:policydoc:` 键域
/// （org:structure@v1，随组织同步分发；入站合入以同一五步链把关）。
/// 返回 `{orgId, policyDocHash, publishedAt, signer, degraded}`。
pub(crate) fn policy_publish_inner(kernel: &mut Kernel, org_id: &str) -> Result<Value, String> {
    kernel.policy_publish(org_id).map_err(err)
}

// ------------------------------------------------------------------
// Tauri 命令
// ------------------------------------------------------------------

#[tauri::command]
pub fn plugin_policy_read(
    state: tauri::State<'_, KernelState>,
    org_id: String,
) -> Result<Option<Value>, String> {
    policy_read_inner(&*lock_kernel(&state)?, &org_id)
}

#[tauri::command]
pub fn plugin_policy_submit_draft(
    state: tauri::State<'_, KernelState>,
    doc: Value,
) -> Result<Value, String> {
    policy_submit_draft_inner(&mut *lock_kernel(&state)?, &doc)
}

#[tauri::command]
pub fn plugin_policy_publish(
    state: tauri::State<'_, KernelState>,
    org_id: String,
) -> Result<Value, String> {
    policy_publish_inner(&mut *lock_kernel(&state)?, &org_id)
}

// ------------------------------------------------------------------
// 单元测试：直调 *_inner，不依赖 WebView
// ------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use spark_core::kernel::KernelConfig;

    fn unlocked_kernel() -> (tempfile::TempDir, Kernel) {
        let dir = tempfile::tempdir().unwrap();
        let mut kernel = Kernel::init(KernelConfig {
            data_dir: dir.path().to_path_buf(),
            app_version: "0.0.0-test".to_string(),
            p2p: None,
        })
        .unwrap();
        kernel.init_identity("pw-test-policy", "alice", None).unwrap();
        (dir, kernel)
    }

    #[test]
    fn read_rejects_invalid_org_id_and_returns_null_when_absent() {
        let (_dir, kernel) = unlocked_kernel();

        let err = policy_read_inner(&kernel, "not-an-org-id").unwrap_err();
        assert!(err.contains("invalid orgId"), "unexpected: {err}");

        // 合法 orgId 但无草稿 → null
        assert_eq!(
            policy_read_inner(&kernel, "org_0123456789abcdef").unwrap(),
            None
        );
    }

    #[test]
    fn submit_draft_rejects_malformed_doc() {
        let (_dir, mut kernel) = unlocked_kernel();

        // 结构不合 PolicyDoc → malformed policy doc
        let err = policy_submit_draft_inner(&mut kernel, &serde_json::json!({ "policyV": 1 }))
            .unwrap_err();
        assert!(err.contains("malformed policy doc"), "unexpected: {err}");

        // b1 结构校验：引擎非 b1 → fail-closed 拒绝
        let doc = serde_json::json!({
            "policyV": 1,
            "engine": "cedar",
            "orgId": "org_0123456789abcdef",
            "roster": { "visibility": "org-only" },
            "updatedAt": 1
        });
        let err = policy_submit_draft_inner(&mut kernel, &doc).unwrap_err();
        assert!(!err.is_empty());
    }

    #[test]
    fn publish_requires_draft_and_valid_org_id() {
        let (_dir, mut kernel) = unlocked_kernel();

        let err = policy_publish_inner(&mut kernel, "not-an-org-id").unwrap_err();
        assert!(err.contains("invalid orgId"), "unexpected: {err}");

        // 合法 orgId 但无草稿 → 如实报错（不产出无源发布件）
        let err = policy_publish_inner(&mut kernel, "org_0123456789abcdef").unwrap_err();
        assert!(err.contains("no policy draft"), "unexpected: {err}");
    }
}

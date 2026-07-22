//! 存证查询命令（ipc/db.ts `evidence-head-hash` / `evidence-verify`；按 seq 查为
//! 内核能力的补全暴露，preload 暂无对应通道）。

use serde::Serialize;
use spark_core::evidence::EvidenceEntry;
use spark_core::kernel::Kernel;

use super::{err, lock_kernel};
use crate::KernelState;

/// `evidence-head-hash` 返回。
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct HeadHashResultDto {
    pub hash: Option<String>,
}

/// `evidence-verify` 返回（TS `{valid, height}`）。
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct VerifyResultDto {
    pub valid: bool,
    pub height: u64,
}

// ------------------------------------------------------------------
// 核心实现（测试直调）
// ------------------------------------------------------------------

pub(crate) fn head_hash_inner(kernel: &Kernel) -> Result<HeadHashResultDto, String> {
    let hash = kernel.evidence_head_hash().map_err(err)?;
    Ok(HeadHashResultDto { hash })
}

pub(crate) fn verify_inner(kernel: &Kernel) -> Result<VerifyResultDto, String> {
    let status = kernel.evidence_verify().map_err(err)?;
    Ok(VerifyResultDto {
        valid: status.valid,
        height: status.height,
    })
}

pub(crate) fn entry_inner(kernel: &Kernel, seq: u64) -> Result<Option<EvidenceEntry>, String> {
    kernel.evidence_entry(seq).map_err(err)
}

// ------------------------------------------------------------------
// Tauri 命令
// ------------------------------------------------------------------

#[tauri::command]
pub fn evidence_head_hash(
    state: tauri::State<'_, KernelState>,
) -> Result<HeadHashResultDto, String> {
    head_hash_inner(&*lock_kernel(&state)?)
}

#[tauri::command]
pub fn evidence_verify(
    state: tauri::State<'_, KernelState>,
) -> Result<VerifyResultDto, String> {
    verify_inner(&*lock_kernel(&state)?)
}

#[tauri::command]
pub fn evidence_entry(
    state: tauri::State<'_, KernelState>,
    seq: u64,
) -> Result<Option<EvidenceEntry>, String> {
    entry_inner(&*lock_kernel(&state)?, seq)
}

// ------------------------------------------------------------------
// 单元测试
// ------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use spark_core::collection::CollectionConfig;
    use spark_core::kernel::KernelConfig;
    use spark_core::schema::{CollectionSchemaDeclaration, SyncStrategy};

    fn unlocked_kernel() -> (tempfile::TempDir, Kernel) {
        let dir = tempfile::tempdir().unwrap();
        let mut kernel = Kernel::init(KernelConfig {
            data_dir: dir.path().to_path_buf(),
            app_version: "0.0.0-test".to_string(),
            p2p: None,
        })
        .unwrap();
        kernel.init_identity("correct-horse-battery", "alice", None).unwrap();
        (dir, kernel)
    }

    #[test]
    fn empty_chain_then_entries_after_writes() {
        let (_dir, mut kernel) = unlocked_kernel();

        // 空链
        assert_eq!(head_hash_inner(&kernel).unwrap().hash, None);
        let verify = verify_inner(&kernel).unwrap();
        assert!(verify.valid && verify.height == 0);
        assert!(entry_inner(&kernel, 1).unwrap().is_none());

        // 写入带存证集合 → 高度 1，head 非空，按 seq 可查
        kernel
            .declare_collection(
                "plugin:app",
                "notes",
                CollectionSchemaDeclaration {
                    sync_strategy: Some(SyncStrategy::Lww),
                    governance: false,
                    enable_evidence: true,
                },
            )
            .unwrap();
        kernel
            .doc_put(
                "plugin:app",
                "notes",
                "n1",
                serde_json::json!({"v": 1}),
                CollectionConfig::default(),
            )
            .unwrap();

        let head = head_hash_inner(&kernel).unwrap();
        assert_eq!(head.hash.as_deref().map(str::len), Some(64));
        let verify = verify_inner(&kernel).unwrap();
        assert!(verify.valid && verify.height == 1);
        let entry = entry_inner(&kernel, 1).unwrap().unwrap();
        assert_eq!(entry.domain, "plugin:app");
        assert_eq!(entry.id, "n1");
        assert_eq!(entry.op.as_str(), "put");
        assert!(entry_inner(&kernel, 2).unwrap().is_none());
    }
}

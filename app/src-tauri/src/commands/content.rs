//! 内容面命令（public-topics §七「持有即做种」）：blob 按 CID 的存/取/拉取、
//! GC 根标记与两段式回收。blob 本体一律 base64 过 IPC（与内核 content API
//! 入出参一致）；拉取路径（Kad 检索 + 直连 fetch）为网络阻塞操作，走
//! `run_kernel` 挪阻塞线程池（模式同 identity.rs 的 KDF 命令）。

use spark_core::content::ContentBlobInfo;
use spark_core::kernel::Kernel;

use super::dto::SuccessResult;
use super::{err, lock_kernel, run_kernel};
use crate::KernelState;

// ------------------------------------------------------------------
// 核心实现（测试直调）
// ------------------------------------------------------------------

/// `content-save-blob`：幂等落本地 content store 并按持有即做种声明 provider
/// （p2p 未启动仅落本地；leaf 的 provide 拒绝属预期，静默放过）。
pub(crate) fn save_blob_inner(
    kernel: &mut Kernel,
    data_base64: &str,
) -> Result<ContentBlobInfo, String> {
    kernel.content_save_blob(data_base64).map_err(err)
}

/// `content-read-blob`：本地命中 → base64；未命中 → null（不触发网络拉取）。
pub(crate) fn read_blob_inner(kernel: &Kernel, cid: &str) -> Result<Option<String>, String> {
    kernel.content_read_blob(cid).map_err(err)
}

/// `content-fetch-blob`：本地未命中时经 Kad provider 逐台拉取（接收侧 CID
/// 哈希校验 + 落库 + 自动做种在 p2p 层完成）；全部失败返回 null。
pub(crate) fn fetch_blob_inner(kernel: &mut Kernel, cid: &str) -> Result<Option<String>, String> {
    kernel.content_fetch_blob(cid).map_err(err)
}

/// `content-list-blobs`：本地持有的全部 blob CID（升序）。
pub(crate) fn list_blobs_inner(kernel: &Kernel) -> Result<Vec<String>, String> {
    kernel.content_list_blobs().map_err(err)
}

/// `content-pin-root`：打 GC 根标记（`root` 为持有理由标签，如 `topic:{id}`）。
pub(crate) fn pin_root_inner(
    kernel: &mut Kernel,
    cid: &str,
    root: &str,
) -> Result<SuccessResult, String> {
    kernel.content_pin_root(cid, root).map_err(err)?;
    Ok(SuccessResult::ok())
}

/// `content-unpin-root`：移除一个 GC 根标记；最后一个根移除后进入宽限期
/// 回收路径。
pub(crate) fn unpin_root_inner(
    kernel: &mut Kernel,
    cid: &str,
    root: &str,
) -> Result<SuccessResult, String> {
    kernel.content_unpin_root(cid, root).map_err(err)?;
    Ok(SuccessResult::ok())
}

/// `content-gc-sweep`：无根 blob 两段式回收；每回收一个本体即同步停止做种
/// （GC↔provider 衔接在内核落地）。返回回收的 CID 列表。
pub(crate) fn gc_sweep_inner(kernel: &mut Kernel) -> Result<Vec<String>, String> {
    kernel.content_gc_sweep().map_err(err)
}

// ------------------------------------------------------------------
// Tauri 命令
// ------------------------------------------------------------------

#[tauri::command]
pub async fn content_save_blob(
    state: tauri::State<'_, KernelState>,
    data_base64: String,
) -> Result<ContentBlobInfo, String> {
    run_kernel(state, move |kernel| save_blob_inner(kernel, &data_base64)).await
}

#[tauri::command]
pub fn content_read_blob(
    state: tauri::State<'_, KernelState>,
    cid: String,
) -> Result<Option<String>, String> {
    read_blob_inner(&*lock_kernel(&state)?, &cid)
}

/// 网络阻塞（Kad 检索 + 逐 provider 直连拉取），async + run_kernel。
#[tauri::command]
pub async fn content_fetch_blob(
    state: tauri::State<'_, KernelState>,
    cid: String,
) -> Result<Option<String>, String> {
    run_kernel(state, move |kernel| fetch_blob_inner(kernel, &cid)).await
}

#[tauri::command]
pub fn content_list_blobs(state: tauri::State<'_, KernelState>) -> Result<Vec<String>, String> {
    list_blobs_inner(&*lock_kernel(&state)?)
}

#[tauri::command]
pub fn content_pin_root(
    state: tauri::State<'_, KernelState>,
    cid: String,
    root: String,
) -> Result<SuccessResult, String> {
    pin_root_inner(&mut *lock_kernel(&state)?, &cid, &root)
}

#[tauri::command]
pub fn content_unpin_root(
    state: tauri::State<'_, KernelState>,
    cid: String,
    root: String,
) -> Result<SuccessResult, String> {
    unpin_root_inner(&mut *lock_kernel(&state)?, &cid, &root)
}

/// GC 回收内含 stop_providing 的 block_on，async + run_kernel。
#[tauri::command]
pub async fn content_gc_sweep(state: tauri::State<'_, KernelState>) -> Result<Vec<String>, String> {
    run_kernel(state, |kernel| gc_sweep_inner(kernel)).await
}

// ------------------------------------------------------------------
// 单元测试（不启动真实网络：验证存取幂等、根标记、GC 与非法 CID 拒绝）
// ------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD as B64;
    use spark_core::kernel::KernelConfig;

    use super::*;

    fn kernel_with_identity() -> (tempfile::TempDir, Kernel) {
        let dir = tempfile::tempdir().unwrap();
        let mut kernel = Kernel::init(KernelConfig {
            data_dir: dir.path().to_path_buf(),
            app_version: "0.0.0-test".to_string(),
            p2p: None,
        })
        .unwrap();
        kernel
            .init_identity("correct-horse-battery", "alice", None)
            .unwrap();
        (dir, kernel)
    }

    #[test]
    fn save_read_list_pin_gc_roundtrip() {
        let (_dir, mut kernel) = kernel_with_identity();
        let data = b"topic attachment v1".repeat(64);
        let info = save_blob_inner(&mut kernel, &B64.encode(&data)).unwrap();
        // 幂等：重复保存同内容同 CID
        let again = save_blob_inner(&mut kernel, &B64.encode(&data)).unwrap();
        assert_eq!(info, again);
        assert_eq!(info.size, data.len() as u64);

        // p2p 未启动时 read 命中；未命中为 null
        assert_eq!(
            read_blob_inner(&kernel, info.cid.as_str()).unwrap(),
            Some(B64.encode(&data))
        );
        let missing = spark_core::content::Cid::from_data(b"never saved");
        assert_eq!(read_blob_inner(&kernel, missing.as_str()).unwrap(), None);
        assert_eq!(list_blobs_inner(&kernel).unwrap(), vec![info.cid.to_string()]);

        // 有根标记时 GC 不回收；移除根后经宽限期才可回收（宽限期内不回收）
        pin_root_inner(&mut kernel, info.cid.as_str(), "topic:demo").unwrap();
        assert!(gc_sweep_inner(&mut kernel).unwrap().is_empty());
        unpin_root_inner(&mut kernel, info.cid.as_str(), "topic:demo").unwrap();
        assert!(
            gc_sweep_inner(&mut kernel).unwrap().is_empty(),
            "宽限期内不回收"
        );
        assert!(
            read_blob_inner(&kernel, info.cid.as_str()).unwrap().is_some()
        );
    }

    #[test]
    fn rejects_malformed_cid_and_bad_base64() {
        let (_dir, mut kernel) = kernel_with_identity();
        assert!(save_blob_inner(&mut kernel, "not base64!!!").is_err());
        assert!(
            read_blob_inner(&kernel, "not-a-cid")
                .unwrap_err()
                .contains("invalid cid")
        );
        assert!(
            pin_root_inner(&mut kernel, "ABC", "topic:x")
                .unwrap_err()
                .contains("invalid cid")
        );
        // p2p 未启动时 fetch 本地未命中 → null（不报错）
        let missing = spark_core::content::Cid::from_data(b"nowhere");
        assert_eq!(fetch_blob_inner(&mut kernel, missing.as_str()).unwrap(), None);
    }
}

//! 共同体事务命令（community-affairs §7.2：`sdk.affairs` 壳层薄壳）。
//!
//! 薄壳透传直通内核 `affair_ops` 门面，只做 err 映射与锁内核；权限
//! （`affairs:read` / `affairs:write`）在桥 dispatcher 层强制（对齐既有
//! 插件命令惯例：壳层命令薄壳不做权限）。affairId 由创世记录自认证复算，
//! 不信调用方自报（内核门面校验）。

use serde_json::Value;
use spark_core::kernel::Kernel;

use super::{err, lock_kernel};
use crate::KernelState;

/// 关注事务（genesis 创世记录全链校验；返回自认证 affairId）。
pub(crate) fn affairs_follow_inner(kernel: &mut Kernel, genesis: &Value) -> Result<String, String> {
    kernel.affair_follow(genesis).map_err(err)
}

/// 取关事务（只删关注簿记，保留已复制数据）。
pub(crate) fn affairs_unfollow_inner(kernel: &mut Kernel, affair_id: &str) -> Result<(), String> {
    kernel.affair_unfollow(affair_id).map_err(err)
}

/// 本机关注的事务 id 列表（字典序）。
pub(crate) fn affairs_list_followed_inner(kernel: &Kernel) -> Result<Vec<String>, String> {
    kernel.affair_list_followed().map_err(err)
}

/// 提交一条操作（返回 `{affairId, opHash, status}`）。
pub(crate) fn affairs_submit_op_inner(kernel: &mut Kernel, op: &Value) -> Result<Value, String> {
    kernel.affair_submit_op(op).map_err(err)
}

/// 读本地副本操作日志。
pub(crate) fn affairs_read_log_inner(kernel: &Kernel, affair_id: &str) -> Result<Value, String> {
    kernel.affair_read_log(affair_id).map_err(err)
}

/// 读规则文档版本链（现行版本 + 未生效条目归宿，规则 replay 的读出口）。
pub(crate) fn affairs_read_rules_inner(kernel: &Kernel, affair_id: &str) -> Result<Value, String> {
    kernel.affair_read_rules(affair_id).map_err(err)
}

/// 读决议（公示期状态按链上锚定时刻推导）。
pub(crate) fn affairs_read_resolution_inner(kernel: &Kernel, affair_id: &str) -> Result<Value, String> {
    kernel.affair_read_resolution(affair_id).map_err(err)
}

/// 阶梯快照 payload 生产助手（§9 投票前快照；asOf 缺省 = 当前最大 opHash）。
pub(crate) fn affairs_snapshot_payload_inner(
    kernel: &Kernel,
    affair_id: &str,
    as_of: Option<&str>,
) -> Result<Value, String> {
    kernel.affair_snapshot_payload(affair_id, as_of).map_err(err)
}

/// 执行型事务状态读路径（八态状态机推导；exec == null 的事务如实返回空状态集）。
pub(crate) fn affairs_read_exec_inner(kernel: &Kernel, affair_id: &str) -> Result<Value, String> {
    kernel.affair_read_exec(affair_id).map_err(err)
}

/// 决议组织效力钩子（产出待应用事件并标注回执状态 recorded/unrecorded）。
pub(crate) fn affairs_org_effects_inner(
    kernel: &Kernel,
    org_id: &str,
    affair_id: &str,
) -> Result<Value, String> {
    kernel.affair_org_effects(org_id, affair_id).map_err(err)
}

/// 待应用效力事件的消费编排（写 org:effectrcpt: 回执并逐条存证，幂等、LWW；
/// 名册/策略内容的实际应用归插件/组织侧，须在调用本门面前完成）。
pub(crate) fn affairs_apply_org_effects_inner(
    kernel: &mut Kernel,
    org_id: &str,
    affair_id: &str,
) -> Result<Value, String> {
    kernel.affair_apply_org_effects(org_id, affair_id).map_err(err)
}

/// 阶梯/账龄状态（确定性推导）。
pub(crate) fn affairs_ladder_status_inner(kernel: &Kernel, affair_id: &str) -> Result<Value, String> {
    kernel.affair_ladder_status(affair_id).map_err(err)
}

/// 公开履历聚合（公共身份跨事务账龄/采纳/投票历史，确定性推导）。
pub(crate) fn affairs_public_profile_inner(kernel: &Kernel, identity: &str) -> Result<Value, String> {
    kernel.affair_public_profile(identity).map_err(err)
}

// ------------------------------------------------------------------
// indexer 角色（affair-metadata §7 目录面 + §8 查询）：配置 / 目录 / 查询薄壳
// ------------------------------------------------------------------

/// 启用/关闭 indexer 角色（只动启用位，保留覆盖配置）。
pub(crate) fn affairs_set_indexer_enabled_inner(
    kernel: &mut Kernel,
    enabled: bool,
) -> Result<(), String> {
    kernel.set_indexer_enabled(enabled);
    Ok(())
}

/// 配置子集覆盖（regions/topics 双维度，空列表 = 该维度不限）。
pub(crate) fn affairs_set_indexer_coverage_inner(
    kernel: &mut Kernel,
    regions: Vec<String>,
    topics: Vec<String>,
) -> Result<(), String> {
    kernel.set_indexer_coverage(regions, topics).map_err(err)
}

/// 当前 indexer 角色配置（enabled + regions/topics）。
pub(crate) fn affairs_indexer_config_inner(kernel: &Kernel) -> Result<Value, String> {
    let cfg = kernel.indexer_config();
    Ok(serde_json::json!({
        "enabled": cfg.enabled,
        "regions": cfg.coverage.regions,
        "topics": cfg.coverage.topics,
    }))
}

/// 本地 indexer 目录（新鲜名片条目；轻客户端发现可用 indexer 的读路径）。
pub(crate) fn affairs_indexer_directory_inner(kernel: &Kernel) -> Result<Vec<Value>, String> {
    kernel.indexer_directory().map_err(err)
}

/// 向目录选定的 indexer 发查询帧（未连接先按邻居池地址直连），返回响应帧文本。
pub(crate) fn affairs_indexer_query_inner(
    kernel: &Kernel,
    peer_id: &str,
    request: &str,
) -> Result<String, String> {
    kernel.indexer_query(peer_id, request).map_err(err)
}

// ------------------------------------------------------------------
// Tauri 命令
// ------------------------------------------------------------------

#[tauri::command]
pub fn plugin_affairs_follow(
    state: tauri::State<'_, KernelState>,
    genesis: Value,
) -> Result<String, String> {
    affairs_follow_inner(&mut *lock_kernel(&state)?, &genesis)
}

#[tauri::command]
pub fn plugin_affairs_unfollow(
    state: tauri::State<'_, KernelState>,
    affair_id: String,
) -> Result<(), String> {
    affairs_unfollow_inner(&mut *lock_kernel(&state)?, &affair_id)
}

#[tauri::command]
pub fn plugin_affairs_list_followed(
    state: tauri::State<'_, KernelState>,
) -> Result<Vec<String>, String> {
    affairs_list_followed_inner(&*lock_kernel(&state)?)
}

#[tauri::command]
pub fn plugin_affairs_submit_op(
    state: tauri::State<'_, KernelState>,
    op: Value,
) -> Result<Value, String> {
    affairs_submit_op_inner(&mut *lock_kernel(&state)?, &op)
}

#[tauri::command]
pub fn plugin_affairs_read_log(
    state: tauri::State<'_, KernelState>,
    affair_id: String,
) -> Result<Value, String> {
    affairs_read_log_inner(&*lock_kernel(&state)?, &affair_id)
}

#[tauri::command]
pub fn plugin_affairs_read_rules(
    state: tauri::State<'_, KernelState>,
    affair_id: String,
) -> Result<Value, String> {
    affairs_read_rules_inner(&*lock_kernel(&state)?, &affair_id)
}

#[tauri::command]
pub fn plugin_affairs_read_resolution(
    state: tauri::State<'_, KernelState>,
    affair_id: String,
) -> Result<Value, String> {
    affairs_read_resolution_inner(&*lock_kernel(&state)?, &affair_id)
}

#[tauri::command]
pub fn plugin_affairs_ladder_status(
    state: tauri::State<'_, KernelState>,
    affair_id: String,
) -> Result<Value, String> {
    affairs_ladder_status_inner(&*lock_kernel(&state)?, &affair_id)
}

#[tauri::command]
pub fn plugin_affairs_public_profile(
    state: tauri::State<'_, KernelState>,
    identity: String,
) -> Result<Value, String> {
    affairs_public_profile_inner(&*lock_kernel(&state)?, &identity)
}

#[tauri::command]
pub fn plugin_affairs_snapshot_payload(
    state: tauri::State<'_, KernelState>,
    affair_id: String,
    as_of: Option<String>,
) -> Result<Value, String> {
    affairs_snapshot_payload_inner(&*lock_kernel(&state)?, &affair_id, as_of.as_deref())
}

#[tauri::command]
pub fn plugin_affairs_read_exec(
    state: tauri::State<'_, KernelState>,
    affair_id: String,
) -> Result<Value, String> {
    affairs_read_exec_inner(&*lock_kernel(&state)?, &affair_id)
}

#[tauri::command]
pub fn plugin_affairs_org_effects(
    state: tauri::State<'_, KernelState>,
    org_id: String,
    affair_id: String,
) -> Result<Value, String> {
    affairs_org_effects_inner(&*lock_kernel(&state)?, &org_id, &affair_id)
}

#[tauri::command]
pub fn plugin_affairs_apply_org_effects(
    state: tauri::State<'_, KernelState>,
    org_id: String,
    affair_id: String,
) -> Result<Value, String> {
    affairs_apply_org_effects_inner(&mut *lock_kernel(&state)?, &org_id, &affair_id)
}

#[tauri::command]
pub fn plugin_affairs_set_indexer_enabled(
    state: tauri::State<'_, KernelState>,
    enabled: bool,
) -> Result<(), String> {
    affairs_set_indexer_enabled_inner(&mut *lock_kernel(&state)?, enabled)
}

#[tauri::command]
pub fn plugin_affairs_set_indexer_coverage(
    state: tauri::State<'_, KernelState>,
    regions: Vec<String>,
    topics: Vec<String>,
) -> Result<(), String> {
    affairs_set_indexer_coverage_inner(&mut *lock_kernel(&state)?, regions, topics)
}

#[tauri::command]
pub fn plugin_affairs_indexer_config(
    state: tauri::State<'_, KernelState>,
) -> Result<Value, String> {
    affairs_indexer_config_inner(&*lock_kernel(&state)?)
}

#[tauri::command]
pub fn plugin_affairs_indexer_directory(
    state: tauri::State<'_, KernelState>,
) -> Result<Vec<Value>, String> {
    affairs_indexer_directory_inner(&*lock_kernel(&state)?)
}

#[tauri::command]
pub fn plugin_affairs_indexer_query(
    state: tauri::State<'_, KernelState>,
    peer_id: String,
    request: String,
) -> Result<String, String> {
    affairs_indexer_query_inner(&*lock_kernel(&state)?, &peer_id, &request)
}

// ------------------------------------------------------------------
// 单元测试：直调 *_inner，不依赖 WebView
// ------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use spark_core::kernel::KernelConfig;

    fn temp_kernel() -> (tempfile::TempDir, Kernel) {
        let dir = tempfile::tempdir().unwrap();
        let kernel = Kernel::init(KernelConfig {
            data_dir: dir.path().to_path_buf(),
            app_version: "0.0.0-test".to_string(),
            p2p: None,
        })
        .unwrap();
        (dir, kernel)
    }

    #[test]
    fn list_followed_empty_and_follow_rejects_bad_genesis() {
        let (_dir, mut kernel) = temp_kernel();
        // 无存储时关注簿记读不到（未解锁/未开库）；解锁后为空列表
        kernel
            .init_identity("pw-test-affairs", "alice", None)
            .unwrap();
        assert_eq!(affairs_list_followed_inner(&kernel).unwrap(), Vec::<String>::new());

        // 非法创世记录 → 内核拒绝（不等同于空成功）
        let err = affairs_follow_inner(
            &mut kernel,
            &serde_json::json!({ "kind": "not-a-genesis" }),
        )
        .unwrap_err();
        assert!(!err.is_empty());
    }

    #[test]
    fn submit_op_rejects_malformed_and_unknown_affair() {
        let (_dir, mut kernel) = temp_kernel();
        kernel
            .init_identity("pw-test-affairs", "alice", None)
            .unwrap();

        // 缺 affairId → invalid affairId
        let err = affairs_submit_op_inner(&mut kernel, &serde_json::json!({ "opType": "vote" }))
            .unwrap_err();
        assert!(err.contains("invalid affairId"), "unexpected: {err}");

        // 形态合法但未关注/未知事务 → 入站拒收
        let op = serde_json::json!({
            "affairId": "af_0123456789abcdef0123456789abcdef",
            "opType": "vote",
            "payload": {}
        });
        let err = affairs_submit_op_inner(&mut kernel, &op).unwrap_err();
        assert!(!err.is_empty());
    }

    #[test]
    fn read_log_and_ladder_reject_invalid_affair_id() {
        let (_dir, mut kernel) = temp_kernel();
        kernel
            .init_identity("pw-test-affairs", "alice", None)
            .unwrap();

        let err = affairs_read_log_inner(&kernel, "not-an-affair-id").unwrap_err();
        assert!(err.contains("invalid affairId"), "unexpected: {err}");
        let err = affairs_ladder_status_inner(&kernel, "not-an-affair-id").unwrap_err();
        assert!(err.contains("invalid affairId"), "unexpected: {err}");
        let err = affairs_read_resolution_inner(&kernel, "not-an-affair-id").unwrap_err();
        assert!(err.contains("invalid affairId"), "unexpected: {err}");
        // 公开履历：身份线形 fail-closed；合法身份空视图而非错误
        let err = affairs_public_profile_inner(&kernel, "not-an-identity").unwrap_err();
        assert!(err.contains("invalid identity"), "unexpected: {err}");
        let out = affairs_public_profile_inner(&kernel, &"ab".repeat(32)).unwrap();
        assert_eq!(out["affairsParticipated"], serde_json::json!(0));
    }

    /// 新透出五个门面薄壳：线形校验失败透传内核报错（非静默成功）。
    #[test]
    fn new_facades_reject_invalid_ids() {
        let (_dir, mut kernel) = temp_kernel();
        kernel
            .init_identity("pw-test-affairs", "alice", None)
            .unwrap();

        let err = affairs_read_rules_inner(&kernel, "not-an-affair-id").unwrap_err();
        assert!(err.contains("invalid affairId"), "unexpected: {err}");
        let err = affairs_read_exec_inner(&kernel, "not-an-affair-id").unwrap_err();
        assert!(err.contains("invalid affairId"), "unexpected: {err}");
        let err = affairs_snapshot_payload_inner(&kernel, "not-an-affair-id", None).unwrap_err();
        assert!(err.contains("invalid affairId"), "unexpected: {err}");
        // 形态合法但未关注/未知事务 → 创世缺失报错（asOf 校验在求值装配之后）
        let err = affairs_snapshot_payload_inner(&kernel, &"ab".repeat(32), Some("bad-as-of"))
            .unwrap_err();
        assert!(err.contains("unknown affair"), "unexpected: {err}");
        // org 效力门面：orgId / affairId 双形态校验
        let err = affairs_org_effects_inner(&kernel, "not-an-org", &"ab".repeat(32)).unwrap_err();
        assert!(err.contains("invalid orgId"), "unexpected: {err}");
        let err = affairs_org_effects_inner(&kernel, &format!("org_{}", "cd".repeat(8)), "bad")
            .unwrap_err();
        assert!(err.contains("invalid affairId"), "unexpected: {err}");
        let err =
            affairs_apply_org_effects_inner(&mut kernel, "not-an-org", &"ab".repeat(32)).unwrap_err();
        assert!(err.contains("invalid orgId"), "unexpected: {err}");
    }

    /// indexer 角色薄壳：配置读写直通内核门面，线形校验失败透传报错。
    #[test]
    fn indexer_config_shell_passthrough() {
        let (_dir, mut kernel) = temp_kernel();
        kernel
            .init_identity("pw-test-affairs", "alice", None)
            .unwrap();

        // 默认关闭 + 全覆盖
        let cfg = affairs_indexer_config_inner(&kernel).unwrap();
        assert_eq!(cfg["enabled"], serde_json::json!(false));
        assert_eq!(cfg["regions"], serde_json::json!([]));

        // 覆盖配置生效；启用位独立
        affairs_set_indexer_coverage_inner(
            &mut kernel,
            vec!["110105".to_string()],
            vec!["hoa".to_string()],
        )
        .unwrap();
        affairs_set_indexer_enabled_inner(&mut kernel, true).unwrap();
        let cfg = affairs_indexer_config_inner(&kernel).unwrap();
        assert_eq!(cfg["enabled"], serde_json::json!(true));
        assert_eq!(cfg["regions"], serde_json::json!(["110105"]));
        assert_eq!(cfg["topics"], serde_json::json!(["hoa"]));

        // 线形越限 → 内核拒绝
        let err =
            affairs_set_indexer_coverage_inner(&mut kernel, vec!["x".repeat(33)], vec![])
                .unwrap_err();
        assert!(err.contains("invalid coverage"), "unexpected: {err}");

        // 目录读路径（无名片 → 空）
        assert_eq!(
            affairs_indexer_directory_inner(&kernel).unwrap(),
            Vec::<Value>::new()
        );
        // 查询未连接 peer：报错而非静默成功
        let err = affairs_indexer_query_inner(&kernel, "12D3KooWUnknown", "{}").unwrap_err();
        assert!(!err.is_empty());
    }
}

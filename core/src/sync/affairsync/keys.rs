//! affair 复制面同步层簿记键构造函数（wiki/protocol/community/affair-sync.md §5）。
//!
//! 与 affair 模块的数据键（`affair:rec:`/`affair:op:`/`affair:head:`/`affair:follow:`，
//! 见 core/src/affair/keys.rs）分属两层：本面层键是复制流量衍生的簿记（关注者
//! 目录 + 乱序暂存），不进任何同步流量。前缀 `affairsync:` 非 `org:`/`orgd:`，
//! orgsync-data 白名单天然拒收；反向红线（拒收 `org:` 键）在 envelope/apply 层执行。

use crate::affair::is_valid_identity_id;

/// 关注者目录键前缀：`affairsync:dir:`。
pub const AFFAIRSYNC_DIR_PREFIX: &str = "affairsync:dir:";
/// 乱序暂存键前缀：`affairsync:pend:`。
pub const AFFAIRSYNC_PEND_PREFIX: &str = "affairsync:pend:";

/// `affairsync:dir:{affairId}:`：某事务的关注者目录键域（扫描用）。
pub fn affair_dir_prefix(affair_id: &str) -> String {
    debug_assert!(is_valid_identity_id(affair_id));
    format!("{AFFAIRSYNC_DIR_PREFIX}{affair_id}:")
}

/// `affairsync:dir:{affairId}:{rootId}`：关注者目录单条。
pub fn affair_dir_key(affair_id: &str, root_id: &str) -> String {
    debug_assert!(is_valid_identity_id(affair_id) && is_valid_identity_id(root_id));
    format!("{AFFAIRSYNC_DIR_PREFIX}{affair_id}:{root_id}")
}

/// `affairsync:pend:{affairId}:`：乱序暂存键域（扫描用）。
pub fn affair_pend_prefix(affair_id: &str) -> String {
    debug_assert!(is_valid_identity_id(affair_id));
    format!("{AFFAIRSYNC_PEND_PREFIX}{affair_id}:")
}

/// `affairsync:pend:{affairId}:{opHash}`：乱序暂存单条。
pub fn affair_pend_key(affair_id: &str, op_hash: &str) -> String {
    debug_assert!(is_valid_identity_id(affair_id) && is_valid_identity_id(op_hash));
    format!("{AFFAIRSYNC_PEND_PREFIX}{affair_id}:{op_hash}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_shapes() {
        let id = "ab".repeat(32);
        let hash = "cd".repeat(32);
        assert_eq!(
            affair_dir_key(&id, &hash),
            format!("affairsync:dir:{id}:{hash}")
        );
        assert_eq!(
            affair_pend_key(&id, &hash),
            format!("affairsync:pend:{id}:{hash}")
        );
        // 分域红线：簿记键不以 affair 数据键域/org 键域开头
        assert!(affair_dir_key(&id, &hash).starts_with("affairsync:"));
        assert!(!affair_dir_key(&id, &hash).starts_with("org:"));
        assert!(!affair_pend_key(&id, &hash).starts_with("affair:"));
    }
}

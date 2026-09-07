//! 本地存储键构造函数（wiki/protocol/community/affair.md §3.3，关注者副本）。
//!
//! 键前缀 `affair:` 与 `org:tx:`/`org:` 严格分域：orgsync-data 白名单拒收
//! affair 键，affair 复制面（C4）拒收 `org:` 键。本模块只做键构造（纯函数），
//! 读写归存储层/调用方。

use super::actor::is_valid_identity_id;

/// 创世记录键前缀：`affair:rec:`。
pub const AFFAIR_RECORD_PREFIX: &str = "affair:rec:";
/// 操作条目键前缀：`affair:op:`。
pub const AFFAIR_OP_PREFIX: &str = "affair:op:";
/// DAG 头集合键前缀：`affair:head:`。
pub const AFFAIR_HEAD_PREFIX: &str = "affair:head:";
/// 本地关注状态键前缀（本地键，不进任何同步流量）。
pub const AFFAIR_FOLLOW_PREFIX: &str = "affair:follow:";

/// `affair:rec:{affairId}`：创世记录，每事务一条。
pub fn affair_record_key(affair_id: &str) -> String {
    debug_assert!(is_valid_identity_id(affair_id));
    format!("{AFFAIR_RECORD_PREFIX}{affair_id}")
}

/// `affair:op:{affairId}:{opHash}`：操作条目，append-only。
pub fn affair_op_key(affair_id: &str, op_hash: &str) -> String {
    debug_assert!(is_valid_identity_id(affair_id) && is_valid_identity_id(op_hash));
    format!("{AFFAIR_OP_PREFIX}{affair_id}:{op_hash}")
}

/// `affair:head:{affairId}`：本地观察到的 DAG 头集合。
pub fn affair_head_key(affair_id: &str) -> String {
    debug_assert!(is_valid_identity_id(affair_id));
    format!("{AFFAIR_HEAD_PREFIX}{affair_id}")
}

/// `affair:follow:{affairId}`：本地关注状态（不进同步）。
pub fn affair_follow_key(affair_id: &str) -> String {
    debug_assert!(is_valid_identity_id(affair_id));
    format!("{AFFAIR_FOLLOW_PREFIX}{affair_id}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_shapes() {
        let id = "ab".repeat(32);
        let hash = "cd".repeat(32);
        assert_eq!(affair_record_key(&id), format!("affair:rec:{id}"));
        assert_eq!(affair_op_key(&id, &hash), format!("affair:op:{id}:{hash}"));
        assert_eq!(affair_head_key(&id), format!("affair:head:{id}"));
        assert_eq!(affair_follow_key(&id), format!("affair:follow:{id}"));
        // 分域红线：affair 键不以 org: 开头
        assert!(!affair_record_key(&id).starts_with("org:"));
    }
}

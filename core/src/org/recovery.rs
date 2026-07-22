//! org-recovery 恢复 token（对齐 org.md §10 / p2p-messages.md §8.1，
//! org-recovery.ts:33-41）。
//!
//! ```text
//! timeBucket = floor(nowMs / 600000)                       // 10 分钟桶
//! token      = sha256hex(`${orgId}:${recoverySecret}:${timeBucket}`)
//! ```
//!
//! 输入字节 = 冒号拼接字符串的 UTF-8；输出 = 64 字符小写 hex。
//! 有效 token 集合 = 当前桶 + 上一桶（消除桶边界漏配）；发起查询用当前桶 token。

use sha2::{Digest, Sha256};

use super::types::OrganizationNodeInfo;

/// recovery 时间桶：10 min（p2p/constants.ts:110）。
pub const RECOVERY_TIME_BUCKET_MS: i64 = 10 * 60 * 1000;

/// 恢复视图条目（`getRecoveryView`，service.ts:158-197）：
/// 当前用户为成员的每个组织一条。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoveryViewItem {
    /// 组织 id。
    pub org_id: String,
    /// 恢复盐（64 hex）。
    pub recovery_secret: String,
    /// 成员 nodeInfo（仅含 addresses 非空的成员）。
    pub member_node_infos: Vec<OrganizationNodeInfo>,
}

impl RecoveryViewItem {
    /// 该组织当前的有效 token 集合（当前桶 + 上一桶，org-recovery.ts:36-40）。
    pub fn active_tokens(&self, now_ms: i64) -> [String; 2] {
        active_recovery_tokens(&self.org_id, &self.recovery_secret, now_ms)
    }
}

/// 时间桶：`floor(nowMs / 600000)`（JS `Math.floor` 口径，负数同样向下取整）。
pub fn recovery_time_bucket(now_ms: i64) -> i64 {
    now_ms.div_euclid(RECOVERY_TIME_BUCKET_MS)
}

/// 单个桶的恢复 token：`sha256hex(orgId:recoverySecret:timeBucket)`。
pub fn recovery_token(org_id: &str, recovery_secret: &str, time_bucket: i64) -> String {
    let input = format!("{org_id}:{recovery_secret}:{time_bucket}");
    hex::encode(Sha256::digest(input.as_bytes()))
}

/// 有效 token 集合：`[当前桶, 上一桶]`（消除桶边界漏配）。
pub fn active_recovery_tokens(org_id: &str, recovery_secret: &str, now_ms: i64) -> [String; 2] {
    let bucket = recovery_time_bucket(now_ms);
    [
        recovery_token(org_id, recovery_secret, bucket),
        recovery_token(org_id, recovery_secret, bucket - 1),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_bucket_floor() {
        assert_eq!(recovery_time_bucket(0), 0);
        assert_eq!(recovery_time_bucket(599_999), 0);
        assert_eq!(recovery_time_bucket(600_000), 1);
        assert_eq!(recovery_time_bucket(600_001), 1);
        assert_eq!(recovery_time_bucket(1_720_000_000_000), 2_866_666);
        // 负数也向下取整（JS Math.floor 口径）
        assert_eq!(recovery_time_bucket(-1), -1);
        assert_eq!(recovery_time_bucket(-600_000), -1);
        assert_eq!(recovery_time_bucket(-600_001), -2);
    }

    #[test]
    fn token_is_sha256_of_colon_joined() {
        // 与算法定义自洽：token = sha256hex("orgId:secret:bucket")
        let token = recovery_token("org_0123456789abcdef", &"ab".repeat(32), 2_866_666);
        let expect = hex::encode(Sha256::digest(
            format!("org_0123456789abcdef:{}:2866666", "ab".repeat(32)).as_bytes(),
        ));
        assert_eq!(token, expect);
        assert_eq!(token.len(), 64);
        // 桶不同 → token 不同
        assert_ne!(token, recovery_token("org_0123456789abcdef", &"ab".repeat(32), 2_866_667));
        // orgId/secret 不同 → token 不同
        assert_ne!(token, recovery_token("org_ffffffffffffffff", &"ab".repeat(32), 2_866_666));
    }

    #[test]
    fn active_tokens_cover_current_and_previous_bucket() {
        let org = "org_0123456789abcdef";
        let secret = "cd".repeat(32);
        // 桶边界前 1ms
        let now = 600_000 * 100 - 1;
        let [current, previous] = active_recovery_tokens(org, &secret, now);
        assert_eq!(current, recovery_token(org, &secret, 99));
        assert_eq!(previous, recovery_token(org, &secret, 98));
        assert_ne!(current, previous);
        // 跨入下一桶后集合滑动
        let [next_current, _] = active_recovery_tokens(org, &secret, now + 1);
        assert_eq!(next_current, recovery_token(org, &secret, 100));
    }
}

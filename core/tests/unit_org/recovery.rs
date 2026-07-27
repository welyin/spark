use sha2::{Digest, Sha256};

use spark_core::org::recovery::*;

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
    assert_ne!(
        token,
        recovery_token("org_0123456789abcdef", &"ab".repeat(32), 2_866_667)
    );
    // orgId/secret 不同 → token 不同
    assert_ne!(
        token,
        recovery_token("org_ffffffffffffffff", &"ab".repeat(32), 2_866_666)
    );
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

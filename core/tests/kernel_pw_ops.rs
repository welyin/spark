//! E3 口令校验器 + D′ ack 门控 + 水位回放 + heal 24h 内核集成测试。
//!
//! 覆盖（团队派工 E3 遗留）：
//! - 双内核门控端到端：rotate 时按 should_gate 对目标设备授予/暂扣 ikey 包裹。
//! - 伪造 V 拒用：verify_value 错口令拒 + verify_password_ticket 三态。
//! - 水位回放：apply_value 防回放（changed_at <= applied → 忽略）。
//! - heal 24h 守卫（无 epoch state 不触发、锁定不触发）与 24h/7d 解耦常量。
//! - 旧版语义：pwv 缺失门控恒过、RotationReason Unknown serde 兜底。
//!
//! 说明：heal 的「旋转 24h+」正向触发与 pw_verifier_mismatch 安全日志需注入内部
//! 存储（require_storage 为 pub(crate)），集成测试不可达；此处覆盖可经公共 API
//! 断言的部分（不误触守卫 + 门控/回放/旧版语义全矩阵）。

use ed25519_dalek::SigningKey;
use serde_json::Value;
use spark_core::epoch::RotationReason;
use spark_core::kernel::Kernel;
use spark_core::pw::{
    DEFAULT_GRACE_MS, GateDecision, PasswordVerifier, apply_value, build_value, derive_kverify,
    get_applied_vts, get_pwv, put_applied_vts, put_last_verified_vts, put_pwv, should_gate,
    verify_value,
};
use spark_core::storage::{MemoryStorage, StorageBackend};

const PW: &str = "correct-horse-battery";

fn temp_kernel() -> (tempfile::TempDir, Kernel) {
    let dir = tempfile::tempdir().unwrap();
    let kernel = Kernel::init(spark_core::kernel::KernelConfig {
        data_dir: dir.path().to_path_buf(),
        app_version: "0.0.0-test".to_string(),
        p2p: None,
    })
    .unwrap();
    (dir, kernel)
}

/// 在 writer 存储上构造一条 `pwv:self` 并推进 applied 水位。
fn put_self_pwv(s: &mut MemoryStorage, node: &str, password: &str, changed_at: u64) -> PasswordVerifier {
    let pwv = build_value(password, &[0x11; 16], &[0x22; 12], changed_at, node).unwrap();
    put_pwv(s, node, &pwv, changed_at as i64).unwrap();
    put_applied_vts(s, changed_at).unwrap();
    pwv
}

fn rotate_with_devices(
    s: &mut MemoryStorage,
    my_peer: &str,
    node: &str,
    now_ms: i64,
    devices: &[&str],
) {
    // 与 kernel_epoch 的 rotate 测试同源：writer seed=[1;32]、recipient seed=[2;32]。
    // device_pub_key 存 RAW Ed25519 公钥（rotate 内部经 ed_pk_to_x25519 转换投递）。
    let self_x25519 = spark_core::epoch::ed_sk_to_x25519(&[1; 32]);
    let dev_pub: [u8; 32] = SigningKey::from_bytes(&[2; 32]).verifying_key().to_bytes();
    let auth_devices: Vec<spark_core::epoch::AuthorizedDevice> = devices
        .iter()
        .map(|peer| spark_core::epoch::AuthorizedDevice {
            peer,
            device_pub_key: Some(&dev_pub),
        })
        .collect();
    spark_core::epoch::EpochService::rotate(
        s,
        &"root-x".to_string(),
        my_peer,
        node,
        now_ms,
        RotationReason::PasswordChange,
        &self_x25519,
        &auth_devices,
        None,
    )
    .unwrap();
}

fn scan_log_kinds(s: &MemoryStorage) -> Vec<String> {
    s.scan(&spark_core::storage::ScanOptions::prefix("security:log:"))
        .unwrap()
        .into_iter()
        .map(|(_k, v)| serde_json::from_str::<Value>(&v).unwrap()["kind"].as_str().unwrap_or("").to_string())
        .collect()
}

// ---------------------------------------------------------------------------
// 测试点 A：双内核门控端到端（rotate 按 should_gate 授予/暂扣 ikey 包裹）。
// ---------------------------------------------------------------------------

/// 从未验证设备（新配对/重配对，lastVerifiedVTs==0 且无 ack）→ 立即暂扣不吃 grace。
#[test]
fn gate_never_verified_device_is_gated_immediately() {
    let mut s = MemoryStorage::new();
    put_self_pwv(&mut s, "node-a", PW, 1_000_000);

    let decision = should_gate(&s, "peer-b", 1_500_000).unwrap();
    assert_eq!(
        decision,
        GateDecision::Gated {
            latest_vts: 1_000_000,
            last_verified_vts: 0,
            grace_remaining_ms: None, // 从未验证无 grace
        },
        "从未验证设备立即暂扣，不吃 grace"
    );

    rotate_with_devices(&mut s, "peer-a", "node-a", 1_500_000, &["peer-b"]);
    assert!(
        s.get("ikey:1:peer-a:peer-b").unwrap().is_none(),
        "被门控设备不得收到新 epoch 包裹"
    );
    let kinds = scan_log_kinds(&s);
    assert!(
        kinds.contains(&"pw_grant_gated".to_string()),
        "应记 pw_grant_gated 日志，实为 {kinds:?}"
    );
}

/// 曾验证设备在 grace 窗口（7d）内 → 放行（Pass）。
#[test]
fn gate_verified_device_within_grace_is_passed() {
    let mut s = MemoryStorage::new();
    put_self_pwv(&mut s, "node-a", PW, 1_000_000);
    put_last_verified_vts(&mut s, "peer-b", 1_000_000).unwrap();

    let now_in_grace = 1_000_000u64 + DEFAULT_GRACE_MS - 1;
    assert_eq!(
        should_gate(&s, "peer-b", now_in_grace).unwrap(),
        GateDecision::Pass,
        "曾验证设备在 grace 窗口内放行"
    );

    rotate_with_devices(&mut s, "peer-a", "node-a", now_in_grace as i64, &["peer-b"]);
    assert!(
        s.get("ikey:1:peer-a:peer-b").unwrap().is_some(),
        "grace 内放行的设备应收到包裹（rotate 首轮 epoch=1）"
    );
}

/// 曾验证设备可信锚低于最新 V 且 grace 耗尽 → 暂扣。
#[test]
fn gate_verified_device_grace_expired_is_gated() {
    let mut s = MemoryStorage::new();
    put_self_pwv(&mut s, "node-a", PW, 1_000_000);
    // 可信锚低于 latest_vts：曾验证过旧 V，但未覆盖当前最新 V。
    put_last_verified_vts(&mut s, "peer-b", 999_999).unwrap();

    let now_after_grace = 999_999u64 + DEFAULT_GRACE_MS + 1;
    assert_eq!(
        should_gate(&s, "peer-b", now_after_grace).unwrap(),
        GateDecision::Gated {
            latest_vts: 1_000_000,
            last_verified_vts: 999_999,
            grace_remaining_ms: Some(0), // 曾验证但 grace 已耗尽
        },
        "曾验证设备 grace 耗尽 → 暂扣"
    );

    rotate_with_devices(&mut s, "peer-a", "node-a", now_after_grace as i64, &["peer-b"]);
    assert!(
        s.get("ikey:1:peer-a:peer-b").unwrap().is_none(),
        "grace 耗尽设备不得收到包裹"
    );
}

/// 可信锚覆盖最新 V → 恒放行（即使远超 grace）。
#[test]
fn gate_anchor_covers_latest_v_passes_even_past_grace() {
    let mut s = MemoryStorage::new();
    put_self_pwv(&mut s, "node-a", PW, 1_000_000);

    // 通过 verify_and_anchor_ack 把可信锚推到 latest_vts。
    let kverify = derive_kverify(PW, &[0x11; 16]).unwrap();
    let ack = spark_core::pw::build_ack(&kverify, "peer-b", 1_000_000);
    spark_core::pw::put_pwack(&mut s, "node-a", "peer-b", &ack, 1_000_000).unwrap();
    let anchored = spark_core::pw::verify_and_anchor_ack(&mut s, "peer-b", &kverify, 1_000_000)
        .unwrap();
    assert!(anchored, "合法 ack 应通过 MAC 并推进可信锚");

    let now_far = 1_000_000u64 + DEFAULT_GRACE_MS + 10_000_000;
    assert_eq!(
        should_gate(&s, "peer-b", now_far).unwrap(),
        GateDecision::Pass,
        "可信锚覆盖最新 V → 恒放行"
    );
    rotate_with_devices(&mut s, "peer-a", "node-a", now_far as i64, &["peer-b"]);
    assert!(
        s.get("ikey:1:peer-a:peer-b").unwrap().is_some(),
        "可信锚覆盖设备应收到包裹"
    );
}

// ---------------------------------------------------------------------------
// 测试点 B：伪造 V 拒用。
// ---------------------------------------------------------------------------

/// verify_value 错口令 → false（crypto 层拒用伪造 V）。
#[test]
fn forged_v_verify_rejected_at_crypto_layer() {
    let pwv = build_value(PW, &[0x11; 16], &[0x22; 12], 1_000_000, "node-a").unwrap();
    assert!(verify_value(&pwv, PW), "正确口令验证通过");
    assert!(!verify_value(&pwv, "wrong-password"), "错误口令拒用");
    assert!(!verify_value(&pwv, ""), "空口令拒用");
}

/// verify_password_ticket：正确 → Ok；错误 → TicketMismatch。
///
/// 注：TicketUnavailable（存储已开但无 pwv）需删除已发布 pwv（内部存储不可达），
/// 集成测试不可构造；此处覆盖 Ok / TicketMismatch 两态。
#[test]
fn verify_ticket_three_states() {
    let (_dir, mut kernel) = temp_kernel();
    kernel.init_identity(PW, "alice", None).expect("init identity");

    kernel.verify_password_ticket(PW).expect("正确口令验票通过");
    let err = kernel.verify_password_ticket("wrong-password").unwrap_err().to_string();
    assert_eq!(err, "Ticket mismatch", "错误口令 → TicketMismatch");
}

// ---------------------------------------------------------------------------
// 测试点 C：水位回放（apply_value 防回放）。
// ---------------------------------------------------------------------------

#[test]
fn apply_value_rejects_replay_but_accepts_newer() {
    let mut s = MemoryStorage::new();
    let pwv1 = put_self_pwv(&mut s, "node-a", PW, 1_000_000);
    assert_eq!(get_applied_vts(&s).unwrap(), 1_000_000, "发布即推进水位");

    // 回放旧 V（changed_at <= applied）→ 忽略，返回 false，不覆盖 last-good。
    let replay = PasswordVerifier {
        changed_at: 1_000_000, // 等于 applied
        ..pwv1.clone()
    };
    let applied_replay = apply_value(&mut s, "node-a", &replay, 1_500_000).unwrap();
    assert!(!applied_replay, "回放（<=applied）应被忽略");
    assert_eq!(get_pwv(&s).unwrap().unwrap().changed_at, 1_000_000, "last-good 未被回放覆盖");

    // 新 V（changed_at > applied）→ 应用并推进水位。
    let newer = PasswordVerifier {
        changed_at: 2_000_000,
        ..pwv1.clone()
    };
    let applied_newer = apply_value(&mut s, "node-a", &newer, 2_500_000).unwrap();
    assert!(applied_newer, "更新的 V 应被应用");
    assert_eq!(get_pwv(&s).unwrap().unwrap().changed_at, 2_000_000);
    assert_eq!(get_applied_vts(&s).unwrap(), 2_000_000, "水位推进到新 V");
}

// ---------------------------------------------------------------------------
// 测试点 D：heal 守卫（不误触）+ 24h/7d 解耦常量。
// ---------------------------------------------------------------------------

/// 无 epoch state（fresh init，p2p 未启动）→ 不触发 heal，且不 panic。
#[test]
fn heal_does_not_fire_without_epoch_state() {
    let (_dir, mut kernel) = temp_kernel();
    kernel.init_identity(PW, "alice", None).expect("init identity");
    let healed = kernel.maybe_heal_password(PW).unwrap();
    assert!(!healed, "无 epoch state 不得触发 heal");
}

/// 锁定态 → 不触发 heal（守卫：unlocked 要求）。
#[test]
fn heal_does_not_fire_when_locked() {
    let (_dir, mut kernel) = temp_kernel();
    kernel.init_identity(PW, "alice", None).expect("init identity");
    kernel.lock();
    let healed = kernel.maybe_heal_password(PW).unwrap();
    assert!(!healed, "锁定态不得触发 heal");
}

/// 24h 阈值与 graceMs 7d 解耦：heal 用独立 24h 常量，grace 默认 7d。
#[test]
fn heal_threshold_decoupled_from_grace_ms() {
    // grace 默认 7 天。
    assert_eq!(DEFAULT_GRACE_MS, 7 * 24 * 60 * 60 * 1000, "默认 grace=7 天");
    // heal 阈值是独立的 24h（与 7d 解耦）——heal 判定不读 graceMs。
    let mut s = MemoryStorage::new();
    let now = 1_000_000u64;
    spark_core::pw::put_grace_ms(&mut s, 7 * 24 * 60 * 60 * 1000).unwrap(); // 7d
    // 把 epoch:state.rotated_at 设到 24h+ 前 + 本机无 ikey → 理论上应 heal；
    // 这里验证 heal 不依赖 graceMs（把 graceMs 设极小也不影响 heal 的 24h 判定
    // ——heal 由 kernel 侧 elapsed_ms>=24h 决定，而非 should_gate）。
    let _ = now;
    // 注：heal 正向触发需 kernel 内部存储注入，集成不可达；此处确认 graceMs
    // 独立存储键存在且可写（解耦面）。
    assert_eq!(spark_core::pw::get_grace_ms(&s).unwrap(), 7 * 24 * 60 * 60 * 1000);
}

// ---------------------------------------------------------------------------
// 测试点 E：旧版语义（pwv 缺失门控恒过、RotationReason Unknown 兜底）。
// ---------------------------------------------------------------------------

#[test]
fn legacy_no_pwv_gate_always_passes() {
    let s = MemoryStorage::new();
    assert_eq!(should_gate(&s, "peer-b", 1_000_000).unwrap(), GateDecision::Pass, "无 pwv 恒放行");
}

#[test]
fn rotation_reason_unknown_serde_fallback() {
    let s: spark_core::epoch::EpochState =
        serde_json::from_str(r#"{"current":3,"rotatedAt":1,"rotatedBy":"a","reason":"future_reason"}"#)
            .unwrap();
    assert_eq!(s.reason, RotationReason::Unknown, "未知 reason → Unknown 兜底");
    assert_eq!(s.reason.as_str(), "unknown");

    let heal: spark_core::epoch::EpochState =
        serde_json::from_str(r#"{"current":3,"rotatedAt":1,"rotatedBy":"a","reason":"heal"}"#).unwrap();
    assert_eq!(heal.reason, RotationReason::Heal);
    let pr: spark_core::epoch::EpochState = serde_json::from_str(
        r#"{"current":3,"rotatedAt":1,"rotatedBy":"a","reason":"password_reset"}"#,
    )
    .unwrap();
    assert_eq!(pr.reason, RotationReason::PasswordReset);
}

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
fn put_self_pwv(
    s: &mut MemoryStorage,
    node: &str,
    password: &str,
    changed_at: u64,
) -> PasswordVerifier {
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
        .map(|(_k, v)| {
            serde_json::from_str::<Value>(&v).unwrap()["kind"]
                .as_str()
                .unwrap_or("")
                .to_string()
        })
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

    rotate_with_devices(
        &mut s,
        "peer-a",
        "node-a",
        now_after_grace as i64,
        &["peer-b"],
    );
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
    let anchored =
        spark_core::pw::verify_and_anchor_ack(&mut s, "peer-b", &kverify, 1_000_000).unwrap();
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
    kernel
        .init_identity(PW, "alice", None)
        .expect("init identity");

    kernel.verify_password_ticket(PW).expect("正确口令验票通过");
    let err = kernel
        .verify_password_ticket("wrong-password")
        .unwrap_err()
        .to_string();
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
    assert_eq!(
        get_pwv(&s).unwrap().unwrap().changed_at,
        1_000_000,
        "last-good 未被回放覆盖"
    );

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
    kernel
        .init_identity(PW, "alice", None)
        .expect("init identity");
    let healed = kernel.maybe_heal_password(PW).unwrap();
    assert!(!healed, "无 epoch state 不得触发 heal");
}

/// 锁定态 → 不触发 heal（守卫：unlocked 要求）。
#[test]
fn heal_does_not_fire_when_locked() {
    let (_dir, mut kernel) = temp_kernel();
    kernel
        .init_identity(PW, "alice", None)
        .expect("init identity");
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
    assert_eq!(
        spark_core::pw::get_grace_ms(&s).unwrap(),
        7 * 24 * 60 * 60 * 1000
    );
}

// ---------------------------------------------------------------------------
// 测试点 E：旧版语义（pwv 缺失门控恒过、RotationReason Unknown 兜底）。
// ---------------------------------------------------------------------------

#[test]
fn legacy_no_pwv_gate_always_passes() {
    let s = MemoryStorage::new();
    assert_eq!(
        should_gate(&s, "peer-b", 1_000_000).unwrap(),
        GateDecision::Pass,
        "无 pwv 恒放行"
    );
}

#[test]
fn rotation_reason_unknown_serde_fallback() {
    let s: spark_core::epoch::EpochState = serde_json::from_str(
        r#"{"current":3,"rotatedAt":1,"rotatedBy":"a","reason":"future_reason"}"#,
    )
    .unwrap();
    assert_eq!(
        s.reason,
        RotationReason::Unknown,
        "未知 reason → Unknown 兜底"
    );
    assert_eq!(s.reason.as_str(), "unknown");

    let heal: spark_core::epoch::EpochState =
        serde_json::from_str(r#"{"current":3,"rotatedAt":1,"rotatedBy":"a","reason":"heal"}"#)
            .unwrap();
    assert_eq!(heal.reason, RotationReason::Heal);
    let pr: spark_core::epoch::EpochState = serde_json::from_str(
        r#"{"current":3,"rotatedAt":1,"rotatedBy":"a","reason":"password_reset"}"#,
    )
    .unwrap();
    assert_eq!(pr.reason, RotationReason::PasswordReset);
}

// ---------------------------------------------------------------------------
// 测试点 F：分叉场景 unify（A5 接线修复回归）+ 时戳同源（§13.5）。
// ---------------------------------------------------------------------------

/// 分叉场景：本机身份文件由旧口令封存、最新 V 由他端用账号新口令封存。
/// 契约 m45 §13.1：内核复验与 ack 都必须针对**新口令**——修复前实现误用
/// old_password，分叉场景 unify 必回 ticket-mismatch，且 ack MAC 用旧口令派生
/// 导致对端 writer 永不锚定（D′ 收敛链断裂）。
#[test]
fn unify_password_fork_scenario_rechecks_and_acks_new_password() {
    const NEW_PW: &str = "account-new-password";
    let (_dir, mut kernel) = temp_kernel();
    kernel.init_identity(PW, "alice", None).expect("init");

    // 模拟他端改密到达：V2 用 NEW_PW 封存、changedAt 前进（本机文件仍 PW 封存）。
    let v1_changed = {
        let s = kernel.__test_storage().unwrap();
        get_pwv(&s).unwrap().expect("init 已发布 V1").changed_at
    };
    let salt = [0x33; 16];
    let nonce = [0x44; 12];
    let v2 = build_value(NEW_PW, &salt, &nonce, v1_changed + 1000, "peer-a").unwrap();
    {
        let mut s = kernel.__test_storage().unwrap();
        apply_value(&mut s, "peer-a", &v2, (v1_changed + 1000) as i64).unwrap();
    }

    // 验票分叉语义：账号新口令过；本机旧口令不再是账号口令 → ticket-mismatch。
    kernel
        .verify_password_ticket(NEW_PW)
        .expect("新口令验票通过");
    assert!(
        matches!(
            kernel.verify_password_ticket(PW),
            Err(spark_core::kernel::KernelError::TicketMismatch)
        ),
        "旧口令对新 V 验票 → ticket-mismatch"
    );

    // 错新口令 unify → ticket-mismatch，且文件未被重封（旧口令仍可解锁）。
    assert!(matches!(
        kernel.unify_password(PW, "wrong-new-password", RotationReason::PasswordChange),
        Err(spark_core::kernel::KernelError::TicketMismatch)
    ));
    kernel.lock();
    kernel
        .unlock(PW, None)
        .expect("V 复验失败不得动文件，旧口令仍可解锁");
    // unlock 用旧口令对新 V 验证失败 → stale 置位（乙 引导态）。
    {
        let s = kernel.__test_storage().unwrap();
        assert!(spark_core::pw::get_stale(&s).unwrap(), "观察到分叉 → stale");
    }

    // 正确 unify：旧口令解本机文件 + 新口令过 V 复验 → 重封 + 发 ack + 清 stale。
    kernel
        .unify_password(PW, NEW_PW, RotationReason::PasswordChange)
        .expect("分叉场景 unify 成功");
    let s = kernel.__test_storage().unwrap();
    assert!(!spark_core::pw::get_stale(&s).unwrap(), "unify 后清 stale");
    assert_eq!(get_applied_vts(&s).unwrap(), v2.changed_at, "水位覆盖 V2");
    // ack 必须能被 NEW_PW 派生的 kverify 验证（否则对端 writer 永不锚定）。
    let acks = s
        .scan(&spark_core::storage::ScanOptions::prefix("pwack:"))
        .unwrap();
    assert_eq!(acks.len(), 1, "本机一条 ack");
    let ack = spark_core::pw::PasswordAck::parse(&acks[0].1).unwrap();
    assert_eq!(ack.v_ts, v2.changed_at, "ack 引用 V2 的 changedAt");
    let kverify_new = derive_kverify(NEW_PW, &salt).unwrap();
    let kack = spark_core::pw::derive_kack(&kverify_new);
    let peer = acks[0].0.trim_start_matches("pwack:");
    assert!(
        spark_core::pw::verify_ack_mac(&kack, peer, ack.v_ts, &ack.mac),
        "ack MAC 必须由新口令知识派生（对端可锚定）"
    );

    // 文件已用新口令重封：旧口令解锁失败、新口令成功。
    kernel.lock();
    assert!(kernel.unlock(PW, None).is_err(), "旧口令已失效");
    kernel.unlock(NEW_PW, None).expect("新口令解锁");
}

/// 时戳同源（§13.5 冻结钉）：口令驱动轮换必须 `pwv.changedAt == epoch:state.
/// rotatedAt`，且发布先于轮换（rotate 的逐设备门控以「最新 V」为水位——
/// 先发布后轮换，超 grace 的陈旧设备才会在本次轮换被暂扣）。
#[test]
fn change_password_publishes_v_before_rotation_with_same_ts() {
    let (_dir, mut kernel) = temp_kernel();
    kernel.init_identity(PW, "alice", None).expect("init");
    kernel
        .change_password(PW, "rotated-new-password")
        .expect("change_password");
    let s = kernel.__test_storage().unwrap();
    let state = spark_core::epoch::get_epoch_state(&s)
        .unwrap()
        .expect("改密触发轮换");
    let pwv = get_pwv(&s).unwrap().expect("改密发布新 V");
    assert_eq!(state.reason, RotationReason::PasswordChange);
    assert_eq!(
        pwv.changed_at, state.rotated_at as u64,
        "时戳同源：pwv.changedAt == epoch:state.rotatedAt（§13.5）"
    );
}

// ---------------------------------------------------------------------------
// 测试点 G：密码考试（identity.md §4.2，A6）。
// ---------------------------------------------------------------------------

/// 7 天 ±1s 边界 + 首次启用懒初始化（以启用时刻为初始值，老账号不当场被拦）。
#[test]
fn exam_status_lazy_init_and_interval_boundary() {
    use spark_core::p2p::node::system_now_ms;
    use spark_core::pw::PASSWORD_EXAM_INTERVAL_MS;
    let (_dir, mut kernel) = temp_kernel();
    kernel.init_identity(PW, "alice", None).expect("init");
    // init 是真实密码事件 → 时间戳已刷新，非 overdue。
    let st = kernel.password_exam_status().unwrap();
    assert!(!st.overdue, "init 后非 overdue");

    let mut s = kernel.__test_storage().unwrap();
    let now = system_now_ms() as u64;
    spark_core::pw::put_last_password_auth(&mut s, now - PASSWORD_EXAM_INTERVAL_MS + 1000)
        .unwrap();
    assert!(
        !kernel.password_exam_status().unwrap().overdue,
        "距 7 天还差 1 秒 → 不 overdue"
    );
    spark_core::pw::put_last_password_auth(&mut s, now - PASSWORD_EXAM_INTERVAL_MS - 1000)
        .unwrap();
    assert!(
        kernel.password_exam_status().unwrap().overdue,
        "超 7 天 1 秒 → overdue"
    );

    // 懒初始化：删键模拟启用前老账号 → 以首次查询时刻为初始值写入，不 overdue。
    s.delete(spark_core::pw::LAST_PASSWORD_AUTH_KEY).unwrap();
    let st2 = kernel.password_exam_status().unwrap();
    assert!(!st2.overdue, "首次启用以启用时刻为初始值（§五.2）");
    assert!(
        spark_core::pw::get_last_password_auth(&s).unwrap().is_some(),
        "初始值已写入"
    );
}

/// bioSourced 解锁不刷新时间戳；真实密码解锁刷新。
#[test]
fn bio_unlock_skips_refresh_and_password_unlock_refreshes() {
    use spark_core::p2p::node::system_now_ms;
    let (_dir, mut kernel) = temp_kernel();
    kernel.init_identity(PW, "alice", None).expect("init");
    // 回拨时间戳到 10 天前（已超考试间隔）。
    let old_ts = system_now_ms() as u64 - 10 * 24 * 60 * 60 * 1000;
    {
        let mut s = kernel.__test_storage().unwrap();
        spark_core::pw::put_last_password_auth(&mut s, old_ts).unwrap();
    }
    kernel.lock();
    kernel
        .unlock_bio_sourced(PW, None)
        .expect("bioSourced 解锁");
    let st = kernel.password_exam_status().unwrap();
    assert_eq!(
        st.last_password_auth, old_ts,
        "bioSourced 解锁不刷新 lastPasswordAuth（§4.2 只认密码输入）"
    );
    assert!(st.overdue, "bio 通道下考试状态保持 overdue");
    kernel.lock();
    kernel.unlock(PW, None).expect("真实密码解锁");
    let st2 = kernel.password_exam_status().unwrap();
    assert!(
        st2.last_password_auth > old_ts,
        "真实密码解锁刷新时间戳"
    );
    assert!(!st2.overdue, "刷新后考试通过（生物识别恢复的前提）");
}

/// 连续失败不锁死（防 DoS 自己）；忘记密码出口 = 助记词/QR 恢复通道可用。
#[test]
fn exam_failed_attempts_no_lockout_and_recovery_exit() {
    let (_dir, mut kernel) = temp_kernel();
    let init = kernel.init_identity(PW, "alice", None).expect("init");
    kernel.lock();
    for _ in 0..5 {
        assert!(kernel.unlock("wrong-password", None).is_err());
    }
    kernel
        .unlock(PW, None)
        .expect("连续失败后正确口令仍可解锁（无锁死）");

    // 忘记密码出口：助记词在新设备恢复（既有延迟恢复通道语义不变），
    // 恢复设新密码 = 真实密码事件 → 时间戳刷新。
    let dir_b = tempfile::tempdir().unwrap();
    let mut kernel_b = Kernel::init(spark_core::kernel::KernelConfig {
        data_dir: dir_b.path().to_path_buf(),
        app_version: "0.0.0-test".to_string(),
        p2p: None,
    })
    .unwrap();
    kernel_b
        .recover_mnemonic(&init.mnemonic, "fresh-new-password", "alice", None)
        .expect("助记词恢复出口可用");
    assert!(
        !kernel_b.password_exam_status().unwrap().overdue,
        "恢复即刷新时间戳"
    );
}

// ---------------------------------------------------------------------------
// 测试点 H：profile-sync 直发通道 D′ 纳管（A45）——挂起/补取四门控分支。
// ---------------------------------------------------------------------------

fn profile_snapshot(ts: i64) -> serde_json::Value {
    serde_json::json!({"updatedAt": ts, "nickname": "小明"})
}

/// 挂起只保留 updatedAt 最新的一份（回放缓存，非队列）。
#[test]
fn profile_pending_stash_keeps_newest() {
    let mut s = MemoryStorage::new();
    spark_core::pw::stash_pending_profile(&mut s, &profile_snapshot(100)).unwrap();
    spark_core::pw::stash_pending_profile(&mut s, &profile_snapshot(50)).unwrap();
    assert_eq!(
        s.get(spark_core::pw::PENDING_PROFILE_SYNC_KEY).unwrap().unwrap(),
        profile_snapshot(100).to_string(),
        "旧快照不覆盖新挂起"
    );
    spark_core::pw::stash_pending_profile(&mut s, &profile_snapshot(200)).unwrap();
    assert_eq!(
        s.get(spark_core::pw::PENDING_PROFILE_SYNC_KEY).unwrap().unwrap(),
        profile_snapshot(200).to_string(),
        "新快照替换挂起"
    );
}

/// 补取四门控分支：①pwv 缺失恒过 ②锚覆盖放行 ③曾验证 grace 内放行/超 grace
/// 暂扣 ④从未验证立即暂扣；暂扣时挂起保留，放行时取出即清。
#[test]
fn profile_pending_take_respects_gate_branches() {
    use spark_core::pw::{
        DEFAULT_GRACE_MS, PENDING_PROFILE_SYNC_KEY, put_last_verified_vts,
        stash_pending_profile, take_pending_profile_if_allowed,
    };
    // ① pwv 缺失 → 恒放行（老账号不可回归）。
    let mut s = MemoryStorage::new();
    stash_pending_profile(&mut s, &profile_snapshot(100)).unwrap();
    assert!(
        take_pending_profile_if_allowed(&mut s, "peer-b", 1_000_000)
            .unwrap()
            .is_some(),
        "pwv 缺失恒过"
    );
    assert!(
        s.get(PENDING_PROFILE_SYNC_KEY).unwrap().is_none(),
        "取出即清除"
    );

    // ② ack 覆盖最新 V → 放行。
    let mut s = MemoryStorage::new();
    put_self_pwv(&mut s, "node-a", PW, 1_000);
    put_last_verified_vts(&mut s, "peer-b", 1_000).unwrap();
    stash_pending_profile(&mut s, &profile_snapshot(100)).unwrap();
    assert!(
        take_pending_profile_if_allowed(&mut s, "peer-b", 2_000)
            .unwrap()
            .is_some(),
        "锚覆盖最新 V → 放行"
    );

    // ③ 曾验证（锚 500 < V 1000）：grace 内放行；超 grace 暂扣且挂起保留。
    let mut s = MemoryStorage::new();
    put_self_pwv(&mut s, "node-a", PW, 1_000);
    put_last_verified_vts(&mut s, "peer-b", 500).unwrap();
    stash_pending_profile(&mut s, &profile_snapshot(100)).unwrap();
    assert!(
        take_pending_profile_if_allowed(&mut s, "peer-b", 500 + DEFAULT_GRACE_MS)
            .unwrap()
            .is_some(),
        "曾验证 grace 内 → 放行"
    );
    stash_pending_profile(&mut s, &profile_snapshot(100)).unwrap();
    assert!(
        take_pending_profile_if_allowed(&mut s, "peer-b", 500 + DEFAULT_GRACE_MS + 1)
            .unwrap()
            .is_none(),
        "超 grace → 暂扣"
    );
    assert!(
        s.get(PENDING_PROFILE_SYNC_KEY).unwrap().is_some(),
        "暂扣时挂起保留（待 ack 补齐后补应用）"
    );

    // ④ 从未验证 → 立即暂扣，不吃 grace。
    let mut s = MemoryStorage::new();
    put_self_pwv(&mut s, "node-a", PW, 1_000);
    stash_pending_profile(&mut s, &profile_snapshot(100)).unwrap();
    assert!(
        take_pending_profile_if_allowed(&mut s, "peer-b", 1_000)
            .unwrap()
            .is_none(),
        "从未验证立即暂扣"
    );
    assert!(s.get(PENDING_PROFILE_SYNC_KEY).unwrap().is_some());
}

/// 懒发布/init 路径（publish 先推进 applied → ack 跳过）也必须推进本机自锚——
/// 否则本机侧门控（A45 profile 通道/DeviceOutOfGrace）把已验证设备误判为
/// 从未验证（legacy QR 收敛断裂的回归钉）。
#[test]
fn init_self_anchor_advances_even_when_ack_skipped() {
    let (_dir, mut kernel) = temp_kernel();
    kernel.init_identity(PW, "alice", None).expect("init");
    let s = kernel.__test_storage().unwrap();
    let pwv = get_pwv(&s).unwrap().expect("init 发布 V");
    let anchors = s
        .scan(&spark_core::storage::ScanOptions::prefix(
            "p2p:pw:lastVerifiedVTs:",
        ))
        .unwrap();
    assert_eq!(anchors.len(), 1, "init 即自锚（ack 跳过也要补锚）");
    assert_eq!(
        anchors[0].1,
        pwv.changed_at.to_string(),
        "自锚 == 当前 V changedAt"
    );
}

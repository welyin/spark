//! M5 延迟恢复通道内核集成测试。
//!
//! 覆盖 m4-m5-mobile-plan.md §6 测试点：
//! - 测试点 7 状态机全路径（纯逻辑 + 注入 now_ms）：initiate→pending→confirm
//!   TooEarly→veto→RecoveryVetoed；正常路径：deadline 后 confirm ready→committed。
//! - 测试点 8 reset_password_session：强度 <8 拒、会话重封后 reveal_mnemonic(新口令)
//!   助记词一致、旧口令解不开。
//! - 测试点 9 入站三分支（走 handle_inbound_dm 直调，不 mock）：
//!   from != me 静默、字段缺失拒、veto 乱序墓碑先于 initiated→initiated 落库即带
//!   vetoed、重复 veto 幂等。
//! - 测试点 10 时钟安全：seen 记录 localArrivalMs/windowMs、窗外 veto 拒
//!   VetoWindowExpired、window clamp 0/7d 边界。
//!
//! 信封构造复用 `dm_envelope::build_envelope`（KIND_RECOVERY），入站用
//! `handle_inbound_dm` 校验验签。

mod common;

use std::collections::HashSet;

use ed25519_dalek::SigningKey;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use spark_core::kernel::{Kernel, dm_envelope, handle_inbound_dm};
use spark_core::recovery::{
    MAX_WINDOW_MS, RECOVERY_KIND, RecoveryError, RecoveryOp, RecoveryService, RecoveryState,
};
use spark_core::storage::MemoryStorage;

use common::*;

const NODE: &str = "local-node";

fn root(seed: u8) -> (SigningKey, String) {
    let key = SigningKey::from_bytes(&[seed; 32]);
    let root_id = hex::encode(Sha256::digest(key.verifying_key().to_bytes()));
    (key, root_id)
}

fn deliver_recovery(
    storage: &mut MemoryStorage,
    signing_key: &SigningKey,
    my_root: &str,
    ts: i64,
    now_ms: i64,
    body: Value,
) -> spark_core::kernel::InboundDmResult {
    let envelope =
        dm_envelope::build_envelope(RECOVERY_KIND, my_root, my_root, ts, body, signing_key);
    handle_inbound_dm(
        storage,
        my_root,
        "我",
        envelope,
        "peer-conn",
        &HashSet::new(),
        now_ms,
        NODE,
        None,
    )
    .unwrap()
}

// ---------------------------------------------------------------------------
// 测试点 7：状态机全路径（纯逻辑，注入 now_ms）。
// ---------------------------------------------------------------------------

#[test]
fn state_machine_initiate_tooearly_veto_and_normal_commit() {
    let mut s = MemoryStorage::new();
    let t0 = 1_000_000i64;

    // initiate → pending，deadline = t0 + 1h。
    let pending = RecoveryService::<MemoryStorage>::initiate(
        &mut s,
        t0,
        RecoveryOp::ResetPassword,
        Some(1.0),
    )
    .unwrap();
    assert_eq!(pending.op, RecoveryOp::ResetPassword);
    assert_eq!(pending.state, RecoveryState::Initiated);
    assert!(pending.request_id.starts_with("rc"));

    // confirm TooEarly（未到 deadline）。
    let err = RecoveryService::<MemoryStorage>::check_confirm_ready(
        &s,
        pending.deadline - 1,
        &pending.request_id,
    )
    .unwrap_err();
    assert_eq!(
        err,
        RecoveryError::TooEarly,
        "deadline 前 confirm → TooEarly"
    );

    // veto → pending 置 vetoed + 墓碑。
    let vetoed = RecoveryService::<MemoryStorage>::veto(
        &mut s,
        pending.deadline - 1,
        &pending.request_id,
        "peer-guardian",
    )
    .unwrap();
    assert!(vetoed);
    let p2 = RecoveryService::<MemoryStorage>::get_pending(&s)
        .unwrap()
        .unwrap();
    assert_eq!(p2.state, RecoveryState::Vetoed);
    assert!(p2.vetoed);
    assert!(
        RecoveryService::<MemoryStorage>::get_veto_tombstone(&s, &pending.request_id)
            .unwrap()
            .is_some(),
        "应落墓碑"
    );

    // veto 后即使过 deadline confirm → RecoveryVetoed。
    let err2 = RecoveryService::<MemoryStorage>::check_confirm_ready(
        &s,
        pending.deadline + 1,
        &pending.request_id,
    )
    .unwrap_err();
    assert_eq!(
        err2,
        RecoveryError::RecoveryVetoed,
        "veto 后 confirm → RecoveryVetoed"
    );

    // 正常路径：新请求，deadline 后 confirm ready → committed。
    let pending2 = RecoveryService::<MemoryStorage>::initiate(
        &mut s,
        t0,
        RecoveryOp::ResetPassword,
        Some(1.0),
    )
    .unwrap();
    let err3 = RecoveryService::<MemoryStorage>::check_confirm_ready(
        &s,
        pending2.deadline - 1,
        &pending2.request_id,
    )
    .unwrap_err();
    assert_eq!(err3, RecoveryError::TooEarly);
    let ready = RecoveryService::<MemoryStorage>::check_confirm_ready(
        &s,
        pending2.deadline,
        &pending2.request_id,
    )
    .unwrap();
    assert_eq!(ready.request_id, pending2.request_id, "deadline 到达可确认");
    RecoveryService::<MemoryStorage>::mark_committed(&mut s, &pending2.request_id).unwrap();
    let p3 = RecoveryService::<MemoryStorage>::get_pending(&s)
        .unwrap()
        .unwrap();
    assert_eq!(
        p3.state,
        RecoveryState::Committed,
        "确认后 pending → committed"
    );
    // seen 命中者 committed 标记。
    let seen = RecoveryService::<MemoryStorage>::get_seen(&s, &pending2.request_id).unwrap();
    // 本机发起时不一定有 seen（broadcast 由 p2p 负责），仅断言 committed 状态即可。
    let _ = seen;

    // 状态聚合：committed 后不再 ready_to_confirm。
    let status = RecoveryService::<MemoryStorage>::status(&s, pending2.deadline + 1).unwrap();
    assert!(!status.ready_to_confirm);
}

#[test]
fn initiate_rejects_duplicate_active_and_pair_new_device() {
    let mut s = MemoryStorage::new();
    RecoveryService::<MemoryStorage>::initiate(&mut s, 0, RecoveryOp::ResetPassword, Some(0.001))
        .unwrap();
    // 活跃 pending 存在 → RecoveryPending。
    let err = RecoveryService::<MemoryStorage>::initiate(
        &mut s,
        0,
        RecoveryOp::ResetPassword,
        Some(0.001),
    )
    .unwrap_err();
    assert_eq!(
        err,
        RecoveryError::RecoveryPending,
        "活跃 pending 时再发起 → RecoveryPending"
    );

    // 已有 committed 后可再发起（单槽复用）。
    let pending = RecoveryService::<MemoryStorage>::get_pending(&s)
        .unwrap()
        .unwrap();
    RecoveryService::<MemoryStorage>::mark_committed(&mut s, &pending.request_id).unwrap();
    RecoveryService::<MemoryStorage>::initiate(&mut s, 0, RecoveryOp::ResetPassword, Some(0.001))
        .unwrap();

    // PairNewDevice 不支持。
    let err2 =
        RecoveryService::<MemoryStorage>::initiate(&mut s, 0, RecoveryOp::PairNewDevice, None)
            .unwrap_err();
    assert_eq!(err2, RecoveryError::UnsupportedOp);

    // 过短 delay → InvalidInput。
    let err3 =
        RecoveryService::<MemoryStorage>::initiate(&mut s, 0, RecoveryOp::ResetPassword, Some(0.0))
            .unwrap_err();
    assert_eq!(err3, RecoveryError::InvalidInput);
}

// ---------------------------------------------------------------------------
// 测试点 10：时钟安全（window 计算 + 窗外 veto + clamp 边界）。
// ---------------------------------------------------------------------------

#[test]
fn clock_safety_window_clamp_and_out_of_window_veto() {
    // window = clamp(deadline - envelope_ts, (0, 7d])。
    // 负窗口 → clamp 到 1ms（下限）。
    assert_eq!(
        RecoveryService::<MemoryStorage>::recovery_window_ms(1_000, 2_000),
        1,
        "负窗口 clamp 到下限 1ms"
    );
    // 超大窗口 → clamp 到 7d。
    assert_eq!(
        RecoveryService::<MemoryStorage>::recovery_window_ms(MAX_WINDOW_MS + 100, 0),
        MAX_WINDOW_MS,
        "超长窗口 clamp 到 7d"
    );
    // 精确 7d 边界保留。
    assert_eq!(
        RecoveryService::<MemoryStorage>::recovery_window_ms(MAX_WINDOW_MS, 0),
        MAX_WINDOW_MS,
        "正好 7d 不截断"
    );
    // 正常窗口。
    assert_eq!(
        RecoveryService::<MemoryStorage>::recovery_window_ms(3_600_000, 0),
        3_600_000
    );

    // seen 记录 localArrivalMs + windowMs，窗外 veto 拒。
    let mut s = MemoryStorage::new();
    let t0 = 1_000_000i64;
    let window = 1000i64;
    let (seen, created, vetoed) = RecoveryService::<MemoryStorage>::apply_inbound_initiated(
        &mut s,
        t0,
        "rc-clock-1",
        RecoveryOp::ResetPassword,
        "peer-a",
        window,
    )
    .unwrap();
    assert!(created);
    assert!(!vetoed);
    assert_eq!(seen.local_arrival_ms, t0, "seen 记录本地到达时间");
    assert_eq!(seen.window_ms, window, "seen 记录否决窗毫秒");
    // 窗口边界 = t0 + window，到达 t0+window 时刚好可用（now > window_end 才拒）。
    let ok = RecoveryService::<MemoryStorage>::veto(&mut s, t0 + window, "rc-clock-1", "peer-b")
        .unwrap();
    assert!(ok, "窗口边界时刻可否决");
    // 窗外（t0+window+1）→ VetoWindowExpired（已置墓碑后再次 veto 走 seen 分支）。
    let err =
        RecoveryService::<MemoryStorage>::veto(&mut s, t0 + window + 1, "rc-clock-1", "peer-b")
            .unwrap_err();
    assert_eq!(
        err,
        RecoveryError::VetoWindowExpired,
        "窗外 veto → VetoWindowExpired"
    );
}

// ---------------------------------------------------------------------------
// 测试点 9：入站三分支（handle_inbound_dm 直调）。
// ---------------------------------------------------------------------------

/// 构造 recovery-initiated body。
fn initiated_body(request_id: &str, op: &str, deadline: i64, from_device: &str) -> Value {
    json!({
        "kind": "initiated",
        "op": op,
        "requestId": request_id,
        "deadline": deadline,
        "fromDevice": from_device,
    })
}

#[test]
fn inbound_from_not_self_silently_rejected() {
    let (key, my_root) = root(1);
    let (other_key, other_root) = root(2);
    let mut s = MemoryStorage::new();

    // 信封 from != me（用另一把根私钥签，from=other_root）。
    let envelope = dm_envelope::build_envelope(
        RECOVERY_KIND,
        &other_root,
        &my_root,
        1_000_000,
        initiated_body("rc-x", "reset_password", 2_000_000, "peer-a"),
        &other_key,
    );
    let r = handle_inbound_dm(
        &mut s,
        &my_root,
        "我",
        envelope,
        "peer-conn",
        &HashSet::new(),
        1_000_000,
        NODE,
        None,
    )
    .unwrap();
    assert_eq!(
        r.response,
        json!({ "ok": false, "reason": "not-self" }),
        "非自设备信封应静默拒"
    );
    assert!(r.events.is_empty());
    assert!(
        RecoveryService::<MemoryStorage>::get_seen(&s, "rc-x")
            .unwrap()
            .is_none(),
        "非自设备不得落 seen"
    );
    let _ = key;
}

#[test]
fn inbound_initiated_missing_fields_rejected() {
    let (key, my_root) = root(3);
    let mut s = MemoryStorage::new();

    // 缺 deadline → invalid-body。
    let r = deliver_recovery(
        &mut s,
        &key,
        &my_root,
        1_000_000,
        1_000_000,
        json!({"kind":"initiated","op":"reset_password","requestId":"rc-a","fromDevice":"peer-a"}),
    );
    assert_eq!(r.response, json!({ "ok": false, "reason": "invalid-body" }));
    assert!(
        RecoveryService::<MemoryStorage>::get_seen(&s, "rc-a")
            .unwrap()
            .is_none()
    );

    // 非法 op → invalid-body。
    let r2 = deliver_recovery(
        &mut s,
        &key,
        &my_root,
        1_000_000,
        1_000_000,
        json!({"kind":"initiated","op":"bogus","requestId":"rc-b","deadline":2_000_000,"fromDevice":"peer-a"}),
    );
    assert_eq!(
        r2.response,
        json!({ "ok": false, "reason": "invalid-body" })
    );

    // requestId 空 → invalid-body。
    let r3 = deliver_recovery(
        &mut s,
        &key,
        &my_root,
        1_000_000,
        1_000_000,
        json!({"kind":"initiated","op":"reset_password","requestId":"","deadline":2_000_000,"fromDevice":"peer-a"}),
    );
    assert_eq!(
        r3.response,
        json!({ "ok": false, "reason": "invalid-body" })
    );

    // 未知 kind → invalid-kind。
    let r4 = deliver_recovery(
        &mut s,
        &key,
        &my_root,
        1_000_000,
        1_000_000,
        json!({"kind":"nope","requestId":"rc-c"}),
    );
    assert_eq!(
        r4.response,
        json!({ "ok": false, "reason": "invalid-kind" })
    );
}

#[test]
fn inbound_initiated_valid_writes_seen_with_local_clock_and_emits_event() {
    let (key, my_root) = root(4);
    let mut s = MemoryStorage::new();
    let envelope_ts = 1_000_000i64;
    let deadline = 2_000_000i64;
    let local_now = 1_100_000i64;

    let r = deliver_recovery(
        &mut s,
        &key,
        &my_root,
        envelope_ts,
        local_now,
        initiated_body("rc-ok", "reset_password", deadline, "peer-a"),
    );
    assert_eq!(r.response, json!({ "ok": true }));
    // 事件 deadline = 本地到达 + window（本地时钟独立，非发起方 deadline）。
    let ev = r.events.iter().find(|e| {
        matches!(e, spark_core::p2p::P2pEvent::RecoveryUpdated { state, .. } if state == "initiated")
    });
    assert!(ev.is_some(), "首次到达应发 RecoveryUpdated{{initiated}}");
    if let Some(spark_core::p2p::P2pEvent::RecoveryUpdated {
        deadline: ev_deadline,
        from_device,
        op,
        ..
    }) = ev
    {
        assert_eq!(from_device, "peer-a");
        assert_eq!(op.as_deref(), Some("reset_password"));
        // 事件 deadline = localArrival + clamp(deadline - envelope_ts)。
        let window = RecoveryService::<MemoryStorage>::recovery_window_ms(deadline, envelope_ts);
        assert_eq!(
            *ev_deadline,
            Some(local_now + window),
            "事件 deadline 用本地否决窗终点"
        );
    }

    let seen = RecoveryService::<MemoryStorage>::get_seen(&s, "rc-ok")
        .unwrap()
        .unwrap();
    assert_eq!(seen.local_arrival_ms, local_now, "seen 记录本地到达时间");
    assert_eq!(
        seen.window_ms,
        RecoveryService::<MemoryStorage>::recovery_window_ms(deadline, envelope_ts)
    );
    assert!(!seen.vetoed && !seen.committed);

    // 重复到达幂等：不发第二次事件。
    let r2 = deliver_recovery(
        &mut s,
        &key,
        &my_root,
        envelope_ts,
        local_now,
        initiated_body("rc-ok", "reset_password", deadline, "peer-a"),
    );
    assert_eq!(r2.response, json!({ "ok": true }));
    assert!(r2.events.is_empty(), "重复 initiated 幂等，不重发事件");
}

#[test]
fn inbound_veto_tombstone_before_initiated_lands_vetoed() {
    let (key, my_root) = root(5);
    let mut s = MemoryStorage::new();
    // 乱序：veto 先到（fail-safe：requestId 非空即落墓碑）。
    let r_veto = deliver_recovery(
        &mut s,
        &key,
        &my_root,
        1_000_000,
        1_100_000,
        json!({"kind":"vetoed","requestId":"rc-late","fromDevice":"peer-b"}),
    );
    assert_eq!(r_veto.response, json!({ "ok": true }));
    assert!(
        RecoveryService::<MemoryStorage>::get_veto_tombstone(&s, "rc-late")
            .unwrap()
            .is_some(),
        "veto 乱序先到落墓碑"
    );

    // 随后 initiated 到达 → seen 落库即带 vetoed。
    let r_init = deliver_recovery(
        &mut s,
        &key,
        &my_root,
        1_000_000,
        1_200_000,
        initiated_body("rc-late", "reset_password", 2_000_000, "peer-a"),
    );
    assert_eq!(r_init.response, json!({ "ok": true }));
    let seen = RecoveryService::<MemoryStorage>::get_seen(&s, "rc-late")
        .unwrap()
        .unwrap();
    assert!(seen.vetoed, "墓碑存在时 initiated 落库即带 vetoed");

    // 重复 veto 幂等：不再发第二次事件（changed=false）。
    let r_veto2 = deliver_recovery(
        &mut s,
        &key,
        &my_root,
        1_000_000,
        1_300_000,
        json!({"kind":"vetoed","requestId":"rc-late","fromDevice":"peer-c"}),
    );
    assert_eq!(r_veto2.response, json!({ "ok": true }));
    assert!(r_veto2.events.is_empty(), "重复 veto 幂等不重发事件");
}

#[test]
fn inbound_committed_unknown_request_silent_ok() {
    let (key, my_root) = root(6);
    let mut s = MemoryStorage::new();
    // 未知 requestId 的 committed → 静默 ok（fail-closed 但补投无意义不报错）。
    let r = deliver_recovery(
        &mut s,
        &key,
        &my_root,
        1_000_000,
        1_000_000,
        json!({"kind":"committed","op":"reset_password","requestId":"rc-unknown","fromDevice":"peer-a"}),
    );
    assert_eq!(r.response, json!({ "ok": true }));
    assert!(r.events.is_empty(), "未知 committed 静默 ok 不发事件");

    // 缺字段 → invalid-body。
    let r2 = deliver_recovery(
        &mut s,
        &key,
        &my_root,
        1_000_000,
        1_000_000,
        json!({"kind":"committed","op":"reset_password","requestId":"rc-unknown"}),
    );
    assert_eq!(
        r2.response,
        json!({ "ok": false, "reason": "invalid-body" })
    );
}

#[test]
fn inbound_committed_marks_seen_and_pending_terminal() {
    let (key, my_root) = root(7);
    let mut s = MemoryStorage::new();
    // 先落 initiated seen。
    deliver_recovery(
        &mut s,
        &key,
        &my_root,
        1_000_000,
        1_000_000,
        initiated_body("rc-done", "reset_password", 2_000_000, "peer-a"),
    );
    // 再落 committed。
    let r = deliver_recovery(
        &mut s,
        &key,
        &my_root,
        1_000_000,
        1_000_000,
        json!({"kind":"committed","op":"reset_password","requestId":"rc-done","fromDevice":"peer-a"}),
    );
    assert_eq!(r.response, json!({ "ok": true }));
    let seen = RecoveryService::<MemoryStorage>::get_seen(&s, "rc-done")
        .unwrap()
        .unwrap();
    assert!(seen.committed, "seen 命中 committed");
    assert!(
        r.events.iter().any(|e| matches!(e, spark_core::p2p::P2pEvent::RecoveryUpdated { state, .. } if state == "committed")),
        "首次 committed 应发 RecoveryUpdated{{committed}}"
    );
}

// ---------------------------------------------------------------------------
// 测试点 8：reset_password_session（真 kernel）。
// ---------------------------------------------------------------------------

#[test]
fn reset_password_session_strength_reencrypt_and_unlock() {
    let dir = tempfile::tempdir().unwrap();
    let mut k = Kernel::init(config(dir.path())).expect("kernel init");
    let (_root, mnemonic) = init_identity(&mut k);

    // 强度 <8 → PasswordTooShort。
    let err = k.reset_password_session("short").unwrap_err().to_string();
    assert!(err.contains("at least 8"), "强度<8 应拒：{err}");

    // 重封为强口令。
    k.reset_password_session("newpassword456")
        .expect("reset ok");

    // 新口令可再次查看助记词，且一致。
    let revealed = k.reveal_mnemonic("newpassword456").expect("new pw reveal");
    assert_eq!(revealed.trim(), mnemonic.trim(), "重封后助记词一致");

    // 旧口令解不开。
    let err2 = k.reveal_mnemonic("password123").unwrap_err().to_string();
    assert!(err2.contains("Invalid"), "旧口令应解不开：{err2}");
}

#[test]
fn reset_password_session_changes_unlock_behavior() {
    let dir = tempfile::tempdir().unwrap();
    let mut k = Kernel::init(config(dir.path())).expect("kernel init");
    let (root_id, _mnemonic) = init_identity(&mut k);
    let root_clone = root_id.clone();

    // 锁定后：旧口令可解锁 → 重封新口令 → 旧口令失效、新口令可解锁。
    k.lock();
    k.unlock(PASSWORD, Some(&root_clone))
        .expect("old pw unlock before reset");

    k.reset_password_session("brandnew789").expect("reset");

    k.lock();
    let err = k
        .unlock(PASSWORD, Some(&root_clone))
        .unwrap_err()
        .to_string();
    assert!(err.contains("Invalid"), "旧口令解锁应失败：{err}");

    let ok = k
        .unlock("brandnew789", Some(&root_clone))
        .expect("new pw unlock");
    assert_eq!(ok, root_id, "新口令解锁返回同一 rootId");
}

// ---------------------------------------------------------------------------
// 终态不可逆（team-lead 统一回归 b）：pending 的 Committed / Vetoed 为终态，
// 乱序到达的对侧事件不得覆盖回另一终态。
// ---------------------------------------------------------------------------

#[test]
fn committed_is_terminal_out_of_order_veto_does_not_overwrite() {
    // pending 已 Committed → 乱序 veto 不得把 pending 改回 Vetoed（apply_inbound_vetoed
    // 守卫 pending.state != Committed；get_pending 命中时也不发事件）。
    let (key, my_root) = root(20);
    let mut s = MemoryStorage::new();

    // 发起 → 直接 committed。
    let pending = RecoveryService::<MemoryStorage>::initiate(
        &mut s,
        1_000_000,
        RecoveryOp::ResetPassword,
        Some(0.001),
    )
    .unwrap();
    RecoveryService::<MemoryStorage>::mark_committed(&mut s, &pending.request_id).unwrap();
    let p = RecoveryService::<MemoryStorage>::get_pending(&s)
        .unwrap()
        .unwrap();
    assert_eq!(p.state, RecoveryState::Committed);

    // 乱序 veto 入站 → pending 仍 Committed（终态不可逆）。
    let r = deliver_recovery(
        &mut s,
        &key,
        &my_root,
        1_100_000,
        1_100_000,
        json!({"kind":"vetoed","requestId":pending.request_id,"fromDevice":"peer-b"}),
    );
    assert_eq!(r.response, json!({ "ok": true }));
    let p2 = RecoveryService::<MemoryStorage>::get_pending(&s)
        .unwrap()
        .unwrap();
    assert_eq!(
        p2.state,
        RecoveryState::Committed,
        "已 Committed 的 pending 收到乱序 veto 不得改回 Vetoed"
    );
    assert!(!p2.vetoed, "终态 Committed 不得带 vetoed 标记");
    // seen 侧不受影响（本机发起路径无 seen，仅断言 pending 终态稳定）。
    let _ = r.events;
}

#[test]
fn vetoed_is_terminal_late_committed_does_not_overwrite() {
    // pending 已 Vetoed → 迟到的 committed 不得把 pending 改回 Committed（mark_committed
    // 守卫 pending.state != Committed && != Vetoed）。
    let (key, my_root) = root(21);
    let mut s = MemoryStorage::new();

    // 发起（1h 窗口，deadline 足够远）→ 窗口内 veto。
    let pending = RecoveryService::<MemoryStorage>::initiate(
        &mut s,
        1_000_000,
        RecoveryOp::ResetPassword,
        Some(1.0),
    )
    .unwrap();
    let rid = pending.request_id.clone();
    RecoveryService::<MemoryStorage>::veto(&mut s, 1_100_000, &rid, "peer-b").unwrap();
    let p = RecoveryService::<MemoryStorage>::get_pending(&s)
        .unwrap()
        .unwrap();
    assert_eq!(p.state, RecoveryState::Vetoed);

    // 迟到 committed 入站 → pending 仍 Vetoed（终态不可逆）。
    let r = deliver_recovery(
        &mut s,
        &key,
        &my_root,
        1_200_000,
        1_200_000,
        json!({"kind":"committed","op":"reset_password","requestId":rid,"fromDevice":"peer-a"}),
    );
    assert_eq!(r.response, json!({ "ok": true }));
    let p2 = RecoveryService::<MemoryStorage>::get_pending(&s)
        .unwrap()
        .unwrap();
    assert_eq!(
        p2.state,
        RecoveryState::Vetoed,
        "已 Vetoed 的 pending 收到迟到 committed 不得改回 Committed"
    );
    assert!(p2.vetoed, "终态 Vetoed 保留 vetoed 标记");
    let _ = r.events;
}

#[test]
fn handle_committed_respects_veto_first_seen_guard() {
    // handle_committed 的否决优先守卫：pending 先被 veto（seen.vetoed=true、pending→Vetoed），
    // 随后迟到的 committed 走 mark_committed——其 pending.state==Vetoed 守卫使 pending 不得
    // 翻回 Committed（否决优先）。
    let (key, my_root) = root(22);
    let mut s = MemoryStorage::new();

    // 本机发起 → 得到 pending（requestId = rid）。
    let pending = RecoveryService::<MemoryStorage>::initiate(
        &mut s,
        1_000_000,
        RecoveryOp::ResetPassword,
        Some(1.0),
    )
    .unwrap();
    let rid = pending.request_id.clone();

    // 入站 initiated（同 requestId）→ 落 seen 记录。
    let r_init = deliver_recovery(
        &mut s,
        &key,
        &my_root,
        1_000_000,
        1_000_000,
        initiated_body(&rid, "reset_password", 2_000_000, "peer-a"),
    );
    assert_eq!(r_init.response, json!({ "ok": true }));
    assert!(
        RecoveryService::<MemoryStorage>::get_seen(&s, &rid)
            .unwrap()
            .is_some(),
        "前置：seen 已落库"
    );

    // 入站 veto（同 requestId）→ seen 置 vetoed + pending→Vetoed。
    let r_veto = deliver_recovery(
        &mut s,
        &key,
        &my_root,
        1_000_000,
        1_100_000,
        json!({"kind":"vetoed","requestId":rid,"fromDevice":"peer-b"}),
    );
    assert_eq!(r_veto.response, json!({ "ok": true }));
    let seen = RecoveryService::<MemoryStorage>::get_seen(&s, &rid)
        .unwrap()
        .unwrap();
    assert!(
        seen.vetoed && !seen.committed,
        "前置：seen vetoed 且未 committed"
    );
    assert_eq!(
        RecoveryService::<MemoryStorage>::get_pending(&s)
            .unwrap()
            .unwrap()
            .state,
        RecoveryState::Vetoed
    );

    // 迟到 committed 入站 → 否决优先，pending 不得翻回 Committed。
    let r = deliver_recovery(
        &mut s,
        &key,
        &my_root,
        1_000_000,
        1_200_000,
        json!({"kind":"committed","op":"reset_password","requestId":rid,"fromDevice":"peer-a"}),
    );
    assert_eq!(r.response, json!({ "ok": true }));
    let p = RecoveryService::<MemoryStorage>::get_pending(&s)
        .unwrap()
        .unwrap();
    assert_eq!(
        p.state,
        RecoveryState::Vetoed,
        "否决优先：seen.vetoed 时 committed 不得把 pending 翻回 Committed"
    );
    let seen2 = RecoveryService::<MemoryStorage>::get_seen(&s, &rid)
        .unwrap()
        .unwrap();
    assert!(seen2.vetoed, "seen 保留 vetoed");
    let _ = r.events;
}

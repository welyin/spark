//! A5 验收：改密三件套（乙 + V + D′）真实双 kernel + P2P 端到端。
//!
//! - 用例 1（双设备改密 e2e）：A 改密 → B 引导态（stale + 新 V）→ D′ 断粮
//!   （grace=0 下 A 暂扣 epoch2 包裹）→ B 验票 + unify（V 校验 + 本机重封 +
//!   ack）→ A 锚定授权补发 → B 拿到新 epoch 密钥恢复数据面。
//! - 用例 2（§13.7/13.8 QR 旧口令恢复 gated）：QR（V1）早于他端改密（V2）→
//!   B 恢复后经 D′ 门控暂扣（无新 epoch 密钥）→ verify_ticket 新口令 →
//!   unify → ack → A 锚定补发 → B 拿到新 epoch 密钥。
//!
//! 注：D′ 数据面断言一律钉 epoch 密钥材料（ikey 包裹 / 本机密钥表 /
//! effective），不钉头像——profile-sync 直发通道（host dm profile_apply）
//! 不经 epoch 门控，头像在 gated 期间也会到达，不是敏感面代理。
//!
//! 与 kernel_identity 的 QR 收敛用例同型：共享宿主机 P2P 端口/身份 seed，
//! 文件内互斥串行，避免撞资源与 dm 限流窗口。

mod common;

use common::*;
use spark_core::epoch::RotationReason;
use spark_core::kernel::{Kernel, KernelError};
use spark_core::pw;
use spark_core::storage::StorageBackend;

const NEW_PW: &str = "account-new-password";

/// 30KB 大头像：QR 紧凑载荷必裁剪（<3KB 预算），恢复设备只能靠「epoch 密钥
/// 解开 profile 记录」找回——头像在场即数据面已通的硬证据（小头像会被 QR
/// 载荷直接携带，断言失效）。
fn big_avatar() -> String {
    format!("data:image/png;base64,{}", "A".repeat(30 * 1024))
}

/// 文件内互斥（真实 P2P 双 kernel 用例串行）。
static PW_E2E_SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn e2e_guard() -> std::sync::MutexGuard<'static, ()> {
    PW_E2E_SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

fn peer_id(kernel: &Kernel) -> String {
    kernel
        .p2p_status()
        .unwrap()
        .and_then(|s| s.peer_id)
        .expect("p2p peerId")
}

fn pwv_changed_at(kernel: &Kernel) -> u64 {
    let s = kernel.__test_storage().unwrap();
    pw::get_pwv(&s).unwrap().expect("pwv 存在").changed_at
}

fn last_verified(kernel: &Kernel, peer: &str) -> u64 {
    let s = kernel.__test_storage().unwrap();
    pw::get_last_verified_vts(&s, peer).unwrap()
}

fn has_ikey(kernel: &Kernel, epoch: u64, writer: &str, recipient: &str) -> bool {
    let s = kernel.__test_storage().unwrap();
    s.get(&spark_core::epoch::ikey_key(epoch, writer, recipient))
        .unwrap()
        .is_some()
}

fn wait_connected(a: &Kernel, b_peer: &str, what: &str) {
    wait_until(
        || {
            a.p2p_status()
                .unwrap()
                .unwrap()
                .connected_peers
                .iter()
                .any(|p| p == b_peer)
        },
        20_000,
        what,
    );
}

// ---------------------------------------------------------------------------
// 用例 1：双设备改密 e2e（identity.md §六：A 改密 → B 引导态 → 输新密码
// （V 校验 + 重封 + ack）→ B 恢复数据面；遗忘设备 grace 后断粮——以 graceMs=0
// 把「grace 耗尽」折叠成立刻，同一链路覆盖两个验收点）。
// ---------------------------------------------------------------------------
#[test]
fn password_change_dual_device_gated_then_unify_converges() {
    let _guard = e2e_guard();
    let avatar = big_avatar();
    let dir_a = tempfile::tempdir().unwrap();
    let mut kernel_a = fresh_kernel(dir_a.path());
    kernel_a
        .init_identity(PASSWORD, "小明", Some(&avatar))
        .unwrap();
    // grace=0：曾验证设备一旦锚落后即视为超 grace（折叠 7 天等待）。
    {
        let mut s = kernel_a.__test_storage().unwrap();
        pw::put_grace_ms(&mut s, 0).unwrap();
    }
    kernel_a.start_p2p().unwrap();
    let qr = kernel_a.backup_payload_qr(PASSWORD).unwrap();

    // B 经 QR 恢复（同口令），建立基线数据面（A 锚定 B + B 拿到 epoch1 密钥）。
    // 注：基线/恢复断言一律钉 epoch 密钥材料（profile-sync 直发通道 A45 已
    // 纳管 D′，但其挂起/补到语义由用例 2 专断，此处不重复断言）。
    let dir_b = tempfile::tempdir().unwrap();
    let mut kernel_b = fresh_kernel(dir_b.path());
    kernel_b.recover_backup(&qr, PASSWORD).unwrap();
    kernel_b.start_p2p().unwrap();
    let a_peer = peer_id(&kernel_a);
    let b_peer = peer_id(&kernel_b);
    wait_connected(&kernel_b, &a_peer, "B 连接 A");
    wait_connected(&kernel_a, &b_peer, "A 连接 B");
    let v1_changed = pwv_changed_at(&kernel_a);
    wait_until(
        || last_verified(&kernel_a, &b_peer) == v1_changed,
        90_000,
        "基线：A 锚定 B@V1（ack MAC 校验通过）",
    );
    wait_until(
        || {
            let s = kernel_b.__test_storage().unwrap();
            spark_core::epoch::get_effective(&s).unwrap() == 1
                && spark_core::epoch::get_local_key(&s, 1).unwrap().is_some()
        },
        90_000,
        "基线：B 拿到 epoch1 密钥（数据面已通）",
    );

    // A 改密：publish-first + 时戳同源；B（锚@V1、grace=0）在 epoch2 门控暂扣。
    kernel_a.change_password(PASSWORD, NEW_PW).unwrap();
    let v2_changed = pwv_changed_at(&kernel_a);
    assert!(v2_changed > v1_changed, "改密发布新 V");
    {
        let s = kernel_a.__test_storage().unwrap();
        let state = spark_core::epoch::get_epoch_state(&s).unwrap().unwrap();
        assert_eq!(state.current, 2, "改密触发 epoch2");
        assert_eq!(
            state.rotated_at as u64, v2_changed,
            "时戳同源：rotatedAt == pwv.changedAt（§13.5）"
        );
        assert!(
            s.get(&spark_core::epoch::ikey_key(2, &a_peer, &b_peer))
                .unwrap()
                .is_none(),
            "D′ 断粮：B 锚@V1 超 grace（graceMs=0），epoch2 包裹暂扣"
        );
    }

    // B 引导态：收到新 V + stale 置位（会话旧口令解不开 V2，auto-unify 不触发）。
    wait_until(
        || pwv_changed_at(&kernel_b) == v2_changed,
        60_000,
        "B 收到新 V",
    );
    {
        let s = kernel_b.__test_storage().unwrap();
        assert!(pw::get_stale(&s).unwrap(), "B 引导态：stale 置位");
    }

    // B 验票：新口令过、旧口令/错口令 ticket-mismatch（不动文件不清标记）。
    kernel_b
        .verify_password_ticket(NEW_PW)
        .expect("新口令验票通过");
    assert!(matches!(
        kernel_b.verify_password_ticket(PASSWORD),
        Err(KernelError::TicketMismatch)
    ));
    // 旧口令错误分支：unify 本机旧密校验 → invalid-password。
    assert!(matches!(
        kernel_b.unify_password("not-the-old-password", NEW_PW, RotationReason::PasswordChange),
        Err(KernelError::InvalidPassword)
    ));

    // B unify：V 复验（新口令）+ 本机重封 + ack。
    kernel_b
        .unify_password(PASSWORD, NEW_PW, RotationReason::PasswordChange)
        .expect("分叉场景 unify 成功");
    {
        let s = kernel_b.__test_storage().unwrap();
        assert!(!pw::get_stale(&s).unwrap(), "unify 后清 stale");
        assert_eq!(
            pw::get_applied_vts(&s).unwrap(),
            v2_changed,
            "B 水位覆盖 V2"
        );
        let ack = pw::get_pwack(&s, &b_peer).unwrap().expect("B 已发 ack");
        assert_eq!(ack.v_ts, v2_changed, "ack 引用 V2 changedAt");
    }

    // A 锚定 B@V2 → 授权补发 epoch2 包裹 → B 密钥表落表（数据面恢复）。
    wait_until(
        || last_verified(&kernel_a, &b_peer) == v2_changed,
        90_000,
        "A 锚定 B@V2（ack 到达 + MAC 校验）",
    );
    wait_until(
        || has_ikey(&kernel_a, 2, &a_peer, &b_peer),
        90_000,
        "A 授权补发 epoch2 包裹",
    );
    wait_until(
        || {
            let s = kernel_b.__test_storage().unwrap();
            spark_core::epoch::get_effective(&s).unwrap() == 2
                && spark_core::epoch::get_local_key(&s, 2).unwrap().is_some()
        },
        90_000,
        "B 拿到 epoch2 密钥（数据面恢复）",
    );

    kernel_a.shutdown().unwrap();
    kernel_b.shutdown().unwrap();
}

// ---------------------------------------------------------------------------
// 用例 2（§13.7/13.8）：QR 旧口令恢复 gated → unify → 收敛。
// QR（携带 V1）早于 A 改密（V2）；B 恢复后对 A 是「从未验证设备」（D2 可信锚
// 模型：旧 V 的 ack MAC 对 A 的新口令会话不可验，不锚定）→ 立即暂扣、敏感面
// 停更；输一次新口令完成 unify 后收敛。
// ---------------------------------------------------------------------------
#[test]
fn qr_stale_password_recovery_gated_then_unify_converges() {
    let _guard = e2e_guard();
    let avatar = big_avatar();
    let dir_a = tempfile::tempdir().unwrap();
    let mut kernel_a = fresh_kernel(dir_a.path());
    kernel_a
        .init_identity(PASSWORD, "小明", Some(&avatar))
        .unwrap();
    kernel_a.start_p2p().unwrap();
    // 先导出 QR（携带 V1），再改密（V2）——QR 口令自此分叉为旧口令。
    let qr = kernel_a.backup_payload_qr(PASSWORD).unwrap();
    let v1_changed = pwv_changed_at(&kernel_a);
    kernel_a.change_password(PASSWORD, NEW_PW).unwrap();
    let v2_changed = pwv_changed_at(&kernel_a);
    assert!(v2_changed > v1_changed);

    // B 用旧口令 QR 恢复：本机解锁合法（本机面），敏感数据面暂扣。
    let dir_b = tempfile::tempdir().unwrap();
    let mut kernel_b = fresh_kernel(dir_b.path());
    kernel_b.recover_backup(&qr, PASSWORD).unwrap();
    // 恢复即注入 A 的 V1（不自建分叉 V）。
    assert_eq!(
        pwv_changed_at(&kernel_b),
        v1_changed,
        "B 恢复后 pwv == QR 携带的 V1"
    );
    // B 侧 grace=0（A45 profile 门控断言用：B 自锚@V1 一旦落后即超 grace，
    // profile-sync 直发通道随 epoch 面一并暂扣）。
    {
        let mut s = kernel_b.__test_storage().unwrap();
        pw::put_grace_ms(&mut s, 0).unwrap();
    }

    kernel_b.start_p2p().unwrap();
    let a_peer = peer_id(&kernel_a);
    let b_peer = peer_id(&kernel_b);
    wait_connected(&kernel_b, &a_peer, "B 连接 A");
    wait_connected(&kernel_a, &b_peer, "A 连接 B");

    // B 收到 V2 → stale（引导态）；旧口令会话解不开 V2，auto-unify 不触发。
    wait_until(
        || pwv_changed_at(&kernel_b) == v2_changed,
        60_000,
        "B 收到 V2",
    );
    // 给 A 侧锚定/补发链路留足反熵轮次后断言 gated 稳态：
    // A 不锚定 B（旧 V 的 ack MAC 对新口令会话不可验）→ 从未验证立即暂扣。
    std::thread::sleep(std::time::Duration::from_secs(3));
    {
        let s = kernel_b.__test_storage().unwrap();
        assert!(pw::get_stale(&s).unwrap(), "B 引导态：stale 置位");
        assert!(
            spark_core::epoch::get_local_key(&s, 2).unwrap().is_none(),
            "gated：B 无 epoch2 密钥（敏感面停更）"
        );
    }
    assert_eq!(
        last_verified(&kernel_a, &b_peer),
        0,
        "A 不锚定旧 V 的 ack（MAC 不可验）→ B 从未验证"
    );
    assert!(
        !has_ikey(&kernel_a, 2, &a_peer, &b_peer),
        "gated：A 暂扣 epoch2 包裹"
    );
    // A45：profile-sync 直发通道已纳管 D′。确定性时序——B 已 gated 后 A 再改
    // 资料（配对握手期的首份快照可能与 V2 竞态，不作断言对象）：A 的新快照
    // 到达 gated 的 B → 挂起（stash），不应用进身份文件。
    let avatar2 = format!("data:image/png;base64,{}", "B".repeat(30 * 1024));
    kernel_a
        .update_profile(PASSWORD, None, Some(Some(&avatar2)), None, None, None)
        .unwrap();
    wait_until(
        || {
            let s = kernel_b.__test_storage().unwrap();
            s.get(pw::PENDING_PROFILE_SYNC_KEY).unwrap().is_some()
        },
        90_000,
        "A 改资料后快照被 D′ 门控挂起（stash）",
    );
    assert_ne!(
        kernel_b
            .current_identity()
            .unwrap()
            .unwrap()
            .avatar
            .as_deref(),
        Some(avatar2.as_str()),
        "gated：新资料快照不应用"
    );

    // B 输一次新口令：verify_ticket → unify（V 校验 + 重封 + ack）。
    kernel_b
        .verify_password_ticket(NEW_PW)
        .expect("新口令验票通过");
    kernel_b
        .unify_password(PASSWORD, NEW_PW, RotationReason::PasswordChange)
        .expect("unify 成功");

    // A 锚定 B@V2 → 授权补发 → B 拿 epoch2 密钥（数据面恢复）。
    wait_until(
        || last_verified(&kernel_a, &b_peer) == v2_changed,
        90_000,
        "A 锚定 B@V2",
    );
    wait_until(
        || has_ikey(&kernel_a, 2, &a_peer, &b_peer),
        90_000,
        "A 授权补发 epoch2 包裹",
    );
    wait_until(
        || {
            let s = kernel_b.__test_storage().unwrap();
            spark_core::epoch::get_effective(&s).unwrap() == 2
                && spark_core::epoch::get_local_key(&s, 2).unwrap().is_some()
        },
        90_000,
        "B 拿到 epoch2 密钥（数据面恢复）",
    );
    // A45：ack 补齐门控转 Pass → 挂起的资料快照补应用（或下轮重发收敛）。
    wait_until(
        || {
            kernel_b
                .current_identity()
                .unwrap()
                .unwrap()
                .avatar
                .as_deref()
                == Some(avatar2.as_str())
        },
        90_000,
        "ack 后 profile 补到：新头像应用",
    );

    kernel_a.shutdown().unwrap();
    kernel_b.shutdown().unwrap();
}

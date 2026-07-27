//! keepalive 判定逻辑（拨号计划、交换目标轮换、恢复触发节奏）单测。

use std::collections::HashSet;

use spark_core::p2p::constants::{RECOVERY_COOLDOWN_MS, RECOVERY_SEARCH_DISPLAY_MS};
use spark_core::p2p::keepalive::*;
use spark_core::p2p::peer_targets::PeerNodeInfo;

fn info(peer_id: &str) -> PeerNodeInfo {
    PeerNodeInfo {
        peer_id: Some(peer_id.to_string()),
        addresses: vec!["/ip4/1.2.3.4/tcp/1/ws".to_string()],
    }
}

#[test]
fn overlay_budget() {
    assert_eq!(overlay_dial_budget(0), 2);
    assert_eq!(overlay_dial_budget(3), 1);
    assert_eq!(overlay_dial_budget(4), 0);
    assert_eq!(overlay_dial_budget(10), 0);
}

#[test]
fn org_dial_plan_caps_at_max() {
    let candidates = vec![info("a"), info("b"), info("c"), info("d"), info("e")];
    let connected = HashSet::from(["a".to_string()]);
    let (to_dial, conn) = plan_organization_dials(&candidates, &connected, 3);
    assert_eq!(conn.len(), 1);
    assert_eq!(to_dial.len(), 3);
    let ids: Vec<String> = to_dial.iter().filter_map(|c| c.peer_id.clone()).collect();
    assert_eq!(ids, vec!["b", "c", "d"]);
}

#[test]
fn exchange_target_rotates() {
    let connected: HashSet<String> = ["a", "b", "c", "self"]
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(
        pick_exchange_target(&connected, "self", 0).as_deref(),
        Some("a")
    );
    assert_eq!(
        pick_exchange_target(&connected, "self", 1).as_deref(),
        Some("b")
    );
    assert_eq!(
        pick_exchange_target(&connected, "self", 3).as_deref(),
        Some("a")
    );
    let empty: HashSet<String> = HashSet::new();
    assert_eq!(pick_exchange_target(&empty, "self", 0), None);
}

#[test]
fn recovery_trigger_cadence() {
    let mut trigger = RecoveryTrigger::new();
    // 连续 2 个 tick 不触发
    assert!(!trigger.on_tick(true, 1000));
    assert!(!trigger.on_tick(true, 2000));
    // 第 3 个 tick 触发并记录查询时间
    assert!(trigger.on_tick(true, 3000));
    // 冷却期内不触发（即使连续 tick）
    assert!(!trigger.on_tick(true, 4000));
    // 可达时清零
    assert!(!trigger.on_tick(false, 5000));
    assert!(!trigger.on_tick(true, 6000));
    // 冷却过后 + 连续 3 tick 再次触发
    assert!(!trigger.on_tick(true, 7000));
    assert!(!trigger.on_tick(true, 8000));
    assert!(trigger.on_tick(true, 3000 + RECOVERY_COOLDOWN_MS + 1));
}

#[test]
fn recovery_state_snapshot() {
    let mut trigger = RecoveryTrigger::new();
    // 从未查询 → Idle
    assert_eq!(trigger.state(10_000), RecoveryState::Idle);
    assert_eq!(trigger.state(10_000).as_str(), "idle");
    assert_eq!(trigger.state(10_000).since(), None);
    // 连续 3 tick 触发查询后，窗口内 → Recovering
    assert!(!trigger.on_tick(true, 1_000));
    assert!(!trigger.on_tick(true, 2_000));
    assert!(trigger.on_tick(true, 3_000));
    assert_eq!(
        trigger.state(3_000 + RECOVERY_SEARCH_DISPLAY_MS),
        RecoveryState::Recovering { since: 3_000 }
    );
    assert_eq!(trigger.state(3_000).as_str(), "recovering");
    assert_eq!(trigger.state(3_000).since(), Some(3_000));
    // 超过显示窗口 → Failed
    assert_eq!(
        trigger.state(3_000 + RECOVERY_SEARCH_DISPLAY_MS + 1),
        RecoveryState::Failed { since: 3_000 }
    );
    assert_eq!(trigger.state(i64::MAX).as_str(), "failed");
}

#[test]
fn recovery_dial_plan_dedupes() {
    let candidates = vec![
        info("a"),
        info("a"),
        PeerNodeInfo {
            peer_id: None,
            addresses: vec!["/x".to_string()],
        },
        PeerNodeInfo {
            peer_id: None,
            addresses: vec!["/x".to_string()],
        },
        info("b"),
    ];
    let plan = plan_recovery_dials(&candidates, 4);
    assert_eq!(plan.len(), 3);
}

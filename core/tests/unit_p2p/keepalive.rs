//! keepalive 判定逻辑（拨号计划、交换目标轮换、恢复触发节奏）单测。

use std::collections::HashSet;

use spark_core::p2p::constants::RECOVERY_SEARCH_DISPLAY_MS;
use spark_core::p2p::keepalive::*;
use spark_core::p2p::peer_targets::PeerNodeInfo;

fn info(peer_id: &str) -> PeerNodeInfo {
    PeerNodeInfo {
        peer_id: Some(peer_id.to_string()),
        addresses: vec!["/ip4/1.2.3.4/tcp/1/ws".to_string()],
    }
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
fn recovery_state_snapshot() {
    // connection-policy M6 后恢复查询改为事件点触发的 DHT 刷新：不再有 tick
    // cadence（on_tick 删除），只记录最近一轮查询时间供 UI 展示。
    let mut trigger = RecoveryTrigger::new();
    // 从未查询 → Idle
    assert_eq!(trigger.state(10_000), RecoveryState::Idle);
    assert_eq!(trigger.state(10_000).as_str(), "idle");
    assert_eq!(trigger.state(10_000).since(), None);
    // 懒连接链 DHT 刷新发起查询后，窗口内 → Recovering
    trigger.note_query(3_000);
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

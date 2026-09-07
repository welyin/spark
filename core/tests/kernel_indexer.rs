//! indexer 目录/子集覆盖集成测试（affair-metadata §7 目录面 + §8 查询）：
//! - 门面层（无网络）：覆盖配置线形校验、启用位与覆盖的独立读写、目录
//!   读路径、未启用/未启动守卫；
//! - e2e（双内核 loopback）：indexer-card gossip 自公告 → 对端目录发现 →
//!   查询应答门控（indexer-disabled / indexer-not-covered / 覆盖内正常应答）
//!   → 覆盖配置对 gossip 收录面的过滤。

mod common;

use std::time::Duration;

use serde_json::{Map, Value, json};
use spark_core::index::SearchQuery;
use spark_core::index::query::{build_query, parse_result};
use spark_core::p2p::constants::AFFAIR_META_TOPIC;
use spark_core::p2p::node::system_now_ms;

use common::*;

/// 门面层（无网络）：覆盖配置校验与守卫。
#[test]
fn coverage_config_and_facade_guards() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    init_identity(&mut kernel);

    // 默认：关闭 + 全覆盖
    let cfg = kernel.indexer_config();
    assert!(!cfg.enabled);
    assert!(cfg.coverage.is_full());

    // 覆盖配置线形校验：条目长度/条数越限拒绝，且失败不破坏既有配置
    kernel
        .set_indexer_coverage(vec!["110105".to_string()], vec!["hoa".to_string()])
        .unwrap();
    let err = kernel
        .set_indexer_coverage(vec!["x".repeat(33)], vec![])
        .unwrap_err();
    assert!(err.to_string().contains("invalid coverage"), "{err}");
    let err = kernel
        .set_indexer_coverage((0..17).map(|i| i.to_string()).collect(), vec![])
        .unwrap_err();
    assert!(err.to_string().contains("invalid coverage"), "{err}");
    let cfg = kernel.indexer_config();
    assert_eq!(cfg.coverage.regions, vec!["110105".to_string()]);
    assert_eq!(cfg.coverage.topics, vec!["hoa".to_string()]);

    // set_indexer_enabled 只动启用位，保留覆盖配置
    kernel.set_indexer_enabled(true);
    assert!(kernel.indexer_enabled());
    assert_eq!(
        kernel.indexer_config().coverage.topics,
        vec!["hoa".to_string()],
        "启用位切换不得重置覆盖"
    );

    // 目录初始为空
    assert!(kernel.indexer_directory().unwrap().is_empty());

    // 角色关闭时发布名片报 disabled
    kernel.set_indexer_enabled(false);
    let err = kernel.indexer_publish_card().unwrap_err();
    assert!(err.to_string().contains("indexer role disabled"), "{err}");

    // p2p 已随身份初始化自动启动（config 带 p2p）：启用后发布名片成功，
    // payload 过校验链且 peerId 为本机；自卡落本地目录
    kernel.set_indexer_enabled(true);
    let payload = kernel.indexer_publish_card().unwrap();
    let card = spark_core::index::directory::parse_card(&payload).unwrap();
    let self_peer = kernel.p2p_status().unwrap().unwrap().peer_id.unwrap();
    assert_eq!(card.peer_id, self_peer);
    assert_eq!(card.coverage.regions, vec!["110105".to_string()]);
    let dir = kernel.indexer_directory().unwrap();
    assert!(
        dir.iter().any(|e| e["peerId"] == json!(self_peer.clone())),
        "自卡应落本地目录：{dir:?}"
    );

    // 查询未连接且无邻居池地址的 peer：拨号失败报错
    let frame = build_query(
        "q0",
        &SearchQuery {
            text: "选举".to_string(),
            limit: 5,
            ..SearchQuery::plain("", 0)
        },
    );
    let err = kernel
        .indexer_query("12D3KooWSomePeerId", &frame)
        .unwrap_err();
    assert!(!err.to_string().is_empty());
}

/// 元数据公告广播体（type='affair-meta'、domain='affair'、id=affairId）。
fn meta_announce_body(affair_id: &str, title: &str, region: &str) -> Map<String, Value> {
    let mut body = Map::new();
    body.insert("type".to_string(), json!("affair-meta"));
    body.insert("domain".to_string(), json!("affair"));
    body.insert("id".to_string(), json!(affair_id));
    body.insert(
        "payload".to_string(),
        json!({
            "metaV": 1,
            "affairId": affair_id,
            "title": title,
            "summary": "",
            "tags": [format!("region:{region}")],
            "metaSeq": 0,
            "basisOpHash": affair_id,
            "updatedAt": system_now_ms(),
        }),
    );
    body
}

/// e2e：indexer-card 自公告 → 目录发现 → 查询门控 → 覆盖过滤收录。
#[test]
fn indexer_card_directory_and_query_e2e() {
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    let mut kernel_a = fresh_kernel(dir_a.path());
    let mut kernel_b = fresh_kernel(dir_b.path());
    init_identity(&mut kernel_a);
    init_identity(&mut kernel_b);
    kernel_a.start_p2p().unwrap();
    kernel_b.start_p2p().unwrap();
    let a_peer = kernel_a.p2p_status().unwrap().unwrap().peer_id.unwrap();
    let b_peer = kernel_b.p2p_status().unwrap().unwrap().peer_id.unwrap();

    // 互连：互导节点名片（同时把地址灌进邻居池，indexer_query 的
    // 未连接重连路径依赖邻居池地址）
    let card_a = kernel_a.make_node_card(None).unwrap();
    assert_eq!(kernel_b.import_node_card(&card_a).unwrap().connect_error, None);
    let card_b = kernel_b.make_node_card(None).unwrap();
    assert_eq!(kernel_a.import_node_card(&card_b).unwrap().connect_error, None);
    wait_until(
        || {
            kernel_a
                .p2p_status()
                .map(|s| s.unwrap().connected_peers.contains(&b_peer))
                .unwrap_or(false)
                && kernel_b
                    .p2p_status()
                    .map(|s| s.unwrap().connected_peers.contains(&a_peer))
                    .unwrap_or(false)
        },
        10_000,
        "A/B 互连",
    );

    let covered_query = build_query(
        "q1",
        &SearchQuery {
            text: "选举".to_string(),
            limit: 5,
            region: Some("110105".to_string()),
            ..SearchQuery::plain("", 0)
        },
    );

    // B 未启用角色：A 查询 B → indexer-disabled
    let resp = kernel_a.indexer_query(&b_peer, &covered_query).unwrap();
    assert_eq!(parse_result(&resp).unwrap()["error"], json!("indexer-disabled"));

    // A 启用角色 + 区域覆盖 110105，发布目录名片 → B 目录发现 A
    kernel_a.set_indexer_enabled(true);
    kernel_a
        .set_indexer_coverage(vec!["110105".to_string()], vec![])
        .unwrap();
    // gossipsub mesh 建立需要订阅传播：重发直至 B 目录收录
    std::thread::sleep(Duration::from_millis(800));
    wait_until(
        || {
            kernel_a.indexer_publish_card().ok();
            std::thread::sleep(Duration::from_millis(300));
            kernel_b
                .indexer_directory()
                .map(|d| d.iter().any(|e| e["peerId"] == json!(a_peer.clone())))
                .unwrap_or(false)
        },
        15_000,
        "B 目录发现 A 的名片",
    );
    let dir = kernel_b.indexer_directory().unwrap();
    let entry = dir.iter().find(|e| e["peerId"] == json!(a_peer.clone())).unwrap();
    assert_eq!(entry["regions"], json!(["110105"]), "名片携带覆盖配置");

    // 覆盖内查询（region=110105）：正常应答（空结果集也算正常应答）
    let resp = kernel_b.indexer_query(&a_peer, &covered_query).unwrap();
    let payload = parse_result(&resp).unwrap();
    assert!(
        payload.get("results").is_some(),
        "覆盖内查询应正常应答：{payload}"
    );

    // 覆盖外查询（无 region 过滤）：indexer-not-covered
    // （应答侧逐请求方限流 5s，须跨过窗口）
    std::thread::sleep(Duration::from_millis(5_500));
    let uncovered_query = build_query(
        "q2",
        &SearchQuery {
            text: "选举".to_string(),
            limit: 5,
            ..SearchQuery::plain("", 0)
        },
    );
    let resp = kernel_b.indexer_query(&a_peer, &uncovered_query).unwrap();
    assert_eq!(
        parse_result(&resp).unwrap()["error"],
        json!("indexer-not-covered")
    );

    // 覆盖过滤收录：B 启用角色 + 覆盖 110105；A 广播两条公告
    // （region:110105 / region:110119）→ B 只收录覆盖内条目
    kernel_b.set_indexer_enabled(true);
    kernel_b
        .set_indexer_coverage(vec!["110105".to_string()], vec![])
        .unwrap();
    let id_in = "aa".repeat(32);
    let id_out = "bb".repeat(32);
    let search_frame = build_query(
        "q3",
        &SearchQuery {
            text: "公告".to_string(),
            limit: 10,
            ..SearchQuery::plain("", 0)
        },
    );
    let search_hit = |kernel: &spark_core::kernel::Kernel| -> Vec<String> {
        let resp = kernel.indexer_search(&search_frame).unwrap();
        parse_result(&resp).unwrap()["results"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|r| r["affairId"].as_str().map(ToString::to_string))
            .collect()
    };
    wait_until(
        || {
            kernel_a
                .p2p_broadcast(AFFAIR_META_TOPIC, meta_announce_body(&id_in, "覆盖内公告", "110105"))
                .ok();
            kernel_a
                .p2p_broadcast(AFFAIR_META_TOPIC, meta_announce_body(&id_out, "覆盖外公告", "110119"))
                .ok();
            std::thread::sleep(Duration::from_millis(300));
            search_hit(&kernel_b).contains(&id_in)
        },
        15_000,
        "B 收录覆盖内公告",
    );
    let hits = search_hit(&kernel_b);
    assert!(hits.contains(&id_in), "覆盖内公告应被收录");
    assert!(!hits.contains(&id_out), "覆盖外公告不得入子集 indexer 索引");

    kernel_a.shutdown().unwrap();
    kernel_b.shutdown().unwrap();
}

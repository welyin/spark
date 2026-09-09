//! 壳侧 DTO 单测：serde 形状与 core 类型映射。

use spark_app_lib::commands::dto::*;
use spark_core::collection::FilterOp;
use spark_core::schema::SyncStrategy;

#[test]
fn collection_config_dto_defaults_and_strategy_mapping() {
    let dto: CollectionConfigDto = serde_json::from_str("{}").unwrap();
    let config = dto.into_config().unwrap();
    assert!(config.indexed_fields.is_empty());
    assert_eq!(config.sync_strategy, None);

    let dto: CollectionConfigDto =
        serde_json::from_str(r#"{"indexedFields":["author.id"],"syncStrategy":"lww"}"#).unwrap();
    let config = dto.into_config().unwrap();
    assert_eq!(config.indexed_fields, vec!["author.id".to_string()]);
    assert_eq!(config.sync_strategy, Some(SyncStrategy::Lww));

    let dto: CollectionConfigDto =
        serde_json::from_str(r#"{"syncStrategy":"bogus"}"#).unwrap();
    assert!(dto.into_config().is_err());
}

#[test]
fn query_options_dto_maps_ops() {
    let dto: QueryOptionsDto = serde_json::from_str(
        r#"{"limit":10,"reverse":true,"filter":[{"field":"kind","value":"post"},{"field":"ts","value":5,"op":"gte"}]}"#,
    )
    .unwrap();
    let options = dto.into_options().unwrap();
    assert_eq!(options.limit, Some(10));
    assert!(options.reverse);
    assert_eq!(options.filter.len(), 2);
    assert_eq!(options.filter[0].op, FilterOp::Eq);
    assert_eq!(options.filter[1].op, FilterOp::Gte);

    let bad: QueryOptionsDto =
        serde_json::from_str(r#"{"filter":[{"field":"a","value":1,"op":"nope"}]}"#).unwrap();
    assert!(bad.into_options().is_err());
}

#[test]
fn query_result_dto_uses_camel_case_cursor() {
    let dto = QueryResultDto {
        items: vec![DocItemDto {
            id: "a".into(),
            data: serde_json::json!({"x": 1}),
        }],
        next_cursor: Some("a".into()),
    };
    let text = serde_json::to_string(&dto).unwrap();
    assert!(text.contains("\"nextCursor\":\"a\""));
    assert!(text.contains("\"items\""));
}

#[test]
fn p2p_info_dto_stopped_shape() {
    let text = serde_json::to_value(P2pInfoDto::stopped(None)).unwrap();
    assert_eq!(text["started"], serde_json::json!(false));
    assert_eq!(text["peerId"], serde_json::Value::Null);
    assert_eq!(text["error"], serde_json::Value::Null);
}

/// A41 核对补测：OrgSyncOverviewDto 两级呈现映射（K=3 组织级合计 +
/// 逐成员 PC 明细）与 camelCase 形状；kApplicable 必须如实透传
/// （前端据它区分「无 K 不提醒」的 all-members 组织）。
#[test]
fn org_sync_overview_dto_maps_two_level_replica_fields() {
    let overview = spark_core::org::OrgSyncOverview {
        org_id: "org_x".into(),
        replica_target: 3,
        synced_peers: 2,
        total_members: 3,
        members: vec![spark_core::org::MemberSyncOverview {
            root_id: "r-a".into(),
            org_user_id: Some("uid-a".into()),
            peer_id: None,
            is_self: true,
            ever_synced: true,
            last_synced_at: None,
        }],
        connected_peers: 1,
        recovery_state: spark_core::p2p::RecoveryState::Idle,
        last_connected_at: None,
        dht_mode: spark_core::p2p::DhtMode::default(),
        status: spark_core::org::OrgNetworkStatus::LocalOnly,
        k_applicable: true,
        member_replicas: vec![
            spark_core::org::MemberReplicaOverview {
                root_id: "r-a".into(),
                org_user_id: Some("uid-a".into()),
                pc_synced: true,
                device_class: "pc",
            },
            spark_core::org::MemberReplicaOverview {
                root_id: "r-b".into(),
                org_user_id: None,
                pc_synced: false,
                device_class: "mobile",
            },
        ],
    };
    let dto = OrgSyncOverviewDto::from(overview);
    assert!(dto.k_applicable);
    assert_eq!(dto.member_replicas.len(), 2);

    let value = serde_json::to_value(&dto).unwrap();
    // 组织级：合计 / 目标两级呈现的第一级
    assert_eq!(value["replicaTarget"], serde_json::json!(3));
    assert_eq!(value["syncedPeers"], serde_json::json!(2));
    assert_eq!(value["kApplicable"], serde_json::json!(true));
    // 第二级：逐成员 PC 明细（手机不计入口径随 deviceClass 如实透传）
    assert_eq!(value["memberReplicas"][0]["pcSynced"], serde_json::json!(true));
    assert_eq!(value["memberReplicas"][0]["deviceClass"], serde_json::json!("pc"));
    assert_eq!(
        value["memberReplicas"][1]["deviceClass"],
        serde_json::json!("mobile")
    );
    // orgUserId 缺省键不出现（双写过渡未发布成员）
    assert!(value["memberReplicas"][1].get("orgUserId").is_none());
    assert_eq!(
        value["memberReplicas"][0]["orgUserId"],
        serde_json::json!("uid-a")
    );
    assert_eq!(value["members"][0]["orgUserId"], serde_json::json!("uid-a"));
    assert_eq!(value["status"], serde_json::json!("localOnly"));
}

/// 无 K 组织（纯 all-members）：kApplicable=false 如实透传，
/// 前端据此不做达标判定、不提醒。
#[test]
fn org_sync_overview_dto_carries_k_not_applicable() {
    let overview = spark_core::org::OrgSyncOverview {
        org_id: "org_y".into(),
        replica_target: 3,
        synced_peers: 1,
        total_members: 2,
        members: vec![],
        connected_peers: 0,
        recovery_state: spark_core::p2p::RecoveryState::Idle,
        last_connected_at: None,
        dht_mode: spark_core::p2p::DhtMode::default(),
        status: spark_core::org::OrgNetworkStatus::LocalOnly,
        k_applicable: false,
        member_replicas: vec![],
    };
    let value = serde_json::to_value(OrgSyncOverviewDto::from(overview)).unwrap();
    assert_eq!(value["kApplicable"], serde_json::json!(false));
    assert_eq!(value["memberReplicas"], serde_json::json!([]));
}

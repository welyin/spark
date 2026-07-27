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

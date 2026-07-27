//! 提取 orgAddressRecord golden vectors（wiki/protocol/org.md §16）。
//!
//! 一次性运行：`cargo run --example extract_org_address_vectors`，把输出的
//! `orgAddressRecord` 节并入 `../spec/vectors/org.json`（删除 meta.pendingGroups
//! 中的 orgAddressRecord 占位）。消费测试：`tests/org_vectors.rs`。
//!
//! 确定性：组织根密钥 seed 固定 [7u8; 32]，nowMs 固定 1720000000000，与
//! org.json 其余节的 NOW 口径一致。

use ed25519_dalek::SigningKey;
use serde_json::json;
use spark_core::org::{
    ORG_ADDRESS_RECORD_DEFAULT_TTL_MS, build_org_address_record_payload, sign_org_address_record,
    verify_org_address_record,
};

fn main() {
    let seed = [7u8; 32];
    let seed_hex: String = seed.iter().map(|b| format!("{b:02x}")).collect();
    let signing_key = SigningKey::from_bytes(&seed);
    let now_ms: i64 = 1_720_000_000_000;
    let org_id = "org_0123456789abcdef";
    let gateways = vec!["aa".repeat(32), "bb".repeat(32)];

    let cases: Vec<_> = [
        ("with-display-name", Some("星火公开组织".to_string())),
        ("without-display-name", None),
    ]
    .into_iter()
    .map(|(name, display_name)| {
        let record = sign_org_address_record(
            &signing_key,
            org_id,
            display_name,
            gateways.clone(),
            1,
            now_ms,
            ORG_ADDRESS_RECORD_DEFAULT_TTL_MS,
        );
        let payload = build_org_address_record_payload(&record.unsigned());
        let ok = verify_org_address_record(&record, now_ms).is_ok();

        // 篡改 seq → 签名不再匹配
        let mut tampered = record.clone();
        tampered.seq += 1;
        let tampered_reason = verify_org_address_record(&tampered, now_ms)
            .reason()
            .unwrap_or("");

        // 越过 publishedAt + ttl → ttl 窗口
        let expired_reason = verify_org_address_record(&record, now_ms + record.ttl + 1)
            .reason()
            .unwrap_or("");

        json!({
            "name": name,
            "record": record,
            "payload": payload,
            "verify": { "ok": ok },
            "verifyTampered": { "ok": false, "reason": tampered_reason },
            "verifyExpired": { "ok": false, "reason": expired_reason },
        })
    })
    .collect();

    let section = json!({
        "seedHex": seed_hex,
        "nowMs": now_ms,
        "cases": cases,
    });
    println!("{}", serde_json::to_string_pretty(&section).unwrap());
}

//! dm_e2e HKDF 密钥派生 golden vector 验收测试：加载 `../spec/vectors/dm_e2e.json`
//! 逐条断言 `hkdf_sha256(shared, info)` 的确定性输出（32B 会话密钥）。
//!
//! AES-256-GCM 密文本身随机（nonce 随机），不做 golden（协议守护已注明）；
//! 密钥派生（HKDF-SHA256，**方向无关**，info 绑 `spark-dm-e2e-v1:{min}:{max}`——
//! from/to 按字典序排序小在前，A→B 与 B→A 派生同一份会话密钥）确定性，
//! 本 vector 锁定派生字节，供跨实现（TS/Rust）对齐。`hkdf_sha256` 是纯函数，
//! 直接消费向量中的已排序 `info`；from/to 排序接线在 `derive_session_key`
//! （实现侧，S3 由执行开发按方向无关修正）由服务单测覆盖。

use spark_core::dm_e2e::hkdf_sha256;

fn vectors() -> Vec<serde_json::Value> {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../spec/vectors/dm_e2e.json");
    let raw = std::fs::read_to_string(path).expect("read dm_e2e vector");
    let parsed: serde_json::Value = serde_json::from_str(&raw).expect("parse dm_e2e vector");
    parsed.as_array().cloned().expect("dm_e2e.json 应为数组")
}

#[test]
fn hkdf_derivation_matches_golden() {
    let vs = vectors();
    assert!(!vs.is_empty(), "dm_e2e.json 不应为空数组");
    for v in &vs {
        let shared: [u8; 32] = hex::decode(v["sharedHex"].as_str().unwrap())
            .unwrap()
            .try_into()
            .unwrap();
        let info = v["info"].as_str().unwrap();
        let out = hkdf_sha256(&shared, info);
        assert_eq!(
            hex::encode(out),
            v["derivedKeyHex"].as_str().unwrap(),
            "HKDF-SHA256 派生应精确匹配 golden（info={info}）"
        );
    }
}

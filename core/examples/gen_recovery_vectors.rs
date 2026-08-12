//! 生成 M5 system/recovery 三条 golden vector（initiated / vetoed / committed）。
//!
//! 仅依赖 core crate 现有导出，不新增第三方依赖。
//! 输出为可直接追加到 code/spec/vectors/dm_envelope.json 的 JSON 数组元素。

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use ed25519_dalek::{Signer as _, SigningKey};
use sha2::Digest as _;

// 复用 core 的 dm_envelope 线形（不 copy 实现，防漂移）。
use spark_core::kernel::dm_envelope::build_signing_payload;

const SECRET_HEX: &str = "0707070707070707070707070707070707070707070707070707070707070707";
const TS: i64 = 1_720_000_000_000;

fn root_id_from_key(key: &SigningKey) -> String {
    let pub_key = key.verifying_key().to_bytes();
    hex::encode(sha2::Sha256::digest(pub_key))
}

fn vector(
    description: &str,
    kind: &str,
    body: serde_json::Value,
) -> String {
    let secret: [u8; 32] = hex::decode(SECRET_HEX).unwrap().try_into().unwrap();
    let key = SigningKey::from_bytes(&secret);
    let root_id = root_id_from_key(&key);
    let pub_key_b64 = B64.encode(key.verifying_key().to_bytes());

    let payload = build_signing_payload(kind, &root_id, &root_id, TS, &body);
    let sig = key.sign(payload.as_bytes());

    let v = serde_json::json!({
        "description": description,
        "secretKeyHex": SECRET_HEX,
        "pubKeyBase64": pub_key_b64,
        "rootId": root_id,
        "kind": kind,
        "to": root_id,
        "ts": TS,
        "body": body,
        "payload": payload,
        "sigBase64": B64.encode(sig.to_bytes()),
    });
    serde_json::to_string_pretty(&v).unwrap()
}

fn main() {
    let request_id = "rc0123456789abcdef0123456789abcdef";
    let op = "reset_password";
    let from_device = "peer-device-123";
    let deadline: i64 = 1_720_000_000_000 + 86_400_000; // ts + 24h

    let initiated = vector(
        "system/recovery 信封：initiated 广播，from==to==rootId，body 携带 kind/op/requestId/deadline/fromDevice",
        "system/recovery",
        serde_json::json!({
            "kind": "initiated",
            "op": op,
            "requestId": request_id,
            "deadline": deadline,
            "fromDevice": from_device,
        }),
    );

    let vetoed = vector(
        "system/recovery 信封：vetoed 广播，from==to==rootId，body 携带 kind/requestId/fromDevice",
        "system/recovery",
        serde_json::json!({
            "kind": "vetoed",
            "requestId": request_id,
            "fromDevice": from_device,
        }),
    );

    let committed = vector(
        "system/recovery 信封：committed 广播，from==to==rootId，body 携带 kind/op/requestId/fromDevice",
        "system/recovery",
        serde_json::json!({
            "kind": "committed",
            "op": op,
            "requestId": request_id,
            "fromDevice": from_device,
        }),
    );

    println!("{},\n{},\n{}", initiated, vetoed, committed);
}

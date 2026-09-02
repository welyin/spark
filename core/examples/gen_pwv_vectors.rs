//! 生成 `code/spec/vectors/pwv.json`（口令校验器 golden vectors）。
//!
//! 这是 pwv/pwack 线形的**事实源生成器**：用与 `core::pw`（E2 实现）相同的
//! scrypt / aes-gcm / hmac / sha2 crate 直接推导，确保字节级可被真实实现复现。
//! 运行：`cargo run --example gen_pwv_vectors -- <repo_root>`
//!
//! 依赖以下已锁定 crate（见 Cargo.toml）：
//!   scrypt 0.12 / aes-gcm 0.11 / hmac 0.13 / sha2 0.11 / base64 0.22
//!
//! 线形推导（字节级权威，见 personal-data-sync.md §13）：
//!   Kverify = scrypt(P2, salt, N=32768, r=8, p=1, len=32)      // N=32768 同身份文件
//!   ct      = AES-256-GCM(Kverify, nonce12, "spark-pwv1")      // 无 AAD
//!   Kack    = sha256(Kverify || "spark-pwack")
//!   mac     = HMAC-SHA256(Kack, "pwack" || peer_b58 || decimal_ascii(vTs))

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use hmac::{Hmac, Mac};
use scrypt::Params;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::PathBuf;

// ---- 固定测试常量（与向量一一对应） ----
const P2: &str = "spark-e1-golden-password";
const P_WRONG: &str = "spark-e1-wrong-password";
const SALT: &[u8; 16] = &[
    0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e, 0x1f,
];
const NONCE: &[u8; 12] = &[
    0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x2b,
];
const CHANGED_AT: u64 = 1_760_000_000_000;
const PEER: &str = "12D3KooWPqT2nMDSiXUSx5D7fasaxhxKigVhcqfkKqrLghCq9jxz";
const PLAINTEXT_TAG: &[u8] = b"spark-pwv1"; // 公开常量明文（8B），无 AAD
const KACK_LABEL: &[u8] = b"spark-pwack";
const MAC_PREFIX: &[u8] = b"pwack";

fn scrypt_kverify(pw: &[u8], salt: &[u8]) -> [u8; 32] {
    let params = Params::new(15, 8, 1).expect("valid scrypt params"); // log_n=15 => N=32768
    let mut key = [0u8; 32];
    scrypt::scrypt(pw, salt, &params, &mut key).expect("scrypt ok");
    key
}

fn aes_gcm_encrypt(key: &[u8; 32], nonce: &[u8; 12], plaintext: &[u8]) -> Vec<u8> {
    let cipher = Aes256Gcm::new_from_slice(key).expect("aes key ok");
    let n = Nonce::from_slice(nonce);
    cipher.encrypt(n, plaintext).expect("aes-gcm ok")
}

fn aes_gcm_decrypt(key: &[u8; 32], nonce: &[u8; 12], ct: &[u8]) -> Option<Vec<u8>> {
    let cipher = Aes256Gcm::new_from_slice(key).expect("aes key ok");
    let n = Nonce::from_slice(nonce);
    cipher.decrypt(n, ct).ok()
}

fn kack(key: &[u8; 32]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(key);
    h.update(KACK_LABEL);
    let out = h.finalize();
    let mut k = [0u8; 32];
    k.copy_from_slice(&out);
    k
}

fn ack_mac(kack: &[u8; 32], peer: &str, vts: u64) -> Vec<u8> {
    let mut mac = Hmac::<Sha256>::new_from_slice(kack).expect("hmac key ok");
    mac.update(MAC_PREFIX);
    mac.update(peer.as_bytes());
    mac.update(vts.to_string().as_bytes());
    mac.finalize().into_bytes().to_vec()
}

fn b64(x: &[u8]) -> String {
    STANDARD.encode(x)
}

fn main() {
    let repo_root: PathBuf = std::env::args()
        .nth(1)
        .expect("usage: gen_pwv_vectors <repo_root>")
        .into();
    let out_path = repo_root.join("code/spec/vectors/pwv.json");

    // ---- 向量 1: build/verify 往返 ----
    let kverify = scrypt_kverify(P2.as_bytes(), SALT);
    let ct = aes_gcm_encrypt(&kverify, NONCE, PLAINTEXT_TAG);
    // verify: 正确口令可还原明文
    let dec = aes_gcm_decrypt(&kverify, NONCE, &ct).expect("P2 verifies");
    assert_eq!(dec, PLAINTEXT_TAG, "vector1: P2 还原明文");
    // 错口令 Kverify' 解密失败
    let kverify_wrong = scrypt_kverify(P_WRONG.as_bytes(), SALT);
    assert!(
        aes_gcm_decrypt(&kverify_wrong, NONCE, &ct).is_none(),
        "vector1: 错口令失败"
    );

    // ---- 向量 2: Kack 派生 + ack MAC ----
    let kack_val = kack(&kverify);
    let mac_val = ack_mac(&kack_val, PEER, CHANGED_AT);
    assert_eq!(mac_val.len(), 32, "vector2: HMAC-SHA256 32B");

    // ---- 向量 3: 伪造 V（错口令）拒用 + last-good ----
    let forged_ct = aes_gcm_encrypt(&kverify_wrong, NONCE, PLAINTEXT_TAG);
    let forged_dec = aes_gcm_decrypt(&kverify, NONCE, &forged_ct);
    assert!(
        forged_dec.is_none(),
        "vector3: 错口令密文用真口令解不开 -> 拒用, last-good 保留"
    );

    // ---- 向量 5: pwv:self serde 字节级（字段序逐字节固定） ----
    let pwv_self = format!(
        "{{\"v\":1,\"kdf\":\"scrypt\",\"salt\":\"{}\",\"nonce\":\"{}\",\"ct\":\"{}\",\"changedAt\":{},\"changedBy\":\"{}\"}}",
        b64(SALT),
        b64(NONCE),
        b64(&ct),
        CHANGED_AT,
        PEER
    );

    // ---- 向量 6: pwack 线形 ----
    let pwack_val = format!(
        "{{\"v\":1,\"vTs\":{},\"mac\":\"{}\"}}",
        CHANGED_AT,
        b64(&mac_val)
    );

    // ---- 向量 10: 伪 ack（错 Kack 派生的错误 MAC）—— verify_and_anchor_ack 不锚定 ----
    let wrong_kack = [0xAAu8; 32]; // 伪造者不知道真 Kverify/Kack，用任意错密钥
    let forged_mac = ack_mac(&wrong_kack, PEER, CHANGED_AT);
    assert_ne!(
        forged_mac, mac_val,
        "vector10: 错 Kack 的 MAC 必须不同于真 MAC"
    );

    // ---- 向量 8/9: epoch:state reason 扩展（password_reset / 未知容错） ----
    // reason:"password_reset" 样例（带 serde(other) 兜底后合法）。
    let state_reset = format!(
        "{{\"current\":3,\"rotatedAt\":{},\"rotatedBy\":\"{}\",\"reason\":\"password_reset\"}}",
        CHANGED_AT + 1,
        PEER
    );
    // 未知 reason：新实现必须 `#[serde(other)] Unknown` 兜底反序列化，不得 fail-closed。
    let state_unknown = format!(
        "{{\"current\":4,\"rotatedAt\":{},\"rotatedBy\":\"{}\",\"reason\":\"some_future_reason\"}}",
        CHANGED_AT + 2,
        PEER
    );

    let vectors = serde_json::json!([
        {
            "id": "pwv_build_verify_roundtrip",
            "desc": "build/verify 往返精确值；正确口令还原明文，错口令失败",
            "input": { "password": P2, "wrongPassword": P_WRONG, "salt": b64(SALT), "nonce": b64(NONCE) },
            "expect": {
                "kverify": b64(&kverify),
                "ct": b64(&ct),
                "plaintext": "spark-pwv1",
                "verifyWithP2": true,
                "verifyWithWrong": false
            }
        },
        {
            "id": "pwv_kack_and_ack_mac",
            "desc": "Kack=sha256(Kverify||spark-pwack)；ack MAC=HMAC-SHA256(Kack, pwack|peer|vTs)",
            "input": { "kverify": b64(&kverify), "peer": PEER, "vTs": CHANGED_AT },
            "expect": { "kack": b64(&kack_val), "mac": b64(&mac_val) }
        },
        {
            "id": "pwv_forged_rejected_last_good",
            "desc": "伪造 V（错口令 ct）用真口令解不开 -> 拒用，last-good 保留",
            "input": { "goodKverify": b64(&kverify), "forgedCt": b64(&forged_ct), "nonce": b64(NONCE) },
            "expect": { "decryptOk": false }
        },
        {
            "id": "pwv_watermark_monotonic",
            "desc": "回放防护：vTs <= appliedVTs 水位时拒应用，水位只增不减",
            "input": { "appliedVTs": CHANGED_AT, "incomingVTs": CHANGED_AT - 5 },
            "expect": { "apply": false, "watermarkStays": CHANGED_AT }
        },
        {
            "id": "pwv_self_serde_bytes",
            "desc": "pwv:self 线形字节级（字段序 v,kdf,salt,nonce,ct,changedAt,changedBy 固定）",
            "input": {},
            "expect": { "json": pwv_self }
        },
        {
            "id": "pwack_wire_bytes",
            "desc": "pwack:{peer} 值线形字节级（v,vTs,mac 固定序）",
            "input": {},
            "expect": { "json": pwack_val }
        },
        {
            "id": "pwv_missing_gate_pass",
            "desc": "无 V 旧版语义：pwv:self 缺失 -> 门控恒过（不可回归项）",
            "input": { "pwvPresent": false, "ackPresent": false },
            "expect": { "gatePasses": true, "nonRegressible": true }
        },
        {
            "id": "epoch_state_reason_password_reset",
            "desc": "epoch:state.reason 扩 password_reset 样例（serde 线形）",
            "input": {},
            "expect": { "json": state_reset }
        },
        {
            "id": "epoch_state_unknown_reason_fallback",
            "desc": "未知 reason 容错：serde(other) Unknown 兜底，不得 fail-closed",
            "input": {},
            "expect": { "json": state_unknown, "parsesAsUnknown": true }
        },
        {
            "id": "pseudo_ack_not_anchored",
            "desc": "伪 ack（错 Kack 派生的错误 MAC）verify_and_anchor_ack 不锚定，锚不变，门控不 Pass（D2 攻击链回归）",
            "input": {
                "kverify": b64(&kverify),
                "forgedAckMac": b64(&forged_mac),
                "ackVTs": CHANGED_AT,
                "initialAnchor": 0,
                "pwvChangedAt": CHANGED_AT
            },
            "expect": {
                "macValid": false,
                "anchored": false,
                "anchorStays": 0,
                "gatePasses": false
            }
        },
        {
            "id": "valid_ack_anchored_gate_pass",
            "desc": "合法 ack（正确 MAC）锚定后锚=ack.vTs；should_gate 读锚 Pass（锚>=pwv.changedAt）",
            "input": {
                "kverify": b64(&kverify),
                "validAckMac": b64(&mac_val),
                "ackVTs": CHANGED_AT,
                "initialAnchor": 0,
                "pwvChangedAt": CHANGED_AT
            },
            "expect": {
                "macValid": true,
                "anchored": true,
                "anchorBecomes": CHANGED_AT,
                "gatePasses": true
            }
        },
        {
            "id": "pwv_future_ts_rejected",
            "desc": "未来 ts 拒收：changedAt > now+10min -> reject，水位不推进（防伪造 V 推死水位 DoS）",
            "input": { "appliedVTs": CHANGED_AT, "incomingVTs": CHANGED_AT + 86_400_000 * 2, "now": CHANGED_AT + 60_000 },
            "expect": { "futureRejected": true, "watermarkStays": CHANGED_AT }
        },
        {
            "id": "pwv_replay_rejected",
            "desc": "回放拒收：changedAt <= appliedVTs -> reject，水位不变",
            "input": { "appliedVTs": CHANGED_AT, "incomingVTs": CHANGED_AT - 5 },
            "expect": { "replayRejected": true, "watermarkStays": CHANGED_AT }
        }
    ]);

    fs::write(
        &out_path,
        serde_json::to_string_pretty(&vectors).unwrap() + "\n",
    )
    .expect("write pwv.json");
    println!("wrote {}", out_path.display());

    // ---- 自检 ----
    let parsed: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&out_path).unwrap()).unwrap();
    assert_eq!(parsed.as_array().unwrap().len(), 13, "13 条向量");
    assert_eq!(parsed[4]["expect"]["json"], pwv_self, "vector5 json 自洽");
    assert_eq!(parsed[5]["expect"]["json"], pwack_val, "vector6 json 自洽");
    assert_eq!(
        parsed[9]["expect"]["gatePasses"], false,
        "vector10 伪 ack 门控不 Pass"
    );
    assert_eq!(
        parsed[10]["expect"]["anchored"], true,
        "vector11 合法 ack 锚定"
    );
    assert_eq!(
        parsed[11]["expect"]["futureRejected"], true,
        "vector12 未来 ts 拒收"
    );
    assert_eq!(
        parsed[12]["expect"]["replayRejected"], true,
        "vector13 回放拒收"
    );
    println!("self-check ok: 13 vectors");
}

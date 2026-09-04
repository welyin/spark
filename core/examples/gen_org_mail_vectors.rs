//! 生成 `code/spec/vectors/org-mail.json`（阶段四E 跨组织网关邮箱 golden
//! vectors，p2p-org-mail §21.8 六组清单）。
//!
//! 全部输入固定（种子/键/id/ts/nonce/网关/组织地址记录），输出逐字节确定
//! ——消费测试 `core/tests/org_mail_vectors.rs` 按本文件验收。
//! 运行：`cargo run --example gen_org_mail_vectors -- <repo_root>/code/spec/vectors/org-mail.json`

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use ed25519_dalek::SigningKey;
use serde_json::json;
use spark_core::identity::derive_domain_identity;
use spark_core::org::mailbox::{
    OrgMailEnvelope, OrgMailFrom, OrgMailTo, domain_id_of, org_mail_domain, orgmail_box_with_nonce,
    orgmail_sign, orgmail_sign_payload,
};
use spark_core::org::mailbox_store::fetch_challenge_sign;
use spark_core::org::org_address::sign_org_address_record;

// ---- 固定测试常量 ----
const SEED_A: [u8; 64] = [0xA1; 64]; // 发送方种子（组织 A）
const SEED_B: [u8; 64] = [0xB2; 64]; // 收件方种子（组织 B）
const ORG_A: &str = "org_aaaaaaaaaaaaaaaa";
const ORG_B: &str = "org_bbbbbbbbbbbbbbbb";
const ORG_KEY_A: [u8; 32] = [0x11; 32]; // 组织 A 根签名私钥（向量固定）
const ORG_KEY_B: [u8; 32] = [0x22; 32]; // 组织 B 根签名私钥
const GATEWAY_B_ROOT: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
const ENV_ID: &str = "0123456789abcdef01234567";
const TS: i64 = 1_720_000_000_000;
const NONCE12: [u8; 12] = [0x21; 12];
const CHAL_NONCE: [u8; 16] = [0x42; 16];
const GATEWAY_PEER: &str = "12D3KooWGatewayPeerIdFixedForVectors000000000000";
const CHAL_TS: i64 = 1_720_000_060_000;
const PLAIN: &str = r#"{"kind":"text","text":"hello 跨组织"}"#;

fn envelope_with(from_org_address: Option<String>) -> OrgMailEnvelope {
    let sender = derive_domain_identity(&SEED_A, &org_mail_domain(ORG_A));
    let recipient = derive_domain_identity(&SEED_B, &org_mail_domain(ORG_B));
    let recipient_domain_id = domain_id_of(&recipient.signing_key.verifying_key());
    let record_b = sign_org_address_record(
        &SigningKey::from_bytes(&ORG_KEY_B),
        ORG_B,
        Some("组织B".to_string()),
        vec![GATEWAY_B_ROOT.to_string()],
        1,
        TS,
        3_600_000,
    );
    let to_org_address = serde_json::to_string(&record_b).unwrap();
    let (nonce, ct) = orgmail_box_with_nonce(
        PLAIN.as_bytes(),
        &sender.signing_key,
        &recipient_domain_id,
        &to_org_address,
        ENV_ID,
        &NONCE12,
    )
    .expect("box ok");
    let mut env = OrgMailEnvelope {
        id: ENV_ID.to_string(),
        to: OrgMailTo {
            org_address: to_org_address,
            domain_id: recipient_domain_id,
        },
        from: OrgMailFrom {
            domain_id: domain_id_of(&sender.signing_key.verifying_key()),
            org_address: from_org_address,
        },
        ts: TS,
        ttl: 604_800_000,
        nonce,
        ct,
        sig: String::new(),
    };
    env.sig = orgmail_sign(&sender.signing_key, &env);
    env
}

fn main() {
    let out = std::env::args().nth(1).expect("usage: gen_org_mail_vectors <out.json>");
    let sender = derive_domain_identity(&SEED_A, &org_mail_domain(ORG_A));
    let recipient = derive_domain_identity(&SEED_B, &org_mail_domain(ORG_B));

    // §21.8-1：信封构造（含 from.orgAddress 形态）
    let env_full = envelope_with(Some(
        serde_json::to_string(&sign_org_address_record(
            &SigningKey::from_bytes(&ORG_KEY_A),
            ORG_A,
            Some("组织A".to_string()),
            vec![],
            1,
            TS,
            3_600_000,
        ))
        .unwrap(),
    ));
    // §21.8-4：签名载荷两形态（含/不含 from.orgAddress）
    let env_lean = envelope_with(None);
    let challenge_payload =
        spark_core::org::mailbox_store::fetch_challenge_payload("挑战nonce占位", GATEWAY_PEER, 0);
    let _ = challenge_payload;

    let chal_nonce_b64 = B64.encode(CHAL_NONCE);
    let vectors = json!({
      "_comment": "阶段四E 跨组织网关邮箱 golden vectors（p2p-org-mail §21.8 六组）。生成器：core/examples/gen_org_mail_vectors.rs（勿手改）。",
      "envelope": {
        "desc": "§21.8-1 信封构造：固定两侧域身份种子/id/ts/ttl/nonce/明文 → 逐字节 ct+sig",
        "input": {
          "seedA": B64.encode(SEED_A), "seedB": B64.encode(SEED_B),
          "orgA": ORG_A, "orgB": ORG_B,
          "orgKeyA": B64.encode(ORG_KEY_A), "orgKeyB": B64.encode(ORG_KEY_B),
          "gatewayBRoot": GATEWAY_B_ROOT,
          "id": ENV_ID, "ts": TS, "ttl": 604_800_000,
          "nonce": B64.encode(NONCE12), "plaintext": PLAIN,
        },
        "expect": serde_json::to_value(&env_full).unwrap(),
      },
      "box": {
        "desc": "§21.8-2 box 往返 + 低阶点（全零共享）拒绝",
        "input": { "seedA": B64.encode(SEED_A), "seedB": B64.encode(SEED_B),
                   "nonce": B64.encode(NONCE12), "plaintext": PLAIN },
        "expect": { "nonce": env_full.nonce, "ct": env_full.ct,
                    "lowOrderRejectDomainId": B64.encode([1u8,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0]) },
      },
      "aadRelocate": {
        "desc": "§21.8-3 AAD 搬迁拒收：ct 搬到不同 id 的信封解密必败",
        "input": { "tamperedId": "1123456789abcdef01234567" },
        "expect": { "unboxOk": false },
      },
      "signPayload": {
        "desc": "§21.8-4 签名载荷两形态（含/不含 from.orgAddress）逐字节 + sig 固定值",
        "expect": {
          "withOrgAddress": orgmail_sign_payload(&env_full),
          "withoutOrgAddress": orgmail_sign_payload(&env_lean),
          "sigWith": env_full.sig,
          "sigWithout": env_lean.sig,
        },
      },
      "challenge": {
        "desc": "§21.8-5 挑战载荷串 + challenge sig 固定值；错 peer/ts/重放必败",
        "input": { "nonce": chal_nonce_b64, "gatewayPeerId": GATEWAY_PEER, "challengeTs": CHAL_TS },
        "expect": {
          "payload": spark_core::org::mailbox_store::fetch_challenge_payload(&chal_nonce_b64, GATEWAY_PEER, CHAL_TS),
          "challenge": fetch_challenge_sign(&recipient.signing_key, &chal_nonce_b64, GATEWAY_PEER, CHAL_TS),
        },
      },
      "quotaTtl": {
        "desc": "§21.8-6 配额/TTL 判定边界表",
        "expect": {
          "ttlClamp": { "input": 2_592_000_001i64, "output": 2_592_000_000i64 },
          "ttlDefault": 604_800_000i64,
          "orgCap": 1000, "orgCapRejectAt": 1000,
          "recipientCap": 100, "recipientCapRejectAt": 100,
        },
      },
    });
    // 自检：生成器产物必须过验签（防生成器与实现漂移）
    assert!(spark_core::org::mailbox::orgmail_verify(&env_full), "self-check verify");
    let plain = spark_core::org::mailbox::orgmail_unbox(&env_full, &recipient.signing_key)
        .expect("self-check unbox");
    assert_eq!(String::from_utf8(plain).unwrap(), PLAIN, "self-check plaintext");
    std::fs::write(&out, serde_json::to_string_pretty(&vectors).unwrap()).expect("write vectors");
    eprintln!("written {out}");
    // 防未用告警（生成器内值都已使用；sender 仅用于上游派生自检）
    let _ = sender;
}

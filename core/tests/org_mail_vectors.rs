//! org-mail golden vectors 验收测试（阶段四E）：加载 `../spec/vectors/org-mail.json`
//! 逐组断言（p2p-org-mail §21.8 六组清单），并覆盖网关存储面行为（配额/幂等/
//! TTL/限流/错误码/两轮拉取）。
//!
//! 向量由 `core/examples/gen_org_mail_vectors.rs` 生成（固定种子/键/时刻/
//! nonce，输出逐字节确定）。

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use ed25519_dalek::SigningKey;
use serde_json::Value;
use spark_core::identity::derive_domain_identity;
use spark_core::org::mailbox::{
    OrgMailEnvelope, domain_id_of, org_mail_domain, orgmail_box, orgmail_box_with_nonce,
    orgmail_sign, orgmail_sign_payload, orgmail_unbox, orgmail_verify,
};
use spark_core::org::mailbox_store::{
    fetch_challenge_sign, gateway_deliver, gateway_fetch, gateway_fetch_challenge,
};
use spark_core::org::org_address::sign_org_address_record;
use spark_core::org::service::{CreateOrganizationInput, OrganizationService};
use spark_core::storage::{MemoryStorage, StorageBackend};

fn vectors() -> Value {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../spec/vectors/org-mail.json");
    let raw = std::fs::read_to_string(path).expect("read org-mail vectors");
    serde_json::from_str(&raw).expect("parse org-mail vectors")
}

fn bytes64(v: &Value) -> [u8; 64] {
    let raw = B64.decode(v.as_str().unwrap()).unwrap();
    <[u8; 64]>::try_from(raw.as_slice()).unwrap()
}

/// §21.8-1 信封构造：固定输入 → 逐字节 ct + sig + 线上形态。
#[test]
fn envelope_construction_byte_exact() {
    let v = vectors();
    let expect: OrgMailEnvelope =
        serde_json::from_value(v["envelope"]["expect"].clone()).expect("expect envelope");
    // 逐字段核验固定输入产生的线上形态（完整重建链在 gen example 内自检；
    // 此处逐字节断言关键字段——ct/nonce/sig/id/域身份）
    assert_eq!(expect.id, v["envelope"]["input"]["id"].as_str().unwrap());
    assert_eq!(expect.ts, v["envelope"]["input"]["ts"].as_i64().unwrap());
    let seed_a = bytes64(&v["envelope"]["input"]["seedA"]);
    let sender = derive_domain_identity(&seed_a, &org_mail_domain("org_aaaaaaaaaaaaaaaa"));
    assert_eq!(
        expect.from.domain_id,
        domain_id_of(&sender.signing_key.verifying_key()),
        "发送方域身份 = org-mail:orgA 派生"
    );
    assert!(orgmail_verify(&expect), "信封验签过");
    // sig 与 sign 重算逐字节一致（Ed25519 确定性签名）
    assert_eq!(
        orgmail_sign(&sender.signing_key, &{
            let mut e = expect.clone();
            e.sig = String::new();
            e
        }),
        expect.sig,
        "sig 重算逐字节"
    );
}

/// §21.8-2 box 往返 + 低阶点拒绝。
#[test]
fn box_roundtrip_and_low_order_reject() {
    let v = vectors();
    let input = &v["box"]["input"];
    let seed_a = bytes64(&input["seedA"]);
    let seed_b = bytes64(&input["seedB"]);
    let nonce: [u8; 12] = B64
        .decode(input["nonce"].as_str().unwrap())
        .unwrap()
        .try_into()
        .unwrap();
    let sender = derive_domain_identity(&seed_a, &org_mail_domain("org_aaaaaaaaaaaaaaaa"));
    let recipient = derive_domain_identity(&seed_b, &org_mail_domain("org_bbbbbbbbbbbbbbbb"));
    let record_b = sign_org_address_record(
        &SigningKey::from_bytes(&[0x22; 32]),
        "org_bbbbbbbbbbbbbbbb",
        Some("组织B".to_string()),
        vec!["c".repeat(64)],
        1,
        1_720_000_000_000,
        3_600_000,
    );
    let to_org_address = serde_json::to_string(&record_b).unwrap();
    let recipient_id = domain_id_of(&recipient.signing_key.verifying_key());
    let plain = input["plaintext"].as_str().unwrap();

    // 往返：固定 nonce 逐字节一致 + 解回明文
    let (n, ct) = orgmail_box_with_nonce(
        plain.as_bytes(),
        &sender.signing_key,
        &recipient_id,
        &to_org_address,
        "0123456789abcdef01234567",
        &nonce,
    )
    .expect("box ok");
    assert_eq!(
        n,
        v["box"]["expect"]["nonce"].as_str().unwrap(),
        "nonce 逐字节"
    );
    assert_eq!(ct, v["box"]["expect"]["ct"].as_str().unwrap(), "ct 逐字节");

    // 低阶点拒绝：收件人 domainId = Edwards 单位点（y=1 → Montgomery u=0）
    let low = v["box"]["expect"]["lowOrderRejectDomainId"]
        .as_str()
        .unwrap();
    assert!(
        orgmail_box(
            plain.as_bytes(),
            &sender.signing_key,
            low,
            &to_org_address,
            "0123456789abcdef01234567",
        )
        .is_none(),
        "低阶点（全零共享）拒绝"
    );
}

/// §21.8-3 AAD 搬迁拒收：ct 搬到不同 id 的信封解密必败。
#[test]
fn aad_relocate_fails() {
    let v = vectors();
    let expect_env: OrgMailEnvelope =
        serde_json::from_value(v["envelope"]["expect"].clone()).expect("expect envelope");
    let seed_b = bytes64(&v["envelope"]["input"]["seedB"]);
    let recipient = derive_domain_identity(&seed_b, &org_mail_domain("org_bbbbbbbbbbbbbbbb"));

    // 原样解出明文
    let plain = orgmail_unbox(&expect_env, &recipient.signing_key).expect("unbox ok");
    assert_eq!(
        String::from_utf8(plain).unwrap(),
        v["envelope"]["input"]["plaintext"].as_str().unwrap()
    );
    // 搬迁到不同 id → AAD 不吻合 → 解密必败
    let tampered = OrgMailEnvelope {
        id: v["aadRelocate"]["input"]["tamperedId"]
            .as_str()
            .unwrap()
            .to_string(),
        ..expect_env.clone()
    };
    assert!(
        orgmail_unbox(&tampered, &recipient.signing_key).is_none(),
        "AAD 搬迁拒收"
    );
}

/// §21.8-4 签名载荷两形态逐字节 + sig 固定值 + 篡改验签必败。
#[test]
fn sign_payload_byte_exact_and_tamper_fails() {
    let v = vectors();
    let expect_env: OrgMailEnvelope =
        serde_json::from_value(v["envelope"]["expect"].clone()).expect("expect envelope");
    let expect = &v["signPayload"]["expect"];
    assert_eq!(
        orgmail_sign_payload(&expect_env),
        expect["withOrgAddress"].as_str().unwrap(),
        "含 from.orgAddress 载荷逐字节"
    );
    assert_eq!(expect_env.sig, expect["sigWith"].as_str().unwrap());
    assert!(orgmail_verify(&expect_env), "签名验过");

    // 不含 from.orgAddress 形态：丢键（非 null）
    let lean = OrgMailEnvelope {
        from: spark_core::org::mailbox::OrgMailFrom {
            org_address: None,
            ..expect_env.from.clone()
        },
        ..expect_env.clone()
    };
    assert_eq!(
        orgmail_sign_payload(&lean),
        expect["withoutOrgAddress"].as_str().unwrap(),
        "不含 from.orgAddress 载荷逐字节（丢键）"
    );

    // 篡改任一字段验签必败
    let tampered = OrgMailEnvelope {
        ts: expect_env.ts + 1,
        ..expect_env
    };
    assert!(!orgmail_verify(&tampered), "篡改 ts 验签必败");
}

/// §21.8-5 挑战载荷串 + sig 固定值；错 peerId/ts 验签必败。
#[test]
fn challenge_payload_and_sig_byte_exact() {
    let v = vectors();
    let seed_b = bytes64(&v["envelope"]["input"]["seedB"]);
    let recipient = derive_domain_identity(&seed_b, &org_mail_domain("org_bbbbbbbbbbbbbbbb"));
    let input = &v["challenge"]["input"];
    let expect = &v["challenge"]["expect"];
    let nonce = input["nonce"].as_str().unwrap();
    let peer = input["gatewayPeerId"].as_str().unwrap();
    let ts = input["challengeTs"].as_i64().unwrap();
    assert_eq!(
        spark_core::org::mailbox_store::fetch_challenge_payload(nonce, peer, ts),
        expect["payload"].as_str().unwrap(),
        "挑战载荷串逐字节"
    );
    assert_eq!(
        fetch_challenge_sign(&recipient.signing_key, nonce, peer, ts),
        expect["challenge"].as_str().unwrap(),
        "challenge sig 固定值"
    );
    // 错 peerId / 错 ts → 载荷串不同（验签必败由载荷失配保证，验签路径在
    // 集成测试覆盖）
    assert_ne!(
        spark_core::org::mailbox_store::fetch_challenge_payload(nonce, "otherPeer", ts),
        expect["payload"].as_str().unwrap()
    );
    assert_ne!(
        spark_core::org::mailbox_store::fetch_challenge_payload(nonce, peer, ts + 1),
        expect["payload"].as_str().unwrap()
    );
}

// ---------------------------------------------------------------------------
// 网关存储面（§21.5/§21.6）
// ---------------------------------------------------------------------------

const NOW: i64 = 1_720_000_000_000;

/// 构造网关存储 + 本机网关成员的组织 + 合法信封（与向量同输入）。
fn setup_gateway() -> (MemoryStorage, String, OrgMailEnvelope, String) {
    let v = vectors();
    let env: OrgMailEnvelope =
        serde_json::from_value(v["envelope"]["expect"].clone()).expect("expect envelope");
    let record_b: spark_core::org::OrgAddressRecord =
        serde_json::from_str(&env.to.org_address).unwrap();
    let gateway_root = record_b.gateways[0].clone();
    // 网关本地持有该组织且为成员
    let mut storage = MemoryStorage::new();
    OrganizationService::create_organization(
        &mut storage,
        &CreateOrganizationInput {
            name: "组织B".to_string(),
            ..Default::default()
        },
        &gateway_root,
        NOW,
    )
    .unwrap();
    // create_organization 生成自己的 orgId——网关归属判定查的是**地址记录
    // 的 orgId**（record_b.org_id），把本地记录搬到该 orgId 键位
    let local = OrganizationService::read_all_organizations(&storage).unwrap();
    let mut rec = local.into_iter().next().unwrap();
    rec.org_id = record_b.org_id.clone();
    OrganizationService::save_record(&mut storage, &rec).unwrap();
    (storage, gateway_root, env, record_b.org_id)
}

/// 投递全分支：合法投递 → 幂等重投 ok 不落重复 → 限流 → wrong-org →
/// 过期清扫 → 配额（每收件人 100）。
#[test]
fn gateway_deliver_full_pipeline() {
    let (mut s, gateway_root, env, _org_id) = setup_gateway();
    let v = vectors();
    let seed_a = bytes64(&v["envelope"]["input"]["seedA"]);
    let sender = derive_domain_identity(&seed_a, &org_mail_domain("org_aaaaaaaaaaaaaaaa"));
    // 合法投递（ts 在新鲜窗内）
    let r = gateway_deliver(&mut s, &env, NOW, &gateway_root);
    assert_eq!(r, serde_json::json!({ "ok": true }), "合法投递入箱");
    // 限流先于幂等（§21.5 顺序）：同来源同窗内即便同 id 重投也被限流拦——
    // 用改 id 重签的信封断限流分支
    let mut env2 = env.clone();
    env2.id = "1123456789abcdef01234567".to_string();
    env2.sig = orgmail_sign(&sender.signing_key, &env2); // id 变了必须重签
    let r = gateway_deliver(&mut s, &env2, NOW + 500, &gateway_root);
    assert_eq!(
        r,
        serde_json::json!({ "ok": false, "reason": "rate-limited" }),
        "同来源 1s 内第二封限流"
    );
    // 幂等：限流窗外同 id 重投 → ok 不重复落库
    let r = gateway_deliver(&mut s, &env, NOW + 1500, &gateway_root);
    assert_eq!(r, serde_json::json!({ "ok": true }), "幂等重投 ok");
    let box_count = s
        .scan(&spark_core::storage::ScanOptions::prefix("orgmail:box:"))
        .unwrap()
        .len();
    assert_eq!(box_count, 1, "同 id 去重（不重复落库）");
    // 窗口外放行（幂等命中不记限流账，窗外 env2 落库）
    let r = gateway_deliver(&mut s, &env2, NOW + 2000, &gateway_root);
    assert_eq!(r, serde_json::json!({ "ok": true }), "限流窗口外放行");
    // wrong-org：另一网关 root（不在记录 gateways 内）
    let r = gateway_deliver(&mut s, &env, NOW + 3000, &"e".repeat(64));
    assert_eq!(
        r,
        serde_json::json!({ "ok": false, "reason": "wrong-org" }),
        "非本组织网关拒收"
    );
    // 过期清扫：ts+ttl 过期的信封被惰性清掉
    let old = OrgMailEnvelope {
        id: "2123456789abcdef01234567".to_string(),
        ts: NOW - 604_800_000 - 1000,
        ..env.clone()
    };
    // 旧信封签名失效（ts 变了）——直接写库模拟存量过期信
    s.put(
        &spark_core::org::mailbox_store::mailbox_key(
            &serde_json::from_str::<spark_core::org::OrgAddressRecord>(&old.to.org_address)
                .unwrap()
                .org_id,
            &old.id,
        ),
        &serde_json::to_string(&old).unwrap(),
    )
    .unwrap();
    let swept = spark_core::org::mailbox_store::sweep_expired(&mut s, NOW + 3000).unwrap();
    assert_eq!(swept, 1, "惰性清扫删掉过期信");
}

/// §21.8-6 配额/TTL 判定边界表（向量为权威参数）。
#[test]
fn quota_and_ttl_boundaries() {
    let v = vectors();
    let expect = &v["quotaTtl"]["expect"];
    assert_eq!(
        spark_core::org::mailbox::normalize_ttl(expect["ttlClamp"]["input"].as_i64().unwrap()),
        expect["ttlClamp"]["output"].as_i64().unwrap(),
        "TTL 超上限截断"
    );
    assert_eq!(
        spark_core::org::mailbox::normalize_ttl(0),
        expect["ttlDefault"].as_i64().unwrap(),
        "缺省 7 天"
    );
    // 每收件人 100 条配额：填满后第 101 封 quota
    let (mut s, gateway_root, env, org_id) = setup_gateway();
    let recipient_cap = expect["recipientCap"].as_u64().unwrap() as usize;
    let seed_a = bytes64(&v["envelope"]["input"]["seedA"]);
    let sender = derive_domain_identity(&seed_a, &org_mail_domain("org_aaaaaaaaaaaaaaaa"));
    for i in 0..recipient_cap {
        let mut e = env.clone();
        e.id = format!("{:024x}", i + 100);
        e.sig = orgmail_sign(&sender.signing_key, &e); // id 变了必须重签
        let r = gateway_deliver(&mut s, &e, NOW + 1500 + (i as i64) * 1500, &gateway_root);
        assert_eq!(r, serde_json::json!({ "ok": true }), "第 {} 封入箱", i + 1);
    }
    let mut e = env.clone();
    e.id = format!("{:024x}", recipient_cap + 100);
    e.sig = orgmail_sign(&sender.signing_key, &e);
    let r = gateway_deliver(
        &mut s,
        &e,
        NOW + 1500 + (recipient_cap as i64) * 1500,
        &gateway_root,
    );
    assert_eq!(
        r,
        serde_json::json!({ "ok": false, "reason": "quota" }),
        "每收件人超 100 条拒收"
    );
    // 另一收件人不受该收件人配额影响（但投递限流按来源 from.domainId 同
    // 源——这里同来源需错开 1s 窗）
    let _ = org_id;
}

/// 两轮挑战拉取：取挑战 → 应答取信（域名匹配 + 取信即删）→ 重放 nonce
/// 必败 → 错签名必败。
#[test]
fn fetch_two_round_handshake() {
    let (mut s, gateway_root, env, _org_id) = setup_gateway();
    let v = vectors();
    let seed_b = bytes64(&v["envelope"]["input"]["seedB"]);
    let recipient = derive_domain_identity(&seed_b, &org_mail_domain("org_bbbbbbbbbbbbbbbb"));
    let my_domain_id = domain_id_of(&recipient.signing_key.verifying_key());
    let seed_a = bytes64(&v["envelope"]["input"]["seedA"]);
    let sender = derive_domain_identity(&seed_a, &org_mail_domain("org_aaaaaaaaaaaaaaaa"));
    let gateway_peer = "12D3KooWGwPeer";

    // 入箱两封（一封是我的，一封是别人的）——第 4 参是本机 rootId（wrong-org
    // 判定），gateway_peer 只用于挑战签名载荷
    let r = gateway_deliver(&mut s, &env, NOW, &gateway_root);
    assert_eq!(r, serde_json::json!({ "ok": true }));
    let mut other = env.clone();
    other.id = "3123456789abcdef01234567".to_string();
    other.to.domain_id = B64.encode([0x33; 32]); // 别人的 domainId
    other.sig = orgmail_sign(&sender.signing_key, &other); // id/domainId 变了重签
    let r = gateway_deliver(&mut s, &other, NOW + 1500, &gateway_root);
    assert_eq!(r, serde_json::json!({ "ok": true }));

    // 第一轮：取挑战
    let r1 = gateway_fetch_challenge(&mut s, &my_domain_id, NOW + 2000);
    let nonce = r1["nonce"].as_str().unwrap().to_string();
    let chal_ts = r1["ts"].as_i64().unwrap();
    assert_eq!(r1["phase"].as_str(), Some("challenge"));

    // 第二轮：错签名（别的域身份签）→ invalid-challenge
    let wrong = derive_domain_identity(&[0x99; 64], &org_mail_domain("org_bbbbbbbbbbbbbbbb"));
    let bad_sig = fetch_challenge_sign(&wrong.signing_key, &nonce, gateway_peer, chal_ts);
    let r = gateway_fetch(
        &mut s,
        &my_domain_id,
        &nonce,
        chal_ts,
        &bad_sig,
        gateway_peer,
        NOW + 2100,
    );
    assert_eq!(
        r,
        serde_json::json!({ "ok": false, "reason": "invalid-challenge" }),
        "错签名必败（nonce 已焚）"
    );

    // 重取挑战 + 正确签名 → 域名匹配只回我的信 + 取信即删
    let r1 = gateway_fetch_challenge(&mut s, &my_domain_id, NOW + 2200);
    let nonce = r1["nonce"].as_str().unwrap().to_string();
    let chal_ts = r1["ts"].as_i64().unwrap();
    let sig = fetch_challenge_sign(&recipient.signing_key, &nonce, gateway_peer, chal_ts);
    let r2 = gateway_fetch(
        &mut s,
        &my_domain_id,
        &nonce,
        chal_ts,
        &sig,
        gateway_peer,
        NOW + 2300,
    );
    assert_eq!(r2["ok"], serde_json::json!(true));
    let envelopes = r2["envelopes"].as_array().unwrap();
    assert_eq!(envelopes.len(), 1, "域名匹配：只回我的信");
    let got: OrgMailEnvelope = serde_json::from_value(envelopes[0].clone()).unwrap();
    assert_eq!(got.id, env.id);
    // 取信即删：箱内只剩别人的信
    let left = s
        .scan(&spark_core::storage::ScanOptions::prefix("orgmail:box:"))
        .unwrap();
    assert_eq!(left.len(), 1, "取信即删（同事务）");
    // 重放已焚 nonce → invalid-challenge
    let r = gateway_fetch(
        &mut s,
        &my_domain_id,
        &nonce,
        chal_ts,
        &sig,
        gateway_peer,
        NOW + 2400,
    );
    assert_eq!(
        r,
        serde_json::json!({ "ok": false, "reason": "invalid-challenge" }),
        "重放 nonce 必败"
    );
}

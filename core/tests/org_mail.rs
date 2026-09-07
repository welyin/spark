//! org-mail 直连协议端到端集成测试（阶段四E，p2p-org-mail §21）：三真实
//! libp2p 节点 loopback——发送方 S 直连网关 G 投递（deliver）→ G 落箱 →
//! 收件人 R 两轮挑战拉取（fetch，nonce 一次性 + 载荷绑网关 peerId）→
//! 取信即删 → R 侧域身份解箱回明文。
//!
//! 网关 G 的宿主走 kernel 层真实入站分发（TestHost.handle_org_mail →
//! handle_org_mail_inbound），存储用共享夹具从外部检查落箱/删除。

mod common;

use std::time::Duration;

use ed25519_dalek::SigningKey;
use serde_json::json;
use spark_core::identity::derive_domain_identity;
use spark_core::org::mailbox::{
    OrgMailEnvelope, OrgMailFrom, OrgMailTo, domain_id_of, new_envelope_id, normalize_ttl,
    org_mail_domain, orgmail_box, orgmail_sign, orgmail_unbox,
};
use spark_core::org::mailbox_store::fetch_challenge_sign;
use spark_core::org::org_address::sign_org_address_record;
use spark_core::org::service::{CreateOrganizationInput, OrganizationService};
use spark_core::p2p::P2pEvent;
use spark_core::p2p::peer_targets::PeerNodeInfo;
use spark_core::storage::{ScanOptions, StorageBackend};

use common::p2p::*;

/// 网关入站走 `system_now_ms()`（真实时钟）——地址记录/信封 ts 必须用真实
/// 当前时刻，否则新鲜窗/有效期判定直接拒收。
fn real_now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock")
        .as_millis() as i64
}

/// deliver → 落箱 → 两轮 fetch → 取信即删 → 解箱明文一致 + nonce 重放必败。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn org_mail_deliver_fetch_roundtrip() {
    let now = 1_720_000_000_000i64; // 节点 now_fn（连接/事件层）
    let gateway_root = "cc".repeat(32);
    let sender_root = "aa".repeat(32);
    let recipient_root = "bb".repeat(32);
    let org_b = "org_bbbbbbbbbbbbbbbb";

    // 目标组织地址记录（自认证，gateways 含网关 rootId；ts 用真实时钟）
    let org_key = SigningKey::from_bytes(&[0x42; 32]);
    let record_b = sign_org_address_record(
        &org_key,
        org_b,
        Some("组织B".to_string()),
        vec![gateway_root.clone()],
        1,
        real_now_ms(),
        3_600_000,
    );
    let record_json = serde_json::to_string(&record_b).expect("record json");

    // 网关节点 G：本地持有该组织且为成员（wrong-org 归属判定的正侧）
    let (mut g, state_g, s_g) = start_node(now, Some(&gateway_root)).await;
    {
        let mut guard = s_g.0.lock().unwrap();
        OrganizationService::create_organization(
            &mut *guard,
            &CreateOrganizationInput {
                name: "组织B".to_string(),
                ..Default::default()
            },
            &gateway_root,
            real_now_ms(),
        )
        .expect("create org on gateway");
        // create_organization 生成自己的 orgId——归属判定查地址记录的 orgId，
        // 把本地记录搬到该键位（同 org_mail_vectors setup_gateway）
        let local = OrganizationService::read_all_organizations(&*guard).unwrap();
        let mut rec = local.into_iter().next().unwrap();
        rec.org_id = org_b.to_string();
        OrganizationService::save_record(&mut *guard, &rec).unwrap();
    }
    // 挑战验签绑定网关真实 peerId：回填宿主
    state_g.lock().unwrap().my_peer_id = Some(g.peer_id().to_string());

    // 发送方 S / 收件人 R
    let (mut s, _state_s, _s_s) = start_node(now, Some(&sender_root)).await;
    let (mut r, _state_r, _s_r) = start_node(now, Some(&recipient_root)).await;
    let addrs_g = started_addresses(&mut g).await;
    let _ = started_addresses(&mut s).await;
    let _ = started_addresses(&mut r).await;
    connect(&s, g.peer_id(), &dialable(&addrs_g)).await;
    connect(&r, g.peer_id(), &dialable(&addrs_g)).await;
    wait_for(&mut g, Duration::from_secs(10), |e| {
        matches!(e, P2pEvent::PeerConnected { .. })
    })
    .await;

    let addrs_g = dialable(&addrs_g);
    let target_g = || PeerNodeInfo {
        peer_id: Some(g.peer_id().to_string()),
        addresses: addrs_g.clone(), // org 直连尝试构建要求显式地址（无 dm 式已连接短路）
    };

    // ---- 发送方：构造签名信封（org-mail:{orgA} 域身份）→ deliver ----
    let seed_a = [0x11; 64];
    let seed_b = [0x22; 64];
    let sender = derive_domain_identity(&seed_a, &org_mail_domain("org_aaaaaaaaaaaaaaaa"));
    let recipient = derive_domain_identity(&seed_b, &org_mail_domain(org_b));
    let recipient_domain_id = domain_id_of(&recipient.signing_key.verifying_key());
    let plain = json!({"text": "hello org-mail"}).to_string();
    let id = new_envelope_id();
    let (nonce, ct) = orgmail_box(
        plain.as_bytes(),
        &sender.signing_key,
        &recipient_domain_id,
        &record_json,
        &id,
    )
    .expect("box ok");
    let mut envelope = OrgMailEnvelope {
        id,
        to: OrgMailTo {
            org_address: record_json,
            domain_id: recipient_domain_id.clone(),
        },
        from: OrgMailFrom {
            domain_id: domain_id_of(&sender.signing_key.verifying_key()),
            org_address: None,
        },
        ts: real_now_ms(),
        ttl: normalize_ttl(0),
        nonce,
        ct,
        sig: String::new(),
    };
    envelope.sig = orgmail_sign(&sender.signing_key, &envelope);

    let resp = s
        .org_mail_request(
            &target_g(),
            &json!({ "op": "deliver", "envelope": envelope }).to_string(),
        )
        .await
        .expect("deliver request ok")
        .expect("deliver response");
    assert_eq!(resp, json!({ "ok": true }), "网关收信入箱");

    // G 侧落箱一条（键 = orgmail:box:{orgId}:{id}）
    {
        let guard = s_g.0.lock().unwrap();
        let entries = guard
            .scan(&ScanOptions::prefix("orgmail:box:"))
            .expect("scan box");
        assert_eq!(entries.len(), 1, "网关箱内一封信");
        assert!(entries[0].0.ends_with(&envelope.id));
    }

    // ---- 收件人 R：两轮挑战拉取 ----
    // 第一轮：取挑战 nonce
    let resp1 = r
        .org_mail_request(
            &target_g(),
            &json!({ "op": "fetch", "recipientDomainId": recipient_domain_id }).to_string(),
        )
        .await
        .expect("fetch round1 ok")
        .expect("fetch round1 response");
    assert_eq!(resp1["ok"], json!(true));
    assert_eq!(resp1["phase"], json!("challenge"));
    let chal_nonce = resp1["nonce"].as_str().unwrap().to_string();
    let chal_ts = resp1["ts"].as_i64().unwrap();

    // 第二轮：挑战应答（载荷绑 nonce + 网关 peerId + ts）
    let challenge = fetch_challenge_sign(&recipient.signing_key, &chal_nonce, g.peer_id(), chal_ts);
    let resp2 = r
        .org_mail_request(
            &target_g(),
            &json!({
                "op": "fetch",
                "recipientDomainId": recipient_domain_id,
                "nonce": chal_nonce,
                "challengeTs": chal_ts,
                "challenge": challenge,
            })
            .to_string(),
        )
        .await
        .expect("fetch round2 ok")
        .expect("fetch round2 response");
    assert_eq!(resp2["ok"], json!(true), "挑战验签过、域名匹配回信");
    let envelopes = resp2["envelopes"].as_array().unwrap();
    assert_eq!(envelopes.len(), 1);
    let got: OrgMailEnvelope = serde_json::from_value(envelopes[0].clone()).unwrap();
    assert_eq!(got.id, envelope.id);

    // 取信即删：G 箱已空
    {
        let guard = s_g.0.lock().unwrap();
        let left = guard
            .scan(&ScanOptions::prefix("orgmail:box:"))
            .expect("scan box");
        assert_eq!(left.len(), 0, "取信即删（响应与删除同事务）");
    }

    // R 侧域身份解箱回明文（端到端机密性闭环）
    let decrypted = orgmail_unbox(&got, &recipient.signing_key).expect("unbox ok");
    assert_eq!(String::from_utf8(decrypted).unwrap(), plain);

    // nonce 重放必败（用后即焚）
    let resp3 = r
        .org_mail_request(
            &target_g(),
            &json!({
                "op": "fetch",
                "recipientDomainId": recipient_domain_id,
                "nonce": chal_nonce,
                "challengeTs": chal_ts,
                "challenge": challenge,
            })
            .to_string(),
        )
        .await
        .expect("fetch replay ok")
        .expect("fetch replay response");
    assert_eq!(
        resp3,
        json!({ "ok": false, "reason": "invalid-challenge" }),
        "重放已焚 nonce 必败"
    );

    s.stop().await;
    r.stop().await;
    g.stop().await;
}

/// 非本组织网关（地址记录 gateways 不含本机 rootId）deliver → wrong-org。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn org_mail_deliver_wrong_org_rejected() {
    let now = 1_720_000_000_000i64;
    let gateway_root = "cc".repeat(32); // 记录里的网关
    let stranger_root = "dd".repeat(32); // 实际投递目标（不是记录网关）
    let org_b = "org_bbbbbbbbbbbbbbbb";

    let org_key = SigningKey::from_bytes(&[0x42; 32]);
    let record_b = sign_org_address_record(
        &org_key,
        org_b,
        Some("组织B".to_string()),
        vec![gateway_root.clone()],
        1,
        real_now_ms(),
        3_600_000,
    );

    // 投递目标是一个 rootId 不在记录 gateways 内的节点
    let (mut g, _state_g, s_g) = start_node(now, Some(&stranger_root)).await;
    let (mut s, _state_s, _s_s) = start_node(now, None).await;
    let addrs_g = started_addresses(&mut g).await;
    let _ = started_addresses(&mut s).await;
    connect(&s, g.peer_id(), &dialable(&addrs_g)).await;
    wait_for(&mut g, Duration::from_secs(10), |e| {
        matches!(e, P2pEvent::PeerConnected { .. })
    })
    .await;

    let sender = derive_domain_identity(&[0x11; 64], &org_mail_domain("org_aaaaaaaaaaaaaaaa"));
    let recipient = derive_domain_identity(&[0x22; 64], &org_mail_domain(org_b));
    let recipient_domain_id = domain_id_of(&recipient.signing_key.verifying_key());
    let record_json = serde_json::to_string(&record_b).expect("record json");
    let id = new_envelope_id();
    let (nonce, ct) = orgmail_box(
        b"hi",
        &sender.signing_key,
        &recipient_domain_id,
        &record_json,
        &id,
    )
    .expect("box ok");
    let mut envelope = OrgMailEnvelope {
        id,
        to: OrgMailTo {
            org_address: record_json,
            domain_id: recipient_domain_id,
        },
        from: OrgMailFrom {
            domain_id: domain_id_of(&sender.signing_key.verifying_key()),
            org_address: None,
        },
        ts: real_now_ms(),
        ttl: normalize_ttl(0),
        nonce,
        ct,
        sig: String::new(),
    };
    envelope.sig = orgmail_sign(&sender.signing_key, &envelope);

    let resp = s
        .org_mail_request(
            &PeerNodeInfo {
                peer_id: Some(g.peer_id().to_string()),
                addresses: dialable(&addrs_g),
            },
            &json!({ "op": "deliver", "envelope": envelope }).to_string(),
        )
        .await
        .expect("deliver request ok")
        .expect("deliver response");
    assert_eq!(
        resp,
        json!({ "ok": false, "reason": "wrong-org" }),
        "非记录网关拒收"
    );
    // 箱内无落信
    let guard = s_g.0.lock().unwrap();
    assert!(
        guard
            .scan(&ScanOptions::prefix("orgmail:box:"))
            .unwrap()
            .is_empty()
    );

    s.stop().await;
    g.stop().await;
}

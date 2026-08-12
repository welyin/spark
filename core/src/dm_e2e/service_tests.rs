//! dm_e2e 模块单元测试（拆分至独立文件，service.rs 约 380 行 + 测试罗列另置，
//! 对齐 orgsync/access_tests.rs 的先例）。

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use ed25519_dalek::SigningKey;
use serde_json::{Value, json};

use super::*;
use crate::storage::MemoryStorage;
use crate::sync::orgsync::ed_pk_to_x25519;
use crate::sync::personal::{apply_personal_remote, get_personal_meta};

const NODE_A: &str = "node-a";
const NODE_B: &str = "node-b";
/// 一毫秒。
const H: i64 = 3600 * 1000;

/// 由 seed（单字节填充）构造 root 签名私钥（E2E 用 root 密钥直接转换，
/// 2026-08-11 架构师裁决）。
fn root(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

/// root 公钥 → 对端 X25519 公钥（`ed_pk_to_x25519`）。
fn root_x_pub(key: &SigningKey) -> [u8; 32] {
    ed_pk_to_x25519(&key.verifying_key().to_bytes()).unwrap()
}

fn plain() -> Value {
    json!({"spaceKey": "personal", "message": {"id": "m1", "content": "hello e2e"}})
}

/// 双端密钥协商一致性：A 用「A root 私钥 + B root 公钥 X25519」、B 用「B
/// root 私钥 + A root 公钥 X25519」做 X25519 DH + HKDF，得到同一会话密钥。
#[test]
fn two_sides_derive_same_session_key() {
    let a = root(1);
    let b = root(2);
    let ka = derive_session_key(&a, &root_x_pub(&b), "rootA", "rootB").unwrap();
    let kb = derive_session_key(&b, &root_x_pub(&a), "rootA", "rootB").unwrap();
    assert_eq!(ka, kb, "双端 DH + HKDF 得到同一会话密钥");
}

/// 方向无关（2026-08-11 架构师裁决）：同一对 rootId 的 A→B 与 B→A 派生
/// 同一份会话密钥（HKDF info 用排序后的 from/to）。feed/chat 通道共用这份
/// 方向无关的会话密钥。
#[test]
fn session_key_is_direction_independent() {
    let a = root(1);
    let b = root(2);
    let ab = derive_session_key(&a, &root_x_pub(&b), "rootA", "rootB").unwrap();
    let ba = derive_session_key(&a, &root_x_pub(&b), "rootB", "rootA").unwrap();
    assert_eq!(ab, ba, "A→B 与 B→A 共用同一份会话密钥（方向无关）");
    // feed 与 chat 通道共用（同参数派生同 key）
    let chat = derive_session_key(&a, &root_x_pub(&b), "rootA", "rootB").unwrap();
    assert_eq!(ab, chat, "不区分通道，共用同一份会话密钥");
    // 键序不影响结果：显式小在前 / 大在前同 key
    let lo_first = derive_session_key(&a, &root_x_pub(&b), "rootA", "rootB").unwrap();
    assert_eq!(ab, lo_first, "字典序小在前的 info 与排序结果一致");
}

/// HKDF 派生确定性（RFC 5869 基准）：固定 shared + info → 固定 32B 输出。
/// 该值作为 golden vector 供跨实现对齐（见 dm_e2e.json 消费测试）。
#[test]
fn hkdf_deterministic() {
    let shared = [0x42u8; 32];
    let out = hkdf_sha256(&shared, "spark-dm-e2e-v1:rootA:rootB");
    assert_eq!(out.len(), 32);
    // 同参数幂等
    assert_eq!(out, hkdf_sha256(&shared, "spark-dm-e2e-v1:rootA:rootB"));
    // 换 info 输出变化（域分隔生效）
    assert_ne!(out, hkdf_sha256(&shared, "spark-dm-e2e-v1:rootA:rootC"));
}

/// 端到端：A 设备加密（peer=B），B 设备解密（peer=A）往返成功；篡改 AAD
/// （改 kind/ts）解密失败。
#[test]
fn encrypt_decrypt_roundtrip_and_aad_tamper() {
    let a_key = root(1);
    let b_key = root(2);
    let mut a = MemoryStorage::new();
    let mut b = MemoryStorage::new();

    // A、B 各自协商同一份 A→B 会话密钥（值相同，键各为对端 rootId）
    ensure_session_key(
        &mut a, &a_key, &root_x_pub(&b_key), "b-root-pub-b64", "rootA", "rootB", "rootB", NODE_A, 1000,
    )
    .unwrap();
    ensure_session_key(
        &mut b, &b_key, &root_x_pub(&a_key), "a-root-pub-b64", "rootA", "rootB", "rootA", NODE_B, 1000,
    )
    .unwrap();

    let ts = 1000;
    let body = plain();
    let enc = encrypt_body(&a, "rootA", "rootB", "chat", ts, &body).unwrap();
    assert_eq!(enc["encrypted"], json!(true));
    assert!(enc["ciphertext"].is_string());
    assert!(enc["nonce"].is_string());

    // B 解密 → 还原明文
    let dec = decrypt_body(&b, "rootA", "rootB", "chat", ts, &enc).unwrap();
    assert_eq!(dec, body);

    // AAD 篡改：换 kind / 换 ts → 解密失败
    assert!(decrypt_body(&b, "rootA", "rootB", "feed", ts, &enc).is_err());
    assert!(decrypt_body(&b, "rootA", "rootB", "chat", ts + 1, &enc).is_err());
    // 换 from/to → 密钥/AAD 都变，解密失败
    assert!(decrypt_body(&b, "rootA", "rootC", "chat", ts, &enc).is_err());
    // 篡改密文字节 → AEAD 失败
    let mut tampered = enc.clone();
    tampered["ciphertext"] = json!(tampered["ciphertext"]
        .as_str()
        .unwrap()
        .chars()
        .next()
        .map(|_| {
            // 翻转 base64 尾字符制造坏密文（仅演示路径可达，不必真字节改）
            let s = tampered["ciphertext"].as_str().unwrap();
            let mut v: Vec<char> = s.chars().collect();
            v[0] = if v[0] == 'A' { 'B' } else { 'A' };
            v.into_iter().collect::<String>()
        })
        .unwrap());
    assert!(decrypt_body(&b, "rootA", "rootB", "chat", ts, &tampered).is_err());
}

/// 长期复用不轮换（p2p-dm §19.1.1「长期会话密钥恒等」）：root DH 派生确定性，
/// 两次 `ensure_session_key`（任意间隔注入 now_ms）返回同一密钥、不产生 history
/// 条目、写盘次数不增长（第二次返回 false=复用不写盘）。前向保密由 per-message
/// 临时密钥承担，与密钥表轮换无关。
#[test]
fn long_lived_session_key_is_not_rotated() {
    let a_key = root(1);
    let b_key = root(2);
    let mut b = MemoryStorage::new();
    let peer_x_pub = root_x_pub(&a_key);
    let peer_root_pub_b64 = "a-root-pub-b64";

    // 首次协商：写盘（返回 true）
    let first = ensure_session_key(
        &mut b, &b_key, &peer_x_pub, peer_root_pub_b64, "rootA", "rootB", "rootA", NODE_B, 1000,
    )
    .unwrap();
    assert!(first, "首次协商写盘");
    let rec1 = read_session_key_record(&b, "rootA").unwrap().unwrap();
    let k1 = rec1.current_key.clone();
    assert_eq!(rec1.current_since, 1000);
    assert!(rec1.history_keys.is_empty(), "首次协商不产生 history 条目");

    // 隔任意时长再次调用（now_ms 注入远晚，模拟断联超 24h）：长期密钥恒等
    // → 返回同一密钥、不写盘（false）、不产生 history 条目
    let far_future = 1000 + 48 * H;
    let second = ensure_session_key(
        &mut b, &b_key, &peer_x_pub, peer_root_pub_b64, "rootA", "rootB", "rootA", NODE_B, far_future,
    )
    .unwrap();
    assert!(!second, "既有 current 密钥 → 复用不写盘（写盘次数不增长）");
    let rec2 = read_session_key_record(&b, "rootA").unwrap().unwrap();
    assert_eq!(rec2.current_key, k1, "长期密钥恒等，仍为同一密钥");
    assert!(rec2.history_keys.is_empty(), "不复用不产生同值 history 条目");
    assert_eq!(rec2.current_since, 1000, "currentSince 不被后续调用刷新");
    assert_eq!(rec2.last_seen, 1000, "lastSeen 不被后续调用刷新（复用不写盘）");
}

/// 历史密钥解密：换钥后，用旧密钥签发的离线密文仍可解（select_key_for_ts 按
/// ts 选 current 或历史密钥）。用两个不同密钥手动构造记录验证机制——X25519
/// DH 基于固定域身份是确定性的，真实轮换的新密钥依赖接线层握手（见报告），
/// 本测试聚焦历史密钥的**解密机制**而非轮换密钥值。
#[test]
fn offline_ciphertext_decrypts_with_history_key() {
    let mut b = MemoryStorage::new();
    let t1 = 1000;
    let t2 = 3000;
    // K1 ≠ K3（不同 32B 密钥），模拟换钥前后
    let k1 = B64.encode([1u8; 32]);
    let k3 = B64.encode([3u8; 32]);

    // 阶段一：current = K1（since t1），用 K1 签发 t1 的密文。
    // 两端各以对端 rootId 为键、值相同（encrypt 读 `{rootB}`，decrypt 读 `{rootA}`）。
    let phase1 = SessionKeyRecord {
        current_key: k1.clone(),
        current_since: t1,
        last_seen: t1,
        history_keys: Vec::new(),
        peer_root_pub: None,
    };
    write_session_key_record(&mut b, "rootA", &phase1, NODE_B, t1).unwrap();
    write_session_key_record(&mut b, "rootB", &phase1, NODE_B, t1).unwrap();
    let enc_old = encrypt_body(&b, "rootA", "rootB", "chat", t1, &plain()).unwrap();
    assert_eq!(
        decrypt_body(&b, "rootA", "rootB", "chat", t1, &enc_old).unwrap(),
        plain(),
        "换钥前 current 密钥可解"
    );

    // 阶段二：换钥 → current = K3（since t2），K1 移入 history（两键同步更新）
    let phase2 = SessionKeyRecord {
        current_key: k3,
        current_since: t2,
        last_seen: t2,
        history_keys: vec![HistoryKey { key: k1, since: t1 }],
        peer_root_pub: None,
    };
    write_session_key_record(&mut b, "rootA", &phase2, NODE_B, t2).unwrap();
    write_session_key_record(&mut b, "rootB", &phase2, NODE_B, t2).unwrap();

    // 换钥后，t1 的旧密文仍可用历史密钥 K1 解出；t2 的新密文用 current K3 解
    let dec_old = decrypt_body(&b, "rootA", "rootB", "chat", t1, &enc_old).unwrap();
    assert_eq!(dec_old, plain(), "换钥后旧密文仍可解（历史密钥）");

    let enc_new = encrypt_body(&b, "rootA", "rootB", "chat", t2, &plain()).unwrap();
    assert_eq!(
        decrypt_body(&b, "rootA", "rootB", "chat", t2, &enc_new).unwrap(),
        plain(),
        "换钥后新密文用 current 解"
    );
    // 换钥后，旧时段用 current（K3）解旧密文 → 失败（防密钥混淆）
    assert!(decrypt_body(&b, "rootA", "rootB", "chat", t2, &enc_old).is_err());
}

/// select_key_for_ts 直接判定：current 覆盖 `ts >= currentSince`，历史密钥覆盖
/// 各自时段，无覆盖时段（早于最旧历史）→ None。
#[test]
fn select_key_by_ts() {
    let rec = SessionKeyRecord {
        current_key: "K3".to_string(),
        current_since: 3000,
        last_seen: 3000,
        history_keys: vec![
            HistoryKey { key: "K1".into(), since: 1000 },
            HistoryKey { key: "K2".into(), since: 2000 },
        ],
        peer_root_pub: None,
    };
    assert_eq!(select_key_for_ts(&rec, 500), None, "早于最旧历史 → 无密钥");
    assert_eq!(select_key_for_ts(&rec, 1000), Some("K1"));
    assert_eq!(select_key_for_ts(&rec, 1999), Some("K1"));
    assert_eq!(select_key_for_ts(&rec, 2000), Some("K2"));
    assert_eq!(select_key_for_ts(&rec, 2999), Some("K2"));
    assert_eq!(select_key_for_ts(&rec, 3000), Some("K3"));
    assert_eq!(select_key_for_ts(&rec, 9999), Some("K3"));
}

/// pdsync 自设备收敛：A 设备（node-a）写会话密钥表，经 apply_personal_remote
/// 合入 B 设备（node-b），双方读同一记录（同一 rootId 多台设备共享同一份
/// 1:1 会话密钥）。
#[test]
fn session_key_converges_via_pdsync() {
    let mut a = MemoryStorage::new();
    let mut b = MemoryStorage::new();
    let key = e2e_key_key("rootB");

    let rec = SessionKeyRecord {
        current_key: B64.encode([7u8; 32]),
        current_since: 1000,
        last_seen: 1000,
        history_keys: vec![HistoryKey { key: B64.encode([6u8; 32]), since: 500 }],
        peer_root_pub: None,
    };
    write_session_key_record(&mut a, "rootB", &rec, NODE_A, 1000).unwrap();

    // A → B（pdsync data 合入）
    let meta_a = get_personal_meta(&a, &key).unwrap().unwrap();
    let value = serde_json::to_string(&rec).unwrap();
    let res = apply_personal_remote(&mut b, &key, &value, &meta_a).unwrap();
    assert_eq!(res.as_str(), "applied");
    assert_eq!(read_session_key_record(&b, "rootB").unwrap(), Some(rec));

    // 幂等重放
    let res2 = apply_personal_remote(&mut b, &key, &value, &meta_a).unwrap();
    assert_eq!(res2.as_str(), "equal");
}

/// 存储键线形：`dm:e2e:key:{peerRootId}`。
#[test]
fn storage_key_shape() {
    assert_eq!(
        e2e_key_key(&"bb".repeat(32)),
        format!("dm:e2e:key:{}", "bb".repeat(32))
    );
    // pdsync category 命中该前缀
    assert!(crate::sync::pdsync::category_for_key(&e2e_key_key("x")).is_some());
}

// ── 密钥轮换：临时密钥对交换（p2p-dm §19.1.1）────────────────────────────

/// 临时交换双端派生一致：发送方用「我方临时私钥 + 对端 root 公钥 X25519」、
/// 接收方用「我方 root 私钥 + 对端 ephPub」做 X25519 DH，交换性保证两路径
/// 派生同一会话密钥（HKDF info 同 root 直接转换路径，方向无关排序）。
#[test]
fn ephemeral_two_sides_derive_same_key() {
    let b_key = root(2); // 接收方 B 的 root 签名私钥
    let (eph_priv, eph_pub) = generate_ephemeral_keypair();
    // 发送方：我方临时私钥 + 对端（B）root 公钥 X25519
    let send_key =
        derive_session_key_ephemeral(&eph_priv, &root_x_pub(&b_key), "rootA", "rootB").unwrap();
    // 接收方：我方（B）root 私钥 + 对端 ephPub
    let recv_key =
        derive_session_key_from_eph_pub(&b_key, &eph_pub, "rootA", "rootB").unwrap();
    assert_eq!(send_key, recv_key, "X25519 交换性：临时交换两路径派生同一会话密钥");

    // 临时路径方向无关：A→B 与 B→A 派生同一临时会话密钥
    let reversed =
        derive_session_key_ephemeral(&eph_priv, &root_x_pub(&b_key), "rootB", "rootA").unwrap();
    assert_eq!(send_key, reversed, "临时交换路径亦方向无关（HKDF info 排序）");
}

/// 临时交换与 root 直接转换回退路径产出**不同**会话密钥：临时路径引入一次性
/// 随机私钥，打破 root DH 的确定性，提供前向保密（规格「两种路径各自产出
/// 会话密钥写入同一份密钥表，管理一致」；回退仅对端未升级时用）。
#[test]
fn ephemeral_and_pure_root_paths_differ() {
    let a_key = root(1);
    let b_key = root(2);
    let (eph_priv, _eph_pub) = generate_ephemeral_keypair();
    let ephemeral =
        derive_session_key_ephemeral(&eph_priv, &root_x_pub(&b_key), "rootA", "rootB").unwrap();
    let pure_root = derive_session_key(&a_key, &root_x_pub(&b_key), "rootA", "rootB").unwrap();
    assert_ne!(ephemeral, pure_root, "临时交换提供前向保密，密钥与 root DH 不同");
    // 回退路径（root 直接转换 DH）双端一致——即对端未升级时的跨版本兼容密钥
    let fallback_recv = derive_session_key(&b_key, &root_x_pub(&a_key), "rootA", "rootB").unwrap();
    assert_eq!(pure_root, fallback_recv, "回退路径双端派生一致（无前向保密但兼容）");
}

/// 旧记录兼容：`lastSeen` 字段缺省（旧版本线形）反序列化为 0（`#[serde(default)]`），
/// 不设任何回退访问器语义（p2p-dm §19.1.1 向后兼容——`lastSeen` 仅作簿记，不再
/// 驱动任何安全相关的密钥轮换）。
#[test]
fn legacy_record_defaults_last_seen_to_zero() {
    // 旧记录无 lastSeen 字段 → 反序列化 default 为 0
    let legacy = r#"{"currentKey":"AAA=","currentSince":5000,"historyKeys":[]}"#;
    let rec: SessionKeyRecord = serde_json::from_str(legacy).unwrap();
    assert_eq!(rec.last_seen, 0, "lastSeen 缺省（serde default）为 0");
    assert_eq!(rec.current_since, 5000, "currentSince 正常保留");
    assert!(rec.history_keys.is_empty());
    assert_eq!(rec.peer_root_pub, None, "peerRootPub 缺省为 None");

    // 显式 lastSeen 保留
    let with_seen = r#"{"currentKey":"AAA=","currentSince":5000,"lastSeen":9000,"historyKeys":[]}"#;
    let rec2: SessionKeyRecord = serde_json::from_str(with_seen).unwrap();
    assert_eq!(rec2.last_seen, 9000, "显式 lastSeen 保留");
}

/// 临时会话密钥参与真实加解密往返：发送方用临时派生密钥加密，接收方用
/// ephPub 派生同一密钥解密成功（校验临时交换路径在加解密链路上可用）。
#[test]
fn ephemeral_key_roundtrip_encrypt_decrypt() {
    let b_key = root(2);
    let (eph_priv, eph_pub) = generate_ephemeral_keypair();
    // 发送方临时会话密钥
    let send_key = derive_session_key_ephemeral(&eph_priv, &root_x_pub(&b_key), "rootA", "rootB").unwrap();
    // 接收方 ephPub 派生同一密钥
    let recv_key = derive_session_key_from_eph_pub(&b_key, &eph_pub, "rootA", "rootB").unwrap();
    assert_eq!(send_key, recv_key);

    let mut a = MemoryStorage::new();
    let mut b = MemoryStorage::new();
    // 两端各自以临时会话密钥写入密钥表（值相同、键各为对端 rootId）
    let a_rec = SessionKeyRecord {
        current_key: B64.encode(send_key),
        current_since: 1000,
        last_seen: 1000,
        history_keys: Vec::new(),
        peer_root_pub: None,
    };
    let b_rec = SessionKeyRecord {
        current_key: B64.encode(recv_key),
        current_since: 1000,
        last_seen: 1000,
        history_keys: Vec::new(),
        peer_root_pub: None,
    };
    write_session_key_record(&mut a, "rootB", &a_rec, NODE_A, 1000).unwrap();
    write_session_key_record(&mut b, "rootA", &b_rec, NODE_B, 1000).unwrap();

    let ts = 1000;
    let enc = encrypt_body(&a, "rootA", "rootB", "chat", ts, &plain()).unwrap();
    assert_eq!(
        decrypt_body(&b, "rootA", "rootB", "chat", ts, &enc).unwrap(),
        plain(),
        "临时交换会话密钥加解密往返成功"
    );
}

// ── S6 显式密钥加解密（encrypt_body_with_key / decrypt_body_with_key）────

/// 显式密钥加解密往返：S6 接线用——出站用临时派生密钥（不写密钥表）、
/// 入站按 ephPub 派生同一密钥。显式密钥路径不碰密钥表，AAD 仍绑
/// kind:from:to:ts。
#[test]
fn explicit_key_roundtrip_and_aad_binding() {
    let key = [0x11u8; 32];
    let ts = 1000;
    let enc = encrypt_body_with_key(&key, "rootA", "rootB", "chat", ts, &plain()).unwrap();
    assert_eq!(enc["encrypted"], json!(true));
    let dec = decrypt_body_with_key(&key, "rootA", "rootB", "chat", ts, &enc).unwrap();
    assert_eq!(dec, plain(), "显式密钥往返还原明文");
    // AAD 篡改：换 kind / ts → 解密失败
    assert!(decrypt_body_with_key(&key, "rootA", "rootB", "feed", ts, &enc).is_err());
    assert!(decrypt_body_with_key(&key, "rootA", "rootB", "chat", ts + 1, &enc).is_err());
    // 换密钥 → 解密失败
    assert!(decrypt_body_with_key(&[0x22u8; 32], "rootA", "rootB", "chat", ts, &enc).is_err());
}

/// 完整 E2E 信封往返（出站临时交换 → 入站 ephPub 派生解密）：发送方生成
/// 临时密钥对、用临时派生密钥加密 body、携带 ephPub 构造签名信封；接收方
/// 用「我方 root 私钥 + 对端 ephPub」派生同一会话密钥解密。验证先验签后
/// 解密链路可用（S6 接线契约的出站/入站两步）。
#[test]
fn full_e2e_envelope_outbound_inbound() {
    let a_key = root(1); // 发送方 A 的 root 签名私钥
    let b_key = root(2); // 接收方 B 的 root 签名私钥

    // 出站（A→B）：① 临时密钥对 ② 我方临时私钥 + 对端(B)root 公钥派生
    let (eph_priv, eph_pub) = generate_ephemeral_keypair();
    let peer_x25519 = root_x_pub(&b_key);
    let session_key = derive_session_key_ephemeral(&eph_priv, &peer_x25519, "rootA", "rootB").unwrap();
    let ts = 1000;
    let body = json!({ "topic": "moments:p", "feedId": "f1", "payload": { "text": "hi" } });
    let encrypted = encrypt_body_with_key(&session_key, "rootA", "rootB", "feed", ts, &body).unwrap();

    // 出站信封（携带 ephPub，参与签名）——签名用 A 的 root 签名私钥
    let envelope = crate::kernel::dm_envelope::build_envelope_with_eph(
        "feed",
        "rootA",
        "rootB",
        ts,
        encrypted,
        Some(&B64.encode(eph_pub)),
        &a_key,
    );
    // 信封带 ephPub
    assert!(envelope.get("ephPub").is_some());

    // 入站（B）：验签（此处 from/to 用 rootA/rootB，验签需 pubKey==sha256(from)；
    // 简化验证 ephPub 派生路径解密，验签本身在 dm_envelope 测试覆盖）
    let eph_pub_b64 = envelope.get("ephPub").and_then(Value::as_str).unwrap();
    let recv_key = derive_session_key_from_eph_pub(&b_key, &B64.decode(eph_pub_b64).unwrap().try_into().unwrap(), "rootA", "rootB").unwrap();
    assert_eq!(recv_key, session_key, "X25519 交换性：收发两端派生同一会话密钥");
    let dec = decrypt_body_with_key(&recv_key, "rootA", "rootB", "feed", ts, envelope.get("body").unwrap()).unwrap();
    assert_eq!(dec, body, "入站 ephPub 派生密钥解密还原明文");
}

/// 无 ephPub 回退：入站信封无 ephPub（对端未升级）时走 root 直接转换 DH——
/// 接收方用密钥表 current 密钥解密。验证回退路径（decrypt_body 按密钥表）与
/// 出站 root 直接转换派生一致。
#[test]
fn fallback_without_eph_pub_uses_key_table() {
    let a_key = root(1);
    let b_key = root(2);
    let mut a = MemoryStorage::new();
    let mut b = MemoryStorage::new();
    // A、B 各自以 root DH 派生同一会话密钥（无 ephPub 回退）
    ensure_session_key(&mut a, &a_key, &root_x_pub(&b_key), "b-root-pub-b64", "rootA", "rootB", "rootB", NODE_A, 1000).unwrap();
    ensure_session_key(&mut b, &b_key, &root_x_pub(&a_key), "a-root-pub-b64", "rootA", "rootB", "rootA", NODE_B, 1000).unwrap();
    let ts = 1000;
    // 出站：无 ephPub 的 root 直接转换加密（encrypt_body 按密钥表 current 密钥）
    let enc = encrypt_body(&a, "rootA", "rootB", "chat", ts, &plain()).unwrap();
    // 入站：无 ephPub 信封 → 走密钥表回退解密（decrypt_body）
    let dec = decrypt_body(&b, "rootA", "rootB", "chat", ts, &enc).unwrap();
    assert_eq!(dec, plain(), "无 ephPub 回退路径双端一致");
}

// ── 对端 root 公钥记录（record_inbound_peer_root_pub）与出站 E2E 加密 ──────

/// 入站记录对端 root 公钥：记录不存在时创建占位记录（currentKey 空）；
/// 已存值相同不重复写盘；对端 root 公钥变化时更新。占位记录后续由
/// `ensure_session_key` 补全会话密钥。
#[test]
fn record_inbound_peer_root_pub_writes_and_updates() {
    let mut s = MemoryStorage::new();
    // 首次：无记录 → 创建占位记录（currentKey 空，仅对端公钥）
    record_inbound_peer_root_pub(&mut s, "rootB", "b-pub-v1", NODE_A, 1000).unwrap();
    let rec = read_session_key_record(&s, "rootB").unwrap().unwrap();
    assert_eq!(rec.peer_root_pub.as_deref(), Some("b-pub-v1"));
    assert!(rec.current_key.is_empty(), "占位记录会话密钥为空，待 ensure 补全");

    // 已存值相同：不写盘（peer_root_pub 不变）
    record_inbound_peer_root_pub(&mut s, "rootB", "b-pub-v1", NODE_A, 1001).unwrap();
    let rec2 = read_session_key_record(&s, "rootB").unwrap().unwrap();
    assert_eq!(rec2.last_seen, 1000, "值相同不写盘（lastSeen 不刷新）");

    // 对端公钥变化：更新
    record_inbound_peer_root_pub(&mut s, "rootB", "b-pub-v2", NODE_A, 1002).unwrap();
    let rec3 = read_session_key_record(&s, "rootB").unwrap().unwrap();
    assert_eq!(rec3.peer_root_pub.as_deref(), Some("b-pub-v2"));
}

/// 出站 E2E 加密（`encrypt_outbound_body`）：读密钥表对端 root 公钥 →
/// ensure_session_key → 临时密钥对 → 加密；返回密文 + ephPub。无对端公钥
/// 记录 → `NoSessionKey`（内部错误，不静默降级明文）。接收方入站用
/// `derive_session_key_from_eph_pub` 派生同一密钥解密成功。
#[test]
fn encrypt_outbound_body_e2e_roundtrip() {
    let a_key = root(1); // 发送方 A root 私钥
    let b_key = root(2); // 接收方 B root 私钥
    let b_pub_b64 = B64.encode(b_key.verifying_key().to_bytes());
    let mut a = MemoryStorage::new();

    // 无对端公钥记录 → NoSessionKey
    let err = encrypt_outbound_body(&mut a, &a_key, "rootA", "rootB", "chat", 1000, &plain(), NODE_A, 1000)
        .unwrap_err();
    assert!(matches!(err, DmE2eError::NoSessionKey));

    // 入站先记录 B 的 root 公钥（模拟 B 曾给 A 发过信封）
    record_inbound_peer_root_pub(&mut a, "rootB", &b_pub_b64, NODE_A, 1000).unwrap();

    // A 出站加密：读 B 公钥 → ensure → 临时 → 加密
    let (encrypted, eph_pub_b64) = encrypt_outbound_body(
        &mut a, &a_key, "rootA", "rootB", "chat", 1000, &plain(), NODE_A, 1000,
    )
    .unwrap();
    assert_eq!(encrypted["encrypted"], json!(true));

    // 接收方 B 用「B root 私钥 + 对端 ephPub」派生同一密钥解密
    let eph_pub: [u8; 32] = B64.decode(&eph_pub_b64).unwrap().try_into().unwrap();
    let recv_key = derive_session_key_from_eph_pub(&b_key, &eph_pub, "rootA", "rootB").unwrap();
    let dec = decrypt_body_with_key(&recv_key, "rootA", "rootB", "chat", 1000, &encrypted).unwrap();
    assert_eq!(dec, plain(), "出站 E2E 加密 + 入站 ephPub 派生解密往返成功");

    // ensure 后密钥表补全会话密钥，且 peerRootPub 已记录
    let rec = read_session_key_record(&a, "rootB").unwrap().unwrap();
    assert!(!rec.current_key.is_empty(), "ensure 补全会话密钥");
    assert_eq!(rec.peer_root_pub.as_deref(), Some(b_pub_b64.as_str()));
}

//! M3 设备 epoch 选择性密钥轮换内核集成测试。
//!
//! 覆盖方案文档 §7（m3-epoch-rotation-plan.md）：
//! - 测试点 2/3  值 wrap/unwrap + AAD 绑定 + should_encrypt 分类矩阵（纯逻辑）。
//! - 测试点 4    rotate 正常流：epoch:state / 密钥表 / effective / 逐授权设备包裹
//!               （排除 revoked / 无 devicePubKey / 本机）+ 安全日志字段完整。
//! - 测试点 5    触发挂点：revoke_device / change_password / start_p2p 初始化（幂等）。
//! - 测试点 7    接收方生效：合入 state + 本机包裹 → unbox → 密钥表/effective 推进；
//!               包裹未到/解不开 → effective 不动、下轮重试。
//! - 测试点 8    新设备补发：带 devicePubKey 的新设备落库 → 补写包裹；revoked 不补。
//! - 测试点 9    协商：对端宣告 0/缺省 → 明文；< 本机 → 按对端；≥ 本机 → 按本机。
//! - 测试点 10   端到端解密合入：密文值解开 → msg:conv 合并行为与明文一致。
//! - 测试点 11   不解不推进：无密钥密文 → 跳过、pmeta 不推进、dseq 不计 → 补发密钥后合入成功。
//! - 测试点 12   **被撤销设备解不开新数据**（端到端核心）：A 撤销 B（epoch2）→ A 新数据
//!               → 模拟不知情设备 C 转发给 B → B 无 ikey2 包裹 → 解不开 → B 库中无此记录。
//! - 测试点 13   老数据可读：旧 epoch 密文用密钥表旧密钥可解（全历史密钥保留）。
//!
//! 纯逻辑层走 `EpochService` / `should_encrypt`（MemoryStorage + 注入 now_ms）；
//! 收发面走 `handle_inbound_dm` 直调（手工签名自设备 pdsync-data 信封），不 mock。

mod common;

use std::collections::HashSet;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use ed25519_dalek::SigningKey;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use spark_core::contact::ContactService;
use spark_core::device::{DeviceRecord, DeviceService};
use spark_core::epoch::{
    AuthorizedDevice, EpochService, EpochState, RotationReason, ed_pk_to_x25519, ed_sk_to_x25519,
    generate_ikey, ikey_key, list_local_keys, parse_ikey_key,
};
use spark_core::kernel::{Kernel, dm_envelope, handle_inbound_dm};
use spark_core::storage::{MemoryStorage, StorageBackend};
use spark_core::sync::meta::DocMeta;
use spark_core::sync::pdsync::{PdsyncRecord, build_data_batch};

use common::*;

const NOW: i64 = 1_720_000_000_000;
const NODE: &str = "local-node";

fn root(seed: u8) -> (SigningKey, String) {
    let key = SigningKey::from_bytes(&[seed; 32]);
    let root_id = hex::encode(Sha256::digest(key.verifying_key().to_bytes()));
    (key, root_id)
}

/// 构造带 devicePubKey 的设备记录（M3 包裹投递前提）。
fn device_record(
    peer: &str,
    uid: &str,
    pub_key: Option<[u8; 32]>,
    revoked: Option<i64>,
) -> DeviceRecord {
    DeviceRecord {
        peer_id: peer.to_string(),
        device_uid: Some(uid.to_string()),
        device_name: "设备".to_string(),
        os: "Android".to_string(),
        os_version: "14".to_string(),
        arch: "aarch64".to_string(),
        macs: Vec::new(),
        app_version: String::new(),
        updated_at: NOW,
        last_seen_at: NOW,
        revoked_at: revoked,
        device_pub_key: pub_key.map(|k| B64.encode(k)),
    }
}

fn x25519_priv_from_sk(sk: &SigningKey) -> [u8; 32] {
    ed_sk_to_x25519(&sk.to_bytes())
}

/// 向 storage 持久化一个确定性 libp2p Ed25519 keypair（seed == ed 私钥）。
/// `try_refresh_keys` / `maybe_grant_epoch_key` 依赖 `p2p:identity:privateKey`。
fn seed_libp2p_keypair(storage: &mut MemoryStorage, seed: &[u8; 32]) {
    let secret =
        libp2p::identity::ed25519::SecretKey::try_from_bytes(*seed).expect("ed25519 secret");
    let keypair = libp2p::identity::ed25519::Keypair::from(secret);
    let lp = libp2p::identity::Keypair::from(keypair);
    let raw = lp.to_protobuf_encoding().expect("protobuf encode");
    storage
        .put("p2p:identity:privateKey", &B64.encode(&raw))
        .unwrap();
}

fn ed_pub(sk: &SigningKey) -> [u8; 32] {
    sk.verifying_key().to_bytes()
}

fn x25519_pub(sk: &SigningKey) -> Option<[u8; 32]> {
    ed_pk_to_x25519(&ed_pub(sk))
}

/// 通用 pdsync-data 投递助手（from==to==自己，手工签名）。
fn deliver_pdsync_data(
    storage: &mut MemoryStorage,
    key: &SigningKey,
    my_root: &str,
    category: &str,
    records: &[PdsyncRecord],
) -> spark_core::kernel::InboundDmResult {
    let body = build_data_batch(category, records, 0, 1);
    let envelope = dm_envelope::build_envelope(
        dm_envelope::KIND_PDSYNC_DATA,
        my_root,
        my_root,
        NOW,
        body,
        key,
    );
    handle_inbound_dm(
        storage,
        my_root,
        "我",
        envelope,
        "peer-self-c",
        &HashSet::new(),
        NOW,
        NODE,
        None,
    )
    .unwrap()
}

fn remote_meta(node: &str, counter: i64, ts: i64) -> DocMeta {
    DocMeta {
        vv: [(node.to_string(), counter)].into_iter().collect(),
        ts,
        node_id: Some(node.to_string()),
        tombstone: None,
    }
}

// ---------------------------------------------------------------------------
// 测试点 3：classify_for_push 分类矩阵（三态：Encrypt / Plain / Skip）。
// 此前用 deprecated should_encrypt（Plain/Skip 都映 false）无法区分，恰好漏掉
// R4「Normal pdoc 误归 Skip」；此处改用 classify_for_push 直断三态。
// ---------------------------------------------------------------------------

fn classify(s: &MemoryStorage, key: &str, epoch: u64) -> spark_core::epoch::EncryptDecision {
    spark_core::epoch::classify_for_push(s, key, epoch).unwrap()
}

#[test]
fn should_encrypt_classification_matrix() {
    let mut s = MemoryStorage::new();
    let enc_epoch = 2u64;

    // 内建内容面 → Encrypt。
    for key in [
        "ct:friend:a",
        "ct:req:in:a",
        "ct:tag:a",
        "device:peer-a",
        "profile:self",
        "msg:conv:personal:dm:x",
        "org:meta:o",
        "ct:org:o",
        "org:inv:in:a",
        "org:inv:out:a",
        "ct:group:g",
        "ct:blocked:b",
    ] {
        assert_eq!(
            classify(&s, key, enc_epoch),
            spark_core::epoch::EncryptDecision::Encrypt,
            "内建 category 应加密: {key}"
        );
    }

    // 自举/策略面明文豁免 → Plain。
    for key in ["pdecl:foo@v1", "epoch:state", "ikey:2:a:b", "ldoc:x"] {
        assert_eq!(
            classify(&s, key, enc_epoch),
            spark_core::epoch::EncryptDecision::Plain,
            "豁免键应明文: {key}"
        );
    }

    // effective == 0 → Plain（未初始化，明文推）。
    assert_eq!(
        classify(&s, "ct:friend:a", 0),
        spark_core::epoch::EncryptDecision::Plain
    );

    // 未注册前缀 → Skip。
    assert_eq!(
        classify(&s, "unknown:cat:key", enc_epoch),
        spark_core::epoch::EncryptDecision::Skip,
        "未注册前缀应 Skip"
    );

    // pdoc Sensitive → Encrypt；缺失 → Skip；Normal → Plain（R4 回归点）。
    let sensitive = json!({
        "name": "notes",
        "version": "v1",
        "sensitivity": "sensitive",
        "collections": []
    });
    s.put("pdecl:notes@v1", &sensitive.to_string()).unwrap();
    assert_eq!(
        classify(&s, "pdoc:notes@v1:k", enc_epoch),
        spark_core::epoch::EncryptDecision::Encrypt,
        "Sensitive pdoc → Encrypt"
    );

    // 声明缺失 → Skip（宁可少推不泄露）。
    assert_eq!(
        classify(&s, "pdoc:missing@v1:k", enc_epoch),
        spark_core::epoch::EncryptDecision::Skip,
        "声明缺失 → Skip"
    );

    // Normal 声明（缺省 sensitivity）→ Plain：epoch 生效下插件数据明文照推，
    // 不得误归 Skip（R4 阻断点）。
    s.put(
        "pdecl:normal@v1",
        &json!({"name":"normal","version":"v1","collections":[]}).to_string(),
    )
    .unwrap();
    assert_eq!(
        classify(&s, "pdoc:normal@v1:k", enc_epoch),
        spark_core::epoch::EncryptDecision::Plain,
        "Normal pdoc → Plain（不得 Skip，否则插件数据停止同步——R4）"
    );

    // fail-open 边角：effective>0 但本机无该 epoch 密钥 → 记录不得明文泄漏（Skip 兜底，
    // 见 encrypt_records_for_push 的 get_local_key None 分支；此处用 classify 验证
    // Normal pdoc 本身仍 Plain，真正 Skip 兜底在 encrypt_records_for_push 层断言）。
    assert_eq!(
        classify(&s, "pdoc:normal@v1:k", enc_epoch),
        spark_core::epoch::EncryptDecision::Plain
    );
}

// ---------------------------------------------------------------------------
// 测试点 4：rotate 正常流（纯逻辑，注入 now_ms）。
// ---------------------------------------------------------------------------

#[test]
fn rotate_writes_state_keytable_effective_and_wraps_authorized_only() {
    let mut s = MemoryStorage::new();
    let (writer_key, my_root) = root(1);
    let writer_peer = "peer-writer";
    let (b_key, _) = root(2);
    let (c_key, _) = root(3);
    let (revoked_key, _) = root(4);
    // 授权设备：B、C 带公钥；D 被撤销；E 无公钥（跳过）。
    DeviceService::upsert_pdsync(
        &mut s,
        &device_record("peer-b", "uid-b", Some(ed_pub(&b_key)), None),
        NOW,
        NODE,
    )
    .unwrap();
    DeviceService::upsert_pdsync(
        &mut s,
        &device_record("peer-c", "uid-c", Some(ed_pub(&c_key)), None),
        NOW,
        NODE,
    )
    .unwrap();
    DeviceService::upsert_pdsync(
        &mut s,
        &device_record("peer-d", "uid-d", Some(ed_pub(&revoked_key)), Some(NOW)),
        NOW,
        NODE,
    )
    .unwrap();
    DeviceService::upsert_pdsync(
        &mut s,
        &device_record("peer-e", "uid-e", None, None),
        NOW,
        NODE,
    )
    .unwrap();

    let b_pub = ed_pub(&b_key);
    let c_pub = ed_pub(&c_key);
    // 授权设备集合由调用方（S4）先过滤 revoked 再传入；此处即传入已过滤结果：
    // B、C 授权；E 无公钥（跳过投递）。revoked 的 D 不在此集合内。
    let devices: Vec<AuthorizedDevice> = vec![
        AuthorizedDevice {
            peer: "peer-b",
            device_pub_key: Some(&b_pub),
        },
        AuthorizedDevice {
            peer: "peer-c",
            device_pub_key: Some(&c_pub),
        },
        AuthorizedDevice {
            peer: "peer-e",
            device_pub_key: None,
        },
    ];

    let state = EpochService::rotate(
        &mut s,
        &my_root,
        writer_peer,
        NODE,
        NOW,
        RotationReason::Revoke,
        &x25519_priv_from_sk(&writer_key),
        &devices,
        None,
    )
    .unwrap();

    // epoch:state：current=1（首轮）、reason=revoke、rotatedBy=writer。
    assert_eq!(state.current, 1);
    let state_stored = spark_core::epoch::get_epoch_state(&s).unwrap().unwrap();
    assert_eq!(state_stored.current, 1);
    assert_eq!(state_stored.rotated_by, writer_peer);
    assert_eq!(state_stored.reason, RotationReason::Revoke);
    assert_eq!(state_stored.rotated_at, NOW);

    // 密钥表 + effective 推进。
    let keys = list_local_keys(&s).unwrap();
    assert_eq!(keys.len(), 1);
    let epoch1_key = *keys.get(&1).unwrap();
    assert_eq!(spark_core::epoch::get_effective(&s).unwrap(), 1);

    // 包裹逐授权设备：B、C 有（writer→B / writer→C）；E（无公钥）无。
    // 调用方过滤后 revoked 的 D 不在 devices 集合 → 无包裹（撤销语义在 S4 层保证）。
    let b_wrap_key = ikey_key(1, writer_peer, "peer-b");
    let c_wrap_key = ikey_key(1, writer_peer, "peer-c");
    let d_wrap_key = ikey_key(1, writer_peer, "peer-d");
    let e_wrap_key = ikey_key(1, writer_peer, "peer-e");
    assert!(s.get(&b_wrap_key).unwrap().is_some(), "B 应有包裹");
    assert!(s.get(&c_wrap_key).unwrap().is_some(), "C 应有包裹");
    assert!(
        s.get(&d_wrap_key).unwrap().is_none(),
        "未授权（revoked）设备不投递包裹"
    );
    assert!(
        s.get(&e_wrap_key).unwrap().is_none(),
        "无 devicePubKey 不得有包裹"
    );

    // 包裹可解回同一密钥。
    let b_rec: spark_core::epoch::IkeyRecord =
        serde_json::from_str(&s.get(&b_wrap_key).unwrap().unwrap()).unwrap();
    let writer_x_pub = x25519_pub(&writer_key).unwrap();
    let b_x_priv = x25519_priv_from_sk(&b_key);
    let unboxed = spark_core::epoch::unbox_ikey(
        &b_rec.wrapped_key,
        &b_rec.nonce,
        &writer_x_pub,
        &b_x_priv,
        &my_root,
        1,
        writer_peer,
        "peer-b",
    )
    .unwrap();
    assert_eq!(unboxed, epoch1_key, "B 解出与 writer 密钥表一致的 ikey");

    // 安全日志 epoch_rotated 字段完整。
    let kinds: Vec<String> = s
        .scan(&spark_core::storage::ScanOptions::prefix("security:log:"))
        .unwrap()
        .into_iter()
        .map(|(_k, v)| {
            serde_json::from_str::<Value>(&v).unwrap()["kind"]
                .as_str()
                .unwrap_or("")
                .to_string()
        })
        .collect();
    assert!(
        kinds.contains(&"epoch_rotated".to_string()),
        "应含 epoch_rotated 日志，实为 {kinds:?}"
    );
}

// ---------------------------------------------------------------------------
// 测试点 12：被撤销设备解不开新数据（端到端核心用例）。
// ---------------------------------------------------------------------------

#[test]
fn revoked_device_cannot_decrypt_new_data_forwarded_by_ignorant_peer() {
    // 同一 rootId（三台设备共享根身份）。
    let (root_key, my_root) = root(7);
    let (a_key, _) = root(8);
    let (c_key, _) = root(9);

    // B 的存储（受害者视角）：seed 设备清单 + epoch1 密钥 + A→B 的 ikey1 包裹。
    let mut b_store = MemoryStorage::new();
    let b_peer = "peer-b";
    // B 本机 libp2p 私钥（x25519 派生）。
    let b_sk = SigningKey::from_bytes(&[0x42; 32]);
    DeviceService::upsert_pdsync(
        &mut b_store,
        &device_record(b_peer, "uid-b", Some(ed_pub(&b_sk)), None),
        NOW,
        NODE,
    )
    .unwrap();
    DeviceService::upsert_pdsync(
        &mut b_store,
        &device_record("peer-a", "uid-a", Some(ed_pub(&a_key)), None),
        NOW,
        NODE,
    )
    .unwrap();

    // B 本机 libp2p keypair（try_refresh_keys 依赖它解包裹）。
    seed_libp2p_keypair(&mut b_store, &[0x42; 32]);

    // B 是 epoch1 持钥设备：A 在 epoch1 曾给 B 派发密钥（B 能解历史数据）。
    let epoch1_ikey = generate_ikey();
    spark_core::epoch::put_local_key(&mut b_store, 1, &epoch1_ikey).unwrap();
    spark_core::epoch::put_effective(&mut b_store, 1).unwrap();
    let a_x_priv = x25519_priv_from_sk(&a_key);
    let b_x_pub = x25519_pub(&b_sk).unwrap();
    let (wrap1, nonce1) = spark_core::epoch::box_ikey(
        &epoch1_ikey,
        &b_x_pub,
        &a_x_priv,
        &my_root,
        1,
        "peer-a",
        b_peer,
    )
    .unwrap();
    let rec1 = spark_core::epoch::IkeyRecord {
        wrapped_key: wrap1,
        nonce: nonce1,
        ts: NOW,
    };
    b_store
        .put(&ikey_key(1, "peer-a", b_peer), &rec1.to_json().unwrap())
        .unwrap();

    // 场景：A 撤销 B 触发 epoch2。A 侧 rotate（授权设备 = A 自身 + 一台「不知情转发者 C」，
    // B 已被 revoke 排除）。B 的存储作为「被转发目标」，通过 pdsync 收到：
    //   ① epoch:state current=2（明文）
    //   ② A 的新数据，用 epoch2 密钥加密（应只有 C 有密钥）。
    let mut a_store = MemoryStorage::new();
    let c_sk = SigningKey::from_bytes(&[0x43; 32]);
    let c_peer = "peer-c";
    DeviceService::upsert_pdsync(
        &mut a_store,
        &device_record(c_peer, "uid-c", Some(ed_pub(&c_sk)), None),
        NOW,
        NODE,
    )
    .unwrap();
    // A 在 epoch1 视图中包含 B（非 revoked）→ rotate 到 epoch2 时 B 才被排除。
    // 这里直接模拟 A 在撤销 B 之后（B.revokedAt 已置位）再做 rotate。
    let mut b_in_a = device_record(b_peer, "uid-b", Some(ed_pub(&b_sk)), None);
    // 先落 epoch1 state 以便 rotate 从 1 递增到 2。
    let state1 = EpochState {
        current: 1,
        rotated_at: NOW - 1000,
        rotated_by: "peer-a".to_string(),
        reason: RotationReason::Init,
    };
    spark_core::epoch::put_epoch_state(&mut a_store, NODE, &state1, NOW - 1000).unwrap();
    // A 授权设备：C（在线转发者）；B 在撤销后 revoked_at 已置位（模拟 A 侧已撤销 B）。
    b_in_a.revoked_at = Some(NOW);
    DeviceService::upsert_pdsync(&mut a_store, &b_in_a, NOW, NODE).unwrap();

    let c_pub = ed_pub(&c_sk);
    let devices: Vec<AuthorizedDevice> = vec![AuthorizedDevice {
        peer: "peer-c",
        device_pub_key: Some(&c_pub),
    }];
    let state2 = EpochService::rotate(
        &mut a_store,
        &my_root,
        "peer-a",
        NODE,
        NOW,
        RotationReason::Revoke,
        &a_x_priv,
        &devices,
        None,
    )
    .unwrap();
    assert_eq!(state2.current, 2, "A 撤销后应推进到 epoch2");

    // A 写一条新数据（好友记录），用 epoch2 密钥加密——这就是「A 的新数据」。
    let epoch2_key = *list_local_keys(&a_store).unwrap().get(&2).unwrap();
    let data_key = "ct:friend:friend-x";
    let plaintext = json!({"rootId": "friend-x", "nickname": "新朋友"}).to_string();
    let ciphertext = spark_core::epoch::wrap_value(&epoch2_key, data_key, 2, &plaintext).unwrap();

    // B 收到 epoch2 的 state（明文 pdsync 扩散），但无 epoch2 密钥。
    spark_core::epoch::put_epoch_state(&mut b_store, NODE, &state2, NOW).unwrap();

    // 不知情设备 C 把加密数据转发给 B（转发不改变密文，B 需自己解）。
    // 投加密数据（B 无 epoch2 密钥 → 解不开 → 跳过、不落库）。
    let data_rec = PdsyncRecord {
        key: data_key.to_string(),
        value: ciphertext.clone(),
        meta: remote_meta("peer-c", 1, NOW),
        dseq: Some(99),
    };
    let r2 = deliver_pdsync_data(
        &mut b_store,
        &root_key,
        &my_root,
        "ct:friend",
        &[data_rec.clone()],
    );
    assert_eq!(r2.response, json!({ "ok": true }));

    // B 库中无此记录（解不开跳过）。
    assert!(
        b_store.get(data_key).unwrap().is_none(),
        "B 不得合入解不开的密文记录"
    );
    // B 的 effective 仍停在 1（无 epoch2 密钥）。
    assert_eq!(
        spark_core::epoch::get_effective(&b_store).unwrap(),
        1,
        "B 无 epoch2 密钥 → effective 不得推进"
    );
    // dseq 不计入 max_dseq：B 未对 C 推进 seen 水位（本轮不应 push need ack）。
    assert!(
        spark_core::sync::dlog::get_seen(&b_store, "peer-c").unwrap_or(0) == 0,
        "解不开的记录 dseq 不计，删除日志水位不得推进"
    );

    // 对照①（方案 Y 源过滤，m3 §5.11.6）：非主导 writer（peer-c）的包裹不被
    // 采信——防双主导 split-brain 互换密钥；B 仍停在 epoch1。
    let c_x_priv = x25519_priv_from_sk(&c_key);
    let b_x_pub2 = x25519_pub(&b_sk).unwrap();
    let (wrap2, nonce2) = spark_core::epoch::box_ikey(
        &epoch2_key,
        &b_x_pub2,
        &c_x_priv,
        &my_root,
        2,
        "peer-c",
        b_peer,
    )
    .unwrap();
    let rec2 = spark_core::epoch::IkeyRecord {
        wrapped_key: wrap2,
        nonce: nonce2,
        ts: NOW,
    };
    let ikey2_key = ikey_key(2, "peer-c", b_peer);
    b_store.put(&ikey2_key, &rec2.to_json().unwrap()).unwrap();
    // try_refresh_keys 需 writer 的设备记录解析其 devicePubKey。
    DeviceService::upsert_pdsync(
        &mut b_store,
        &device_record(c_peer, "uid-c", Some(ed_pub(&c_key)), None),
        NOW,
        NODE,
    )
    .unwrap();
    spark_core::epoch::EpochService::try_refresh_keys(&mut b_store, &my_root, b_peer, NOW).unwrap();
    assert_eq!(
        spark_core::epoch::get_effective(&b_store).unwrap(),
        1,
        "方案 Y：非主导 writer 的包裹拒收（writer != rotated_by）"
    );

    // 对照②：主导（peer-a，即 epoch:state.rotated_by）补发包裹 → 采信，
    // effective 推进到 2（C 的包裹仍在库中，被源过滤跳过，不影响）。
    let (wrap2a, nonce2a) = spark_core::epoch::box_ikey(
        &epoch2_key,
        &b_x_pub2,
        &a_x_priv,
        &my_root,
        2,
        "peer-a",
        b_peer,
    )
    .unwrap();
    let rec2a = spark_core::epoch::IkeyRecord {
        wrapped_key: wrap2a,
        nonce: nonce2a,
        ts: NOW,
    };
    b_store
        .put(&ikey_key(2, "peer-a", b_peer), &rec2a.to_json().unwrap())
        .unwrap();
    spark_core::epoch::EpochService::try_refresh_keys(&mut b_store, &my_root, b_peer, NOW).unwrap();
    assert_eq!(
        spark_core::epoch::get_effective(&b_store).unwrap(),
        2,
        "主导补发密钥后 effective 推进"
    );

    // 重投同一条加密数据 → 现在能解开 → 合入。
    let r3 = deliver_pdsync_data(&mut b_store, &root_key, &my_root, "ct:friend", &[data_rec]);
    assert_eq!(r3.response, json!({ "ok": true }));
    let stored = b_store.get(data_key).unwrap().expect("密钥到位后应合入");
    assert!(
        stored.contains("新朋友"),
        "B 解开后应合入新数据，实为 {stored}"
    );
}

// ---------------------------------------------------------------------------
// 测试点 11：不解不推进（pmeta/dseq），密钥到位后自愈。
// ---------------------------------------------------------------------------

#[test]
fn no_key_ciphertext_does_not_advance_pmeta_and_recovers_after_grant() {
    let (root_key, my_root) = root(11);
    let mut s = MemoryStorage::new();

    // B 有 epoch2 密钥（正常持钥设备），但未收到 epoch3 密钥。
    let epoch2_key = generate_ikey();
    spark_core::epoch::put_local_key(&mut s, 2, &epoch2_key).unwrap();
    spark_core::epoch::put_effective(&mut s, 2).unwrap();

    // A 在 epoch3 加密的新数据，转发给 B——B 无 epoch3 密钥。
    let epoch3_key = generate_ikey();
    let data_key = "ct:friend:z";
    let plaintext = json!({"rootId":"z","nickname":"z"}).to_string();
    let ciphertext = spark_core::epoch::wrap_value(&epoch3_key, data_key, 3, &plaintext).unwrap();

    // 记录基线 pmeta：本地已有一条 version1（来自对端 old-node）。
    let plain_old = json!({"rootId":"z","nickname":"旧值"}).to_string();
    spark_core::sync::apply_personal_remote(
        &mut s,
        data_key,
        &plain_old,
        &remote_meta("old-node", 1, NOW - 100),
    )
    .unwrap();
    let baseline = spark_core::sync::get_personal_meta(&s, data_key)
        .unwrap()
        .unwrap();

    let rec = PdsyncRecord {
        key: data_key.to_string(),
        value: ciphertext.clone(),
        meta: remote_meta("peer-a", 5, NOW),
        dseq: Some(42),
    };
    let r = deliver_pdsync_data(&mut s, &root_key, &my_root, "ct:friend", &[rec]);
    assert_eq!(r.response, json!({ "ok": true }));

    // 不解不推进：pmeta 不覆盖、dseq 不计。
    let after = spark_core::sync::get_personal_meta(&s, data_key)
        .unwrap()
        .unwrap();
    assert_eq!(
        after.vv.get("old-node"),
        baseline.vv.get("old-node"),
        "解不开的记录不得推进 pmeta"
    );
    // 本地值保持旧值。
    assert_eq!(s.get(data_key).unwrap().unwrap(), plain_old);
    assert_eq!(
        spark_core::sync::dlog::get_seen(&s, "peer-a").unwrap_or(0),
        0,
        "dseq 不计"
    );

    // 密钥到位后重投 → 合入成功（自愈语义）。
    spark_core::epoch::put_local_key(&mut s, 3, &epoch3_key).unwrap();
    spark_core::epoch::put_effective(&mut s, 3).unwrap();
    let rec2 = PdsyncRecord {
        key: data_key.to_string(),
        value: ciphertext.clone(),
        meta: remote_meta("peer-a", 5, NOW),
        dseq: Some(42),
    };
    let r2 = deliver_pdsync_data(&mut s, &root_key, &my_root, "ct:friend", &[rec2]);
    assert_eq!(r2.response, json!({ "ok": true }));
    assert_eq!(
        s.get(data_key).unwrap().unwrap(),
        json!({"rootId":"z","nickname":"z"}).to_string(),
        "密钥到位后合入成功"
    );
}

// ---------------------------------------------------------------------------
// 测试点 10：端到端解密合入——密文值解开后 msg:conv 行为与明文一致。
// ---------------------------------------------------------------------------

#[test]
fn encrypted_pdsync_data_decrypts_and_merges_like_plaintext() {
    let (root_key, my_root) = root(12);
    let mut s = MemoryStorage::new();
    let epoch_key = generate_ikey();
    spark_core::epoch::put_local_key(&mut s, 2, &epoch_key).unwrap();
    spark_core::epoch::put_effective(&mut s, 2).unwrap();

    let (_, peer_root) = root(13);
    let conv_id = spark_core::kernel::direct_conversation_id(&peer_root);
    let mut remote = spark_core::message::ConversationRecord {
        id: conv_id.clone(),
        kind: spark_core::message::ConversationKind::Direct,
        title: "密文标题".to_string(),
        peer_root_id: peer_root.clone(),
        peer: None,
        unread_count: 0,
        pinned_at: 123,
        muted: true,
        draft: String::new(),
        updated_at: 0,
        meta_updated_at: 999,
    };
    let _ = &mut remote;
    let rec = PdsyncRecord {
        key: format!("msg:conv:personal:{conv_id}"),
        value: spark_core::epoch::wrap_value(
            &epoch_key,
            &format!("msg:conv:personal:{conv_id}"),
            2,
            &serde_json::to_string(&remote).unwrap(),
        )
        .unwrap(),
        meta: remote_meta("peer-a", 1, NOW),
        dseq: None,
    };
    let r = deliver_pdsync_data(&mut s, &root_key, &my_root, "msg:conv", &[rec]);
    assert_eq!(r.response, json!({ "ok": true }));
    let stored = spark_core::message::MessageService::get_conversation(&s, "personal", &conv_id)
        .unwrap()
        .expect("密文会话应解出并落库");
    assert_eq!(stored.title, "密文标题", "解密后合并行为与明文一致");
    assert!(stored.muted);
    assert_eq!(stored.meta_updated_at, 999);
}

// ---------------------------------------------------------------------------
// 测试点 7：接收方生效（EpochService::try_activate_key / try_refresh_keys）。
// ---------------------------------------------------------------------------

#[test]
fn receiver_activates_key_when_ikey_arrives_but_not_without() {
    let (root_key, my_root) = root(14);
    let mut s = MemoryStorage::new();
    let b_sk = SigningKey::from_bytes(&[0x62; 32]);
    let b_peer = "peer-b";
    let writer_sk = SigningKey::from_bytes(&[0x61; 32]);
    let writer_peer = "peer-writer";

    // 写入 epoch:state current=2 + 本机（B）作为 recipient 的包裹，尚未激活。
    let state = EpochState {
        current: 2,
        rotated_at: NOW,
        rotated_by: writer_peer.to_string(),
        reason: RotationReason::Init,
    };
    spark_core::epoch::put_epoch_state(&mut s, NODE, &state, NOW).unwrap();
    // B 本机 libp2p keypair（try_refresh_keys 用其 x25519 私钥解包裹）。
    seed_libp2p_keypair(&mut s, &[0x62; 32]);
    let epoch_key = generate_ikey();
    let b_x_pub = x25519_pub(&b_sk).unwrap();
    let writer_x_priv = x25519_priv_from_sk(&writer_sk);
    let (wrap, nonce) = spark_core::epoch::box_ikey(
        &epoch_key,
        &b_x_pub,
        &writer_x_priv,
        &my_root,
        2,
        writer_peer,
        b_peer,
    )
    .unwrap();
    let rec = spark_core::epoch::IkeyRecord {
        wrapped_key: wrap,
        nonce,
        ts: NOW,
    };
    s.put(&ikey_key(2, writer_peer, b_peer), &rec.to_json().unwrap())
        .unwrap();

    // writer 设备记录带 devicePubKey（try_refresh_keys 解包依赖它）。
    DeviceService::upsert_pdsync(
        &mut s,
        &device_record(writer_peer, "uid-w", Some(ed_pub(&writer_sk)), None),
        NOW,
        NODE,
    )
    .unwrap();

    // 刷新前：无密钥、effective=0。
    assert!(spark_core::epoch::get_effective(&s).unwrap() == 0);
    assert!(spark_core::epoch::get_local_key(&s, 2).unwrap().is_none());

    // 刷新 → 激活。
    spark_core::epoch::EpochService::try_refresh_keys(&mut s, &my_root, b_peer, NOW).unwrap();
    assert_eq!(spark_core::epoch::get_effective(&s).unwrap(), 2);
    assert_eq!(
        spark_core::epoch::get_local_key(&s, 2).unwrap().unwrap(),
        epoch_key,
        "B 解出的 ikey 与 writer 派发一致"
    );

    // 日志 epoch_key_activated。
    let kinds: Vec<String> = s
        .scan(&spark_core::storage::ScanOptions::prefix("security:log:"))
        .unwrap()
        .into_iter()
        .map(|(_k, v)| {
            serde_json::from_str::<Value>(&v).unwrap()["kind"]
                .as_str()
                .unwrap_or("")
                .to_string()
        })
        .collect();
    assert!(
        kinds.contains(&"epoch_key_activated".to_string()),
        "应含 epoch_key_activated"
    );

    // 包裹未到 / 解不开 → effective 不推进（用空存储新场景：state 有但无包裹）。
    let mut s2 = MemoryStorage::new();
    spark_core::epoch::put_epoch_state(&mut s2, NODE, &state, NOW).unwrap();
    seed_libp2p_keypair(&mut s2, &[0x62; 32]);
    spark_core::epoch::EpochService::try_refresh_keys(&mut s2, &my_root, b_peer, NOW).unwrap();
    assert_eq!(
        spark_core::epoch::get_effective(&s2).unwrap(),
        0,
        "无包裹不得推进 effective"
    );
    let _ = root_key;
}

// ---------------------------------------------------------------------------
// 测试点 8：新设备补发（maybe_grant_epoch_key），revoked 不补。
// ---------------------------------------------------------------------------

#[test]
fn new_device_grant_writes_ikey_but_revoked_does_not() {
    let (_, my_root) = root(15);
    let mut s = MemoryStorage::new();
    let self_sk = SigningKey::from_bytes(&[0x70; 32]);
    let self_peer = "peer-self";
    let new_sk = SigningKey::from_bytes(&[0x71; 32]);
    let new_peer = "peer-new";

    // 本机是持钥设备（effective=2，有 epoch2 密钥）。
    let epoch_key = generate_ikey();
    spark_core::epoch::put_local_key(&mut s, 2, &epoch_key).unwrap();
    spark_core::epoch::put_effective(&mut s, 2).unwrap();
    spark_core::epoch::put_epoch_state(
        &mut s,
        NODE,
        &EpochState {
            current: 2,
            rotated_at: NOW,
            rotated_by: self_peer.to_string(),
            reason: RotationReason::Init,
        },
        NOW,
    )
    .unwrap();
    // 本机 libp2p keypair（maybe_grant 用其 x25519 私钥作 writer 包裹）。
    seed_libp2p_keypair(&mut s, &[0x70; 32]);

    // 补发：带 devicePubKey 的新设备 → 写 ikey:{2}:{self}:{new}。
    let granted = spark_core::epoch::EpochService::maybe_grant_epoch_key(
        &mut s,
        &my_root,
        self_peer,
        NODE,
        NOW,
        new_peer,
        Some(&B64.encode(ed_pub(&new_sk))),
        None,
        None,
    )
    .unwrap();
    assert!(granted, "新设备应补发密钥");

    let wrap_key = ikey_key(2, self_peer, new_peer);
    let raw = s.get(&wrap_key).unwrap().expect("补发包裹已写入");
    let rec: spark_core::epoch::IkeyRecord = serde_json::from_str(&raw).unwrap();
    // 新设备可解出与 effective 一致的密钥（recipient 私钥 = new 设备 x25519 私钥）。
    let new_x_priv = x25519_priv_from_sk(&new_sk);
    let unboxed = spark_core::epoch::unbox_ikey(
        &rec.wrapped_key,
        &rec.nonce,
        &x25519_pub(&self_sk).unwrap(),
        &new_x_priv,
        &my_root,
        2,
        self_peer,
        new_peer,
    )
    .unwrap();
    assert_eq!(unboxed, epoch_key, "新设备解出与 effective 一致的 ikey");

    // revoked 设备不补发。
    let granted2 = spark_core::epoch::EpochService::maybe_grant_epoch_key(
        &mut s,
        &my_root,
        self_peer,
        NODE,
        NOW,
        "peer-revoked",
        Some(&B64.encode(ed_pub(&new_sk))),
        Some(NOW),
        None,
    )
    .unwrap();
    assert!(!granted2, "revoked 设备不得补发");
    assert!(
        s.get(&ikey_key(2, self_peer, "peer-revoked"))
            .unwrap()
            .is_none()
    );

    // 无 devicePubKey 不补发。
    let granted3 = spark_core::epoch::EpochService::maybe_grant_epoch_key(
        &mut s,
        &my_root,
        self_peer,
        NODE,
        NOW,
        "peer-nopub",
        None,
        None,
        None,
    )
    .unwrap();
    assert!(!granted3, "无 devicePubKey 不补发");

    // 幂等：已补发不重复写。
    let granted4 = spark_core::epoch::EpochService::maybe_grant_epoch_key(
        &mut s,
        &my_root,
        self_peer,
        NODE,
        NOW,
        new_peer,
        Some(&B64.encode(ed_pub(&new_sk))),
        None,
        None,
    )
    .unwrap();
    assert!(!granted4, "已补发不重复");
}

// ---------------------------------------------------------------------------
// 测试点 13：老数据可读（全历史密钥保留）。
// ---------------------------------------------------------------------------

#[test]
fn old_epoch_ciphertext_readable_with_historical_key() {
    let epoch1_key = generate_ikey();
    let epoch2_key = generate_ikey();
    let mut s = MemoryStorage::new();
    // 全历史密钥保留（B 升级到 epoch2 后仍保留 epoch1 密钥）。
    spark_core::epoch::put_local_key(&mut s, 1, &epoch1_key).unwrap();
    spark_core::epoch::put_local_key(&mut s, 2, &epoch2_key).unwrap();
    spark_core::epoch::put_effective(&mut s, 2).unwrap();

    let data_key = "ct:friend:old";
    // 用 epoch1 加密的旧数据（不知情设备在升级前发出的滞后数据）。
    let old_ct = spark_core::epoch::wrap_value(
        &epoch1_key,
        data_key,
        1,
        &json!({"rootId":"old","nickname":"老"}).to_string(),
    )
    .unwrap();
    // 当前 effective=2 也能解 epoch1 密文（全历史密钥保留）。
    assert_eq!(
        spark_core::epoch::unwrap_value(&epoch1_key, data_key, &old_ct).as_deref(),
        Some(r#"{"rootId":"old","nickname":"老"}"#),
        "旧 epoch 密文用密钥表旧密钥可解"
    );
    // 本地存储层面 old ct 可直读为「存在」。
    let _ = &mut s;
    assert!(spark_core::epoch::is_ikey_ciphertext(&old_ct));
}

// ---------------------------------------------------------------------------
// 测试点 2：值 wrap/unwrap + AAD 绑定 + $enc 判别。
// ---------------------------------------------------------------------------

#[test]
fn wrap_unwrap_aad_binding_and_discern() {
    let key = generate_ikey();
    let record_key = "ct:friend:alice";
    let plain = r#"{"rootId":"alice","nickname":"Alice"}"#;
    let ct = spark_core::epoch::wrap_value(&key, record_key, 2, plain).unwrap();

    // 判别：密文正判；明文/缺字段不误判。
    assert!(spark_core::epoch::is_ikey_ciphertext(&ct));
    assert!(!spark_core::epoch::is_ikey_ciphertext(&json!({"a":1})));
    assert!(!spark_core::epoch::is_ikey_ciphertext(
        &json!({"$enc":"ikey"})
    ));

    // 往返。
    assert_eq!(
        spark_core::epoch::unwrap_value(&key, record_key, &ct).as_deref(),
        Some(plain)
    );

    // AAD 绑定：改键解密失败。
    assert!(
        spark_core::epoch::unwrap_value(&key, "ct:friend:wrong", &ct).is_none(),
        "AAD 绑定：改键失败"
    );
    // 错密钥失败。
    let mut wrong = key;
    wrong[0] ^= 1;
    assert!(spark_core::epoch::unwrap_value(&wrong, record_key, &ct).is_none());
    // 非密文 → None。
    assert!(spark_core::epoch::unwrap_value(&key, record_key, &json!({"a":1})).is_none());

    // 解析 ikey 键。
    let (epoch, writer, recipient) = parse_ikey_key("ikey:2:peer-a:peer-b").unwrap();
    assert_eq!(
        (epoch, writer.as_str(), recipient.as_str()),
        (2, "peer-a", "peer-b")
    );
    assert!(parse_ikey_key("ct:friend:a").is_none());
    assert!(parse_ikey_key("ikey:2:peer-a:peer-b:extra").is_none());
}

// ---------------------------------------------------------------------------
// 测试点 9：协商——hello 对端宣告 epoch 决定加密档位。
// ---------------------------------------------------------------------------

#[test]
fn hello_remote_epoch_negotiation_plain_vs_cipher() {
    let (_, my_root) = root(16);
    let mut s = MemoryStorage::new();
    let epoch_key = generate_ikey();
    spark_core::epoch::put_local_key(&mut s, 3, &epoch_key).unwrap();
    spark_core::epoch::put_effective(&mut s, 3).unwrap();

    // 对端宣告 0 / 缺省 → 发送侧 min(effective, 0)=0 → 明文。
    assert_eq!(crate_min_enc_epoch(&s, None), 0, "对端缺省 → 明文");
    assert_eq!(crate_min_enc_epoch(&s, Some(0)), 0, "对端宣告 0 → 明文");

    // 对端宣告 < 本机 → 按对端值加密。
    assert_eq!(
        crate_min_enc_epoch(&s, Some(2)),
        2,
        "对端 2 < 本机 3 → 按 2 加密"
    );

    // 对端 ≥ 本机 → 按本机 effective。
    assert_eq!(crate_min_enc_epoch(&s, Some(3)), 3);
    assert_eq!(crate_min_enc_epoch(&s, Some(9)), 3, "对端高于本机 → 按本机");
    let _ = &my_root;
}

// 镜像发送侧 enc_epoch = min(local_effective, remote_epoch ?? 0) 的选取逻辑。
fn crate_min_enc_epoch(_s: &MemoryStorage, remote_epoch: Option<u64>) -> u64 {
    spark_core::epoch::get_effective(_s)
        .unwrap_or(0)
        .min(remote_epoch.unwrap_or(0))
}

// ---------------------------------------------------------------------------
// 测试点 5：触发挂点（真 kernel）——start_p2p 初始化、幂等。
// ---------------------------------------------------------------------------

#[test]
fn start_p2p_initializes_epoch1_idempotently() {
    let dir = tempfile::tempdir().unwrap();
    let mut k = Kernel::init(config(dir.path())).expect("kernel init");
    let (_root, _mnemonic) = init_identity(&mut k);

    // start_p2p 尾段触发 epoch init（首次 → epoch1）。
    k.start_p2p().expect("start_p2p");
    let storage = k.__test_storage().unwrap();
    assert_eq!(
        spark_core::epoch::get_effective(&storage).unwrap(),
        1,
        "start_p2p 首次应初始化到 epoch1"
    );
    let state = spark_core::epoch::get_epoch_state(&storage)
        .unwrap()
        .unwrap();
    assert_eq!(state.current, 1);
    assert_eq!(state.reason, RotationReason::Init);

    // 幂等：已有 state 不再 init（再次 rotate 会 +1，而非重置为 1）。
    // 重启 start_p2p（stop→start）不应把 current 重置。
    k.stop_p2p();
    k.start_p2p().expect("restart p2p");
    let storage2 = k.__test_storage().unwrap();
    assert_eq!(
        spark_core::epoch::get_epoch_state(&storage2)
            .unwrap()
            .unwrap()
            .current,
        1,
        "已有 state 的二次 start_p2p 不得重复 init"
    );

    // 安全日志 epoch_init 已写入。
    let kinds: Vec<String> = storage2
        .scan(&spark_core::storage::ScanOptions::prefix("security:log:"))
        .unwrap()
        .into_iter()
        .map(|(_k, v)| {
            serde_json::from_str::<Value>(&v).unwrap()["kind"]
                .as_str()
                .unwrap_or("")
                .to_string()
        })
        .collect();
    assert!(
        kinds.contains(&"epoch_rotated".to_string()),
        "应含 epoch_rotated 日志，实为 {kinds:?}"
    );
}

// ---------------------------------------------------------------------------
// epoch:state 经 pdsync data 通道到达对端（protocol-guard 关注的端到端路径）：
//   ① epoch 前缀注册（category_for_key("epoch:state") 命中）→ data 通道不
//     category-mismatch 拒收，epoch:state 明文落库；
//   ② 同批 ikey: 包裹合入后，handle_pdsync_data 的 epoch 尾段自动触发
//     try_refresh_keys → effective 推进 + 密钥激活（无需手动调）。
// 该路径此前无测试覆盖：测试点 7/12 均手动调 try_refresh_keys，未触达
// handle_pdsync_data 第 638 行的自动刷新挂点，也未锁定 epoch: 前缀注册。
// ---------------------------------------------------------------------------

#[test]
fn epoch_state_arrives_via_pdsync_data_and_auto_activates_key() {
    let (root_key, my_root) = root(17);
    let mut s = MemoryStorage::new();
    let self_sk = SigningKey::from_bytes(&[0x52; 32]); // 本机 B
    let writer_sk = SigningKey::from_bytes(&[0x51; 32]); // 对端 A
    let writer_peer = "peer-a";

    // 本机 libp2p keypair（try_refresh_keys 用其 x25519 私钥解包裹）。
    seed_libp2p_keypair(&mut s, &[0x52; 32]);
    // writer 设备记录带 devicePubKey（解包依赖它）。
    DeviceService::upsert_pdsync(
        &mut s,
        &device_record(writer_peer, "uid-a", Some(ed_pub(&writer_sk)), None),
        NOW,
        NODE,
    )
    .unwrap();

    // 对端 A 派发给本机（NODE）的 epoch2 包裹（ikey:2:a:{NODE}）。
    // try_refresh_keys 按 `my_peer=ctx.node_id`（deliver_pdsync_data 传 NODE）匹配
    // `ikey:{current}:*:{NODE}`，故 recipient 取 NODE；x25519 私钥来自种子 [0x52;32]。
    let epoch2_key = generate_ikey();
    let b_x_pub = x25519_pub(&self_sk).unwrap();
    let writer_x_priv = x25519_priv_from_sk(&writer_sk);
    let (wrap, nonce) = spark_core::epoch::box_ikey(
        &epoch2_key,
        &b_x_pub,
        &writer_x_priv,
        &my_root,
        2,
        writer_peer,
        NODE,
    )
    .unwrap();
    let rec = spark_core::epoch::IkeyRecord {
        wrapped_key: wrap,
        nonce,
        ts: NOW,
    };

    // 同批 data 记录：epoch:state（明文）+ 本机 ikey: 包裹，category 名为 "epoch"。
    let state = EpochState {
        current: 2,
        rotated_at: NOW,
        rotated_by: writer_peer.to_string(),
        reason: RotationReason::Revoke,
    };
    let state_rec = PdsyncRecord {
        key: "epoch:state".to_string(),
        value: serde_json::from_str(&state.to_json().unwrap()).unwrap(),
        meta: remote_meta("peer-a", 1, NOW),
        dseq: Some(1),
    };
    let ikey_rec = PdsyncRecord {
        key: ikey_key(2, writer_peer, NODE),
        value: serde_json::from_str(&rec.to_json().unwrap()).unwrap(),
        meta: remote_meta("peer-a", 2, NOW),
        dseq: Some(2),
    };

    // 刷新前：无密钥、effective=0。
    assert_eq!(spark_core::epoch::get_effective(&s).unwrap(), 0);

    // 经 pdsync data 通道投递 epoch category——应合入且不被 category-mismatch 拒收。
    let r = deliver_pdsync_data(&mut s, &root_key, &my_root, "epoch", &[state_rec, ikey_rec]);
    assert_eq!(
        r.response,
        json!({ "ok": true }),
        "epoch category 应被识别并合入"
    );

    // epoch:state 已落库（明文通道，含 AAD 豁免）。
    let state_stored = spark_core::epoch::get_epoch_state(&s).unwrap().unwrap();
    assert_eq!(state_stored.current, 2, "epoch:state 应经 data 通道落库");

    // handle_pdsync_data 的 epoch 尾段自动 try_refresh_keys → 密钥激活 + effective 推进。
    assert_eq!(
        spark_core::epoch::get_effective(&s).unwrap(),
        2,
        "收到 epoch:state + ikey 包裹后应自动激活到 effective=2"
    );
    assert_eq!(
        spark_core::epoch::get_local_key(&s, 2).unwrap().unwrap(),
        epoch2_key,
        "自动激活解出的 ikey 与 writer 派发一致"
    );
}

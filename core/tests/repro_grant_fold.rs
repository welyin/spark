// 临时定向复现：ikey 补发后 epoch 折叠 vv 是否立即可见（qr 收敛断点定位）。
// 运行：cargo test --test kernel_epoch repro_grant_fold -- --nocapture

use spark_core::device::{DeviceRecord, DeviceService};
use spark_core::epoch::{EpochService, EpochState, RotationReason, put_epoch_state};
use spark_core::storage::{MemoryStorage, StorageBackend};
use spark_core::sync::pdsync::{CATEGORIES, collect_category_vv};

fn b64_ed(pk: &[u8; 32]) -> String {
    base64::Engine::encode(&base64::engine::general_purpose::STANDARD, pk)
}

#[test]
fn repro_grant_fold_includes_ikey() {
    let mut s = MemoryStorage::new();
    let now = 1_720_000_000_000i64;
    // A 侧 libp2p keypair（grant 需 x25519 私钥）。
    let secret = libp2p::identity::ed25519::SecretKey::try_from_bytes([0x41; 32]).unwrap();
    let lp = libp2p::identity::Keypair::from(libp2p::identity::ed25519::Keypair::from(secret));
    let raw = lp.to_protobuf_encoding().unwrap();
    s.put(
        "p2p:identity:privateKey",
        &base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &raw),
    )
    .unwrap();
    // A 侧 epoch1 state。
    let state = EpochState {
        current: 1,
        rotated_at: now - 1000,
        rotated_by: "peer-a".to_string(),
        reason: RotationReason::Init,
    };
    put_epoch_state(&mut s, "peer-a", &state, now - 1000).unwrap();
    spark_core::epoch::put_effective(&mut s, 1).unwrap();
    spark_core::epoch::put_local_key(&mut s, 1, &[7u8; 32]).unwrap();

    // recipient B 的设备记录（带 Ed25519 公钥——须为真实曲线点）。
    let b_sk = ed25519_dalek::SigningKey::from_bytes(&[0x42; 32]);
    let dev_b = DeviceRecord {
        peer_id: "peer-b".to_string(),
        device_uid: Some("uid-b".to_string()),
        device_name: "设备B".to_string(),
        os: "Android".to_string(),
        os_version: "14".to_string(),
        arch: "aarch64".to_string(),
        macs: Vec::new(),
        app_version: String::new(),
        updated_at: now,
        last_seen_at: now,
        revoked_at: None,
        device_pub_key: Some(b64_ed(&b_sk.verifying_key().to_bytes())),
    };
    DeviceService::upsert_pdsync(&mut s, &dev_b, now, "peer-a").unwrap();

    let granted = EpochService::maybe_grant_epoch_key(
        &mut s,
        "root-x",
        "peer-a",
        "peer-a",
        now,
        "peer-b",
        dev_b.device_pub_key.as_deref(),
        None,
        None,
    )
    .unwrap();
    assert!(granted, "补发应成功");

    // ikey 记录与 pmeta 已落库？
    let ikey = s.get("ikey:1:peer-a:peer-b").unwrap();
    assert!(ikey.is_some(), "ikey 记录应存在");
    let pmeta = s.get("pmeta:ikey:1:peer-a:peer-b").unwrap();
    assert!(pmeta.is_some(), "ikey pmeta 应存在: {:?}", pmeta);
    println!("ikey pmeta raw = {:?}", pmeta);
    // epoch 前缀下全部 pmeta 扫描（复刻 collect_category_vv 的扫描）。
    let scanned: Vec<(String, String)> = spark_core::storage::StorageBackend::scan(
        &s,
        &spark_core::storage::ScanOptions {
            prefix: "pmeta:ikey:".to_string(),
            start: None,
            end: None,
            reverse: false,
            limit: None,
        },
    )
    .unwrap();
    println!("scan pmeta:ikey: → {} 条", scanned.len());
    for (k, v) in &scanned {
        println!("  {k} = {v}");
    }

    // epoch 折叠应含 ikey 分量 {peer-a:2}（per-node 序号：state=1、ikey=2）。
    let cat = CATEGORIES.iter().find(|c| c.name == "epoch").unwrap();
    let vv = collect_category_vv(&s, cat, None).unwrap();
    println!("epoch fold = {vv:?}");
    assert!(
        vv.get("peer-a").copied().unwrap_or(0) >= 2,
        "epoch 折叠须含 ikey 的 {{peer-a:2}}，实际 {vv:?}"
    );
}

#[test]
fn repro_two_records_same_category_collision() {
    // 泛化回归：同 category 两条本机记录，per-node 序号使第二条（{a:2}）对
    // 已持有第一条（{a:1}）的对端**可见**——修复前两条同值 {a:1}，折叠
    // 失明，collect_incremental 判 Equal 跳过、第二条永久不可见。
    let mut s = MemoryStorage::new();
    let now = 1_720_000_000_000i64;
    spark_core::sync::personal::put_personal(&mut s, "node-a", "ct:friend:x", "\"X\"", now)
        .unwrap();
    spark_core::sync::personal::put_personal(&mut s, "node-a", "ct:friend:y", "\"Y\"", now + 1)
        .unwrap();
    let cat = CATEGORIES.iter().find(|c| c.name == "ct:friend").unwrap();
    let fold = collect_category_vv(&s, cat, None).unwrap();
    println!("ct:friend fold = {fold:?}");
    assert_eq!(fold.get("node-a").copied(), Some(2), "折叠应为 {{a:2}}");
    let known: std::collections::BTreeMap<String, i64> =
        [("node-a".to_string(), 1)].into_iter().collect();
    let inc = spark_core::sync::pdsync::collect_incremental(&s, cat, &known, None, 0).unwrap();
    let keys: Vec<String> = inc.iter().map(|r| r.key.clone()).collect();
    println!("incremental vs {{a:1}} = {keys:?}");
    assert_eq!(
        keys,
        vec!["ct:friend:y".to_string()],
        "只缺第二条 → 只推第二条"
    );
}

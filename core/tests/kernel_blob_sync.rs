//! A1/A2/A4 blob 层 loopback 集成测试（personal-data §六 / 协议 §14–16）：
//!
//! - A1 主链路：A 写大 blob → presence 扩散 → B/C 回补 → 确定性复算 →
//!   B 弃块 → 向 C 回补；诚实降级与上线重试；
//! - A2：3 设备配额 0 不驱逐（K 退化全量）；4 设备位次规则驱逐→收敛→
//!   保留位保护→回补 + hello `blobQuota` 端到端；
//! - A4：存量附件（P6 `blob:data:` + pdoc `$blob` 引用）经补登调和迁入
//!   blob 层 → 三端 presence → 删除 pdoc（dlog 传播）→ GC 清无引用
//!   blob（负向：被引用期间 GC 必存活）。

use std::collections::HashSet;

use ed25519_dalek::SigningKey;
use serde_json::Value;
use sha2::{Digest, Sha256};
use spark_core::device::{DEVICE_UID_KEY, DeviceRecord};
use spark_core::kernel::{InboundDmResult, PdsyncOut, dm_envelope, handle_inbound_dm};
use spark_core::storage::{MemoryStorage, StorageBackend};
use spark_core::sync::blob::{self, CHUNK_SIZE_BYTES, CHUNK_THRESHOLD_BYTES, ReadOutcome};
use spark_core::sync::pdsync::{self, MessageWindow};

const NOW: i64 = 1_720_000_000_000;

thread_local! {
    /// 单调推进的测试时钟（每投递一个信封 +1s）：`throttle_fetch` 有 30s
    /// 请求节流，全程固定 NOW 会让「弃块后重拉同 chunkCid」被节流停摆。
    /// thread_local 保证并行测试各自独立、单测试内确定推进。
    static CLOCK: std::cell::Cell<i64> = const { std::cell::Cell::new(NOW) };
}

fn tick_now() -> i64 {
    CLOCK.with(|c| {
        let v = c.get() + 1000;
        c.set(v);
        v
    })
}

/// 确定性内容（与 spec/vectors/blob.json 同规则）。
fn pattern(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
}

/// 自设备身份：rootId = sha256hex(签名公钥)（与 dm_envelope 验签口径一致；
/// 三台设备共享同一 root 密钥——同账号自设备各持身份文件）。
fn self_identity(seed: u8) -> (SigningKey, String) {
    let key = SigningKey::from_bytes(&[seed; 32]);
    let root_id = hex::encode(Sha256::digest(key.verifying_key().to_bytes()));
    (key, root_id)
}

/// loopback 中的一台设备：独立存储 + vv 节点 id + 连接层 peerId + deviceUid。
struct Device {
    storage: MemoryStorage,
    node: &'static str,
    peer: &'static str,
    uid: &'static str,
}

fn device_record(peer: &str, uid: &str) -> DeviceRecord {
    DeviceRecord {
        peer_id: peer.to_string(),
        device_uid: Some(uid.to_string()),
        device_name: peer.to_string(),
        os: "Windows".to_string(),
        arch: "x86_64".to_string(),
        macs: Vec::new(),
        app_version: String::new(),
        os_version: String::new(),
        updated_at: NOW,
        last_seen_at: NOW,
        revoked_at: None,
        device_pub_key: None,
    }
}

fn new_device(node: &'static str, peer: &'static str, uid: &'static str, all: &[Device]) -> Device {
    let mut d = Device {
        storage: MemoryStorage::new(),
        node,
        peer,
        uid,
    };
    // 本机 deviceUid 种子（get_or_create_device_uid 命中，确定性）
    d.storage.put(DEVICE_UID_KEY, uid).unwrap();
    // 设备清单（热切续拉的发送方 deviceUid 反查依据）
    for other in all {
        let rec = device_record(other.peer, other.uid);
        d.storage
            .put(
                &format!("device:{}", other.peer),
                &serde_json::to_string(&rec).unwrap(),
            )
            .unwrap();
    }
    d
}

/// 构造并投递一个自设备 dm 信封（from==to==root，验签通过）。
fn deliver(
    d: &mut Device,
    key: &SigningKey,
    root: &str,
    kind: &str,
    body: Value,
    from_peer: &str,
) -> InboundDmResult {
    let now = tick_now();
    let envelope = dm_envelope::build_envelope(kind, root, root, now, body, key);
    handle_inbound_dm(
        &mut d.storage,
        root,
        "我",
        envelope,
        from_peer,
        &HashSet::new(),
        now,
        d.node,
        None,
    )
    .unwrap()
}

/// PdsyncOut → (kind, body)。A4 补登流依赖 P6 附件拉取（pdsync-attachment），
/// Attach* 一并映射。
fn map_outputs(outs: Vec<PdsyncOut>) -> Vec<(&'static str, Value)> {
    outs.into_iter()
        .map(|out| {
            match out {
                PdsyncOut::Push { body } | PdsyncOut::Data { body } => {
                    (dm_envelope::KIND_PDSYNC_DATA, body)
                }
                PdsyncOut::Need { body } => (dm_envelope::KIND_PDSYNC_NEED, body),
                PdsyncOut::BlobFetch { body } => (dm_envelope::KIND_BLOB_FETCH, body),
                PdsyncOut::BlobChunk { body } => (dm_envelope::KIND_BLOB_CHUNK, body),
                PdsyncOut::AttachReq { body } => (dm_envelope::KIND_PDSYNC_ATTACHMENT_REQ, body),
                PdsyncOut::AttachResp { body } => (dm_envelope::KIND_PDSYNC_ATTACHMENT_RESP, body),
            }
        })
        .collect()
}

/// x 向 y 发 hello，并把随后双向往返（need/data/blob-fetch/blob-chunk）
/// 乒乓投递到无后续输出。
fn sync_round(x: &mut Device, y: &mut Device, key: &SigningKey, root: &str) {
    let self_key = pdsync::self_friend_key(root);
    let window = MessageWindow::default_();
    let hello = pdsync::build_hello(
        &x.storage,
        window.max_age_ms,
        window.max_per_conv,
        "eager",
        Some(&self_key),
        None,
    )
    .unwrap();
    let res = deliver(y, key, root, dm_envelope::KIND_PDSYNC_HELLO, hello, x.peer);
    let mut for_x = map_outputs(res.pdsync_out);
    loop {
        if for_x.is_empty() {
            break;
        }
        let mut for_y = Vec::new();
        for (kind, body) in for_x.drain(..) {
            let r = deliver(x, key, root, kind, body, y.peer);
            for_y.extend(map_outputs(r.pdsync_out));
        }
        if for_y.is_empty() {
            break;
        }
        for (kind, body) in for_y.drain(..) {
            let r = deliver(y, key, root, kind, body, x.peer);
            for_x.extend(map_outputs(r.pdsync_out));
        }
    }
}

/// 双向反熵泵：固定轮数驱动 hello→need→data→ack 全链。
///
/// 不以「零流量」为收敛判据：hello 不携带 dlogAck，未全员确认的墓碑（含
/// 接力传播条目）会在每轮 hello 的 Equal 分支重推一轮并触发即时回执
/// （协议既有行为，生产上靠周期 hello 收敛到全员确认后 GC 平息）——
/// 此类流量幂等，5 轮足以让 vv/data 全部收敛；收敛性改由断言存储状态
/// （presence/副本数）来证明。
fn pump(a: &mut Device, b: &mut Device, key: &SigningKey, root: &str) {
    for _ in 0..5 {
        sync_round(a, b, key, root);
        sync_round(b, a, key, root);
    }
}

/// 按需回补泵：req 发出 fetch body → holder 应答 → req 合入；req 的续拉
/// （分片 NeedMore / 热切下一块）继续循环，直到无后续请求。
fn blob_pull(req: &mut Device, holder: &mut Device, key: &SigningKey, root: &str, body: Value) {
    let mut fetch = Some(body);
    let mut rounds = 0;
    while let Some(body) = fetch.take() {
        rounds += 1;
        assert!(rounds < 200, "回补未在有限轮内完成（疑似死循环）");
        let res = deliver(holder, key, root, dm_envelope::KIND_BLOB_FETCH, body, req.peer);
        for (kind, body) in map_outputs(res.pdsync_out) {
            assert_eq!(kind, dm_envelope::KIND_BLOB_CHUNK, "fetch 应答应为 blob-chunk");
            let r = deliver(req, key, root, kind, body, holder.peer);
            let mut followups = map_outputs(r.pdsync_out);
            // 本实现同一时刻至多一个续拉（NeedMore 或热切下一块）
            assert!(followups.len() <= 1, "续拉至多一条");
            if let Some((kind, body)) = followups.pop() {
                assert_eq!(kind, dm_envelope::KIND_BLOB_FETCH);
                fetch = Some(body);
            }
        }
    }
}

/// 在设备上裸写一条 pdoc `$blob` 引用（**本地键，不经 pdsync**——
/// 生产上引用记录随核心数据全量同步到各设备，裸写等价于同步后的本地
/// 落地态；GC 语义：层内 blob 的生命周期挂引用记录，协议 §16.3）。
///
/// 裸写而非 `put_personal` 的原因：测试要分阶段驱动 blob-fetch 回补，
/// 若引用经 pdsync 同步到对端，P6 eager 调和会抢先拉取破坏阶段断言。
fn seed_blob_ref(d: &mut Device, cid: &str) {
    let pdoc_value = serde_json::json!({ "file": { "$blob": cid } }).to_string();
    d.storage
        .put(
            &format!("pdoc:plugin-x:doc-{}", &cid[..8]),
            &pdoc_value,
        )
        .unwrap();
}

/// 在设备上写 blob 并补本地引用（其余设备用 [`seed_blob_ref`] 补）。
fn seed_blob_with_ref(d: &mut Device, data: &[u8]) -> spark_core::sync::blob::BlobManifest {
    let manifest = blob::save_blob(&mut d.storage, d.node, d.uid, data, NOW).unwrap();
    seed_blob_ref(d, &manifest.cid);
    manifest
}

/// 通用设备组构造（先骨架取 peer/uid 种子，再互相预置设备清单）。
fn make_devices(seeds: &[(&'static str, &'static str, &'static str)]) -> Vec<Device> {
    let skeleton: Vec<Device> = seeds
        .iter()
        .map(|(node, peer, uid)| Device {
            storage: MemoryStorage::new(),
            node,
            peer,
            uid,
        })
        .collect();
    seeds
        .iter()
        .map(|(node, peer, uid)| new_device(node, peer, uid, &skeleton))
        .collect()
}

/// 三台设备的标准配置。
fn three_devices() -> (Device, Device, Device) {
    let mut devices = make_devices(&[
        ("node-a", "peer-a", "uid-a"),
        ("node-b", "peer-b", "uid-b"),
        ("node-c", "peer-c", "uid-c"),
    ]);
    let c = devices.pop().unwrap();
    let b = devices.pop().unwrap();
    let a = devices.pop().unwrap();
    (a, b, c)
}

/// 四台设备（K=3 下存在富余副本位，驱逐选择器可触发）。
fn four_devices() -> (Device, Device, Device, Device) {
    let mut devices = make_devices(&[
        ("node-a", "peer-a", "uid-a"),
        ("node-b", "peer-b", "uid-b"),
        ("node-c", "peer-c", "uid-c"),
        ("node-d", "peer-d", "uid-d"),
    ]);
    let d = devices.pop().unwrap();
    let c = devices.pop().unwrap();
    let b = devices.pop().unwrap();
    let a = devices.pop().unwrap();
    (a, b, c, d)
}

#[test]
fn three_device_loopback_refill_after_drop() {
    let (key, root) = self_identity(1);
    let (mut a, mut b, mut c) = three_devices();

    // 1) A 写大 blob：2MiB+1B → 9 块（8×256KiB + 1B）。引用只落在 A：
    // B/C 若持有引用会被本机 P6 eager 调和抢先自拉，破坏分阶段回补
    // 驱动（B/C 的 GC 安全由「600s 虚拟窗口内不二次触发」保证）
    let data = pattern(CHUNK_THRESHOLD_BYTES * 2 + 1);
    let manifest = seed_blob_with_ref(&mut a, &data);
    assert_eq!(manifest.chunk_cids.len(), 9);
    assert_eq!(manifest.chunk_size, CHUNK_SIZE_BYTES as u64);
    for chunk_cid in &manifest.chunk_cids {
        assert!(blob::has_chunk(&a.storage, chunk_cid));
    }
    let (full, per_chunk) = blob::replica_summary(&a.storage, &manifest.cid)
        .unwrap()
        .expect("A 本地可计数");
    assert_eq!((full, per_chunk.iter().min().copied()), (1, Some(1)));

    // 2) presence 经 pdsync 扩散到 B/C（blob:presence category 端到端）
    pump(&mut a, &mut b, &key, &root);
    pump(&mut a, &mut c, &key, &root);
    for d in [&b, &c] {
        let records = blob::list_presence(&d.storage, &manifest.cid).unwrap();
        assert_eq!(records.len(), 1, "对端应见 A 的 presence");
        assert_eq!(records[0].device_uid, "uid-a");
    }

    // 3) B 读：无 manifest → 计划指向 A → 回补 manifest；热切续拉自动补齐全块
    let outcome = blob::read_or_plan(&mut b.storage, &manifest.cid, None, tick_now()).unwrap();
    let plan = match outcome {
        ReadOutcome::NeedFetch(plan) => plan,
        other => panic!("B 应需回补，得 {other:?}"),
    };
    assert_eq!(plan.manifest_from.as_deref(), Some("uid-a"));
    blob_pull(
        &mut b,
        &mut a,
        &key,
        &root,
        blob::build_fetch_body(&blob::FetchTarget::Manifest {
            cid: manifest.cid.clone(),
        }),
    );
    assert_eq!(
        blob::read_blob(&mut b.storage, &manifest.cid, tick_now())
            .unwrap()
            .as_deref(),
        Some(data.as_slice()),
        "B 回补后装配读出与源一致"
    );
    // B 持有即做种：presence 已写（完整位图）；B 账本上 A/B 两条皆完整
    let (full_b, _) = blob::replica_summary(&b.storage, &manifest.cid)
        .unwrap()
        .unwrap();
    assert_eq!(full_b, 2, "B 复算：A（已同步）+ 自己（回补落账）");
    let records = blob::list_presence(&b.storage, &manifest.cid).unwrap();
    assert!(records.iter().any(|r| r.device_uid == "uid-b"));

    // 4) C 同样回补
    blob_pull(
        &mut c,
        &mut a,
        &key,
        &root,
        blob::build_fetch_body(&blob::FetchTarget::Manifest {
            cid: manifest.cid.clone(),
        }),
    );
    assert!(
        blob::read_blob(&mut c.storage, &manifest.cid, tick_now())
            .unwrap()
            .is_some()
    );

    // 5) 全量泵：presence 收敛后三设备复算结果一致（确定性计数）
    pump(&mut a, &mut b, &key, &root);
    pump(&mut a, &mut c, &key, &root);
    pump(&mut b, &mut c, &key, &root);
    for (name, d) in [("A", &a), ("B", &b), ("C", &c)] {
        let (full, per_chunk) = blob::replica_summary(&d.storage, &manifest.cid)
            .unwrap()
            .unwrap();
        assert_eq!(full, 3, "{name} 复算完整副本数 = 3");
        assert!(
            per_chunk.iter().all(|n| *n == 3),
            "{name} 逐块副本数全 3：{per_chunk:?}"
        );
    }

    // 6) B 弃块（模拟配额驱逐：drop + 驱逐标记，§16.4——否则 P6 eager
    // 调和会立刻把引用中的 blob 重拉回来）：chunk 删除、manifest 保留、
    // presence 墓碑传播
    let dropped = blob::drop_local_chunks(&mut b.storage, b.node, b.uid, &manifest.cid, NOW).unwrap();
    blob::mark_evicted(&mut b.storage, &manifest.cid).unwrap();
    assert_eq!(dropped, 9);
    assert!(blob::get_manifest(&b.storage, &manifest.cid).unwrap().is_some());
    pump(&mut b, &mut a, &key, &root);
    pump(&mut b, &mut c, &key, &root);
    for (name, d) in [("A", &a), ("C", &c)] {
        let (full, _) = blob::replica_summary(&d.storage, &manifest.cid)
            .unwrap()
            .unwrap();
        assert_eq!(full, 2, "{name} 复算：B 弃块后完整副本数 = 2");
    }

    // 7) B 再读：A 离线（在线集只含 uid-c）→ 计划全部指向 C（向 C 回补）
    let online = vec!["uid-c".to_string()];
    let outcome = blob::read_or_plan(&mut b.storage, &manifest.cid, Some(&online), tick_now()).unwrap();
    let plan = match outcome {
        ReadOutcome::NeedFetch(plan) => plan,
        other => panic!("B 应需回补，得 {other:?}"),
    };
    assert_eq!(plan.chunks.len(), 9);
    assert!(
        plan.chunks.iter().all(|f| f.holder == "uid-c"),
        "A 离线时全部块应向 C 回补"
    );
    // 首块拉取即触发热切续拉链（C 持有全部块），一次泵补齐
    blob_pull(
        &mut b,
        &mut c,
        &key,
        &root,
        blob::build_fetch_body(&blob::FetchTarget::Chunk {
            chunk_cid: plan.chunks[0].chunk_cid.clone(),
            offset: 0,
        }),
    );
    assert_eq!(
        blob::read_blob(&mut b.storage, &manifest.cid, tick_now())
            .unwrap()
            .as_deref(),
        Some(data.as_slice()),
        "B 驱逐后从 C 回补，字节与源一致"
    );

    // 8) B 的 presence 恢复 → 泵回 A/C → 副本数回到 3
    pump(&mut b, &mut a, &key, &root);
    pump(&mut b, &mut c, &key, &root);
    let (full, _) = blob::replica_summary(&a.storage, &manifest.cid)
        .unwrap()
        .unwrap();
    assert_eq!(full, 3);
}

#[test]
fn honest_degradation_and_retry_when_holder_online() {
    let (key, root) = self_identity(1);
    let (mut a, mut b, mut c) = three_devices();

    // A 独占一个小 blob；B 无任何记录 → 暂不可用（不落假数据）。
    // 引用只落 A（B 持引用会被 P6 eager 抢先自拉，破坏降级语义）
    let data = pattern(1000);
    let manifest = seed_blob_with_ref(&mut a, &data);
    assert_eq!(
        blob::read_or_plan(&mut b.storage, &manifest.cid, None, tick_now()).unwrap(),
        ReadOutcome::Unavailable,
        "无 manifest 无 presence → 诚实降级"
    );

    // presence 到达后：持有者离线仍降级；在线即可规划
    pump(&mut a, &mut b, &key, &root);
    assert_eq!(
        blob::read_or_plan(&mut b.storage, &manifest.cid, Some(&[]), tick_now()).unwrap(),
        ReadOutcome::Unavailable,
        "持有者离线 → 暂不可用"
    );
    let online = vec!["uid-a".to_string()];
    let outcome = blob::read_or_plan(&mut b.storage, &manifest.cid, Some(&online), tick_now()).unwrap();
    assert!(
        matches!(outcome, ReadOutcome::NeedFetch(_)),
        "持有者上线 → 可规划回补"
    );

    // 拉取失败（向不持有的 C 拉）：missing 应答 → 状态不变（仍无 manifest）
    let res = deliver(
        &mut c,
        &key,
        &root,
        dm_envelope::KIND_BLOB_FETCH,
        blob::build_fetch_body(&blob::FetchTarget::Manifest {
            cid: manifest.cid.clone(),
        }),
        b.peer,
    );
    for (kind, body) in map_outputs(res.pdsync_out) {
        let r = deliver(&mut b, &key, &root, kind, body, c.peer);
        assert!(r.pdsync_out.is_empty(), "missing 后无续拉");
    }
    assert!(
        blob::get_manifest(&b.storage, &manifest.cid).unwrap().is_none(),
        "missing 应答不落任何状态"
    );

    // 持有者上线后重规划 → 回补成功（热切续拉补齐单块）
    blob_pull(
        &mut b,
        &mut a,
        &key,
        &root,
        blob::build_fetch_body(&blob::FetchTarget::Manifest {
            cid: manifest.cid.clone(),
        }),
    );
    assert_eq!(
        blob::read_blob(&mut b.storage, &manifest.cid, tick_now())
            .unwrap()
            .as_deref(),
        Some(data.as_slice())
    );
    // 回补成功 → B 成为新副本（瞬时 2 副本，>K 允许——K 收敛是 A2 驱逐的事）
    let records = blob::list_presence(&b.storage, &manifest.cid).unwrap();
    assert!(records.iter().any(|r| r.device_uid == "uid-b"));
}

// ── A2：配额与驱逐（协议 §15）────────────────────────────────────

/// 三设备（K=3 退化全量）：即使配额压到 0，驱逐选择器恒无候选
/// （|H| ≤ K 硬规则在选择器内强制），诚实上报超配额但不动数据。
#[test]
fn three_device_no_eviction_when_k_equals_devices() {
    let (key, root) = self_identity(1);
    let (mut a, mut b, mut c) = three_devices();
    let data = pattern(1000);
    let manifest = seed_blob_with_ref(&mut a, &data);
    seed_blob_ref(&mut b, &manifest.cid);
    seed_blob_ref(&mut c, &manifest.cid);
    pump(&mut a, &mut b, &key, &root);
    pump(&mut a, &mut c, &key, &root);
    blob_pull(
        &mut b,
        &mut a,
        &key,
        &root,
        blob::build_fetch_body(&blob::FetchTarget::Manifest {
            cid: manifest.cid.clone(),
        }),
    );
    blob_pull(
        &mut c,
        &mut a,
        &key,
        &root,
        blob::build_fetch_body(&blob::FetchTarget::Manifest {
            cid: manifest.cid.clone(),
        }),
    );
    pump(&mut b, &mut c, &key, &root);
    // B 配额 0：副本 3 = K → 选择器无候选，数据分毫不动
    blob::set_blob_quota(&mut b.storage, Some(0)).unwrap();
    let report = blob::evict_over_quota(&mut b.storage, b.node, b.uid, tick_now()).unwrap();
    assert!(report.evicted.is_empty(), "K=3 退化全量不驱逐");
    assert!(report.still_over, "超配额如实上报");
    assert_eq!(
        blob::read_blob(&mut b.storage, &manifest.cid, tick_now())
            .unwrap()
            .as_deref(),
        Some(data.as_slice())
    );
}

/// 四设备：K=3，|H|=4 → 位次 3 的 D 是富余副本——配额触发驱逐 →
/// presence 墓碑传播收敛到 3 副本 → D 再读时向幸存持有者回补；
/// 位次 1 的 B 即使超配额也绝不驱逐（保留位硬保护）；
/// 顺带验证 hello `blobQuota` 字段端到端（A 声明 → B 备查）。
#[test]
fn four_device_eviction_and_refill() {
    let (key, root) = self_identity(1);
    let (mut a, mut b, mut c, mut d) = four_devices();

    // A 声明自定义配额（42 GiB），随 hello 传播
    let quota_a: u64 = 42 * 1024 * 1024 * 1024;
    blob::set_blob_quota(&mut a.storage, Some(quota_a)).unwrap();

    // A 写大 blob（>1MiB → 5 块）；各设备补本地引用
    let data = pattern(CHUNK_THRESHOLD_BYTES + 12345);
    let manifest = seed_blob_with_ref(&mut a, &data);
    seed_blob_ref(&mut b, &manifest.cid);
    seed_blob_ref(&mut c, &manifest.cid);
    seed_blob_ref(&mut d, &manifest.cid);
    assert_eq!(manifest.chunk_cids.len(), 5);

    // presence 扩散后 B/C/D 回补，再全网泵到 4 副本收敛
    for x in [&mut b, &mut c, &mut d] {
        pump(&mut a, x, &key, &root);
    }
    for x in [&mut b, &mut c, &mut d] {
        blob_pull(
            x,
            &mut a,
            &key,
            &root,
            blob::build_fetch_body(&blob::FetchTarget::Manifest {
                cid: manifest.cid.clone(),
            }),
        );
    }
    pump(&mut a, &mut b, &key, &root);
    pump(&mut a, &mut c, &key, &root);
    pump(&mut a, &mut d, &key, &root);
    pump(&mut b, &mut c, &key, &root);
    pump(&mut b, &mut d, &key, &root);
    pump(&mut c, &mut d, &key, &root);
    for (name, x) in [("A", &a), ("B", &b), ("C", &c), ("D", &d)] {
        let (full, _) = blob::replica_summary(&x.storage, &manifest.cid)
            .unwrap()
            .unwrap();
        assert_eq!(full, 4, "{name} 复算完整副本数 = 4");
    }
    // hello blobQuota：B 已备查 A 的声明
    let recorded = b
        .storage
        .get(&blob::remote_blob_quota_key(a.peer))
        .unwrap()
        .expect("B 备查 A 的 blobQuota");
    assert_eq!(recorded, quota_a.to_string());

    // D 配额 1 字节 → 驱逐（D 位次 3 ≥ K=3，富余副本）
    blob::set_blob_quota(&mut d.storage, Some(1)).unwrap();
    let report = blob::evict_over_quota(&mut d.storage, d.node, d.uid, tick_now()).unwrap();
    assert_eq!(report.evicted.len(), 1, "D 驱逐唯一 blob");
    assert_eq!(report.evicted[0].0, manifest.cid);
    assert_eq!(report.evicted[0].1, data.len() as u64);
    assert!(!report.still_over);
    assert!(
        blob::get_manifest(&d.storage, &manifest.cid).unwrap().is_some(),
        "驱逐只删 chunk 留 manifest（可随时回补）"
    );
    // presence 墓碑传播 → 各端复算 3 副本
    pump(&mut d, &mut a, &key, &root);
    pump(&mut d, &mut b, &key, &root);
    pump(&mut d, &mut c, &key, &root);
    for (name, x) in [("A", &a), ("B", &b), ("C", &c)] {
        let (full, _) = blob::replica_summary(&x.storage, &manifest.cid)
            .unwrap()
            .unwrap();
        assert_eq!(full, 3, "{name} 复算：D 驱逐后 = 3 = K");
    }

    // B 配额 1 字节：位次 1 < K（保留位）→ 即使超配额也绝不驱逐
    blob::set_blob_quota(&mut b.storage, Some(1)).unwrap();
    let report = blob::evict_over_quota(&mut b.storage, b.node, b.uid, tick_now()).unwrap();
    assert!(report.evicted.is_empty(), "保留位持有者不驱逐（硬规则 2）");
    assert!(report.still_over, "超配额如实上报");
    for chunk_cid in &manifest.chunk_cids {
        assert!(blob::has_chunk(&b.storage, chunk_cid), "B 数据分毫未动");
    }

    // D 恢复配额 → 再读 → 计划指向幸存持有者 → 回补 → 字节一致
    blob::set_blob_quota(&mut d.storage, None).unwrap();
    let online = vec!["uid-a".to_string()];
    let outcome = blob::read_or_plan(&mut d.storage, &manifest.cid, Some(&online), tick_now()).unwrap();
    assert!(matches!(outcome, ReadOutcome::NeedFetch(_)), "D 可规划回补");
    blob_pull(
        &mut d,
        &mut a,
        &key,
        &root,
        blob::build_fetch_body(&blob::FetchTarget::Manifest {
            cid: manifest.cid.clone(),
        }),
    );
    assert_eq!(
        blob::read_blob(&mut d.storage, &manifest.cid, tick_now())
            .unwrap()
            .as_deref(),
        Some(data.as_slice()),
        "D 驱逐后回补，字节与源一致"
    );
    let records = blob::list_presence(&d.storage, &manifest.cid).unwrap();
    assert!(
        records.iter().any(|r| r.device_uid == "uid-d"),
        "D 回补后重新计入 presence"
    );
}

// ── A4：存量附件补登 + 引用收集 + GC 周期（协议 §16）──────────────

/// 存量（P6 `blob:data:` + pdoc `$blob` 引用）经 hello 调和自动迁入
/// blob 层：三端 presence 收敛 3 副本、cid 引用读穿可读、`blob:data:`
/// 清除；删除 pdoc（dlog 传播）后引用消失，GC 清掉全部副本；
/// 负向：被引用期间 GC 必存活。
#[test]
fn three_device_legacy_backfill_then_gc() {
    let (key, root) = self_identity(1);
    let (mut a, mut b, mut c) = three_devices();

    // A 造存量：pdoc 记录（受管写入，随 pdsync 同步）+ P6 blob:data 附件
    let data = pattern(600 * 1024); // 600KiB（单块，分片续拉）
    let info = spark_core::plugindata::blob::save_blob(&mut a.storage, &data).unwrap();
    let cid = info.hash.clone();
    // pdoc 键必须带代际段（`pdoc:{name}@v{version}:{key}`）且集合有
    // `pdecl:` 声明——P6 驻留裁剪对无声明的 pdoc 一律跳过不推（墓碑同
    // 口径），缺声明会导致删除不传播
    let pdoc_key = format!("pdoc:plugin-x:docs@v1:doc-{}", &cid[..8]);
    spark_core::sync::personal::put_personal(
        &mut a.storage,
        a.node,
        "pdecl:plugin-x:docs@v1",
        &serde_json::json!({ "name": "plugin-x:docs", "version": "1" }).to_string(),
        NOW,
    )
    .unwrap();
    let pdoc_value = serde_json::json!({
        "title": "存量附件文档",
        "file": { "$blob": cid, "name": "legacy.bin" },
    })
    .to_string();
    spark_core::sync::personal::put_personal(&mut a.storage, a.node, &pdoc_key, &pdoc_value, NOW)
        .unwrap();

    // 调和链：pdoc 同步到 B/C；B/C（PC eager）经 P6 拉到 blob:data；
    // 各端补登调和迁入 blob 层（A 的服务端在迁移前后都能供——读穿）。
    // pump 固定 5 轮足够「拉取→下轮迁入→presence 回传」走完。
    pump(&mut a, &mut b, &key, &root);
    pump(&mut a, &mut c, &key, &root);
    pump(&mut b, &mut c, &key, &root);
    pump(&mut a, &mut b, &key, &root);
    pump(&mut a, &mut c, &key, &root);

    // 补登完成：三端层内完整 + blob:data 已清 + 3 副本
    for (name, x) in [("A", &a), ("B", &b), ("C", &c)] {
        assert!(
            blob::has_blob_complete(&x.storage, &cid),
            "{name} 层内完整持有"
        );
        assert!(
            x.storage
                .get(&spark_core::plugindata::blob::blob_data_key(&cid))
                .unwrap()
                .is_none(),
            "{name} blob:data 已迁出"
        );
        let (full, _) = blob::replica_summary(&x.storage, &cid).unwrap().unwrap();
        assert_eq!(full, 3, "{name} 复算 3 副本");
        // cid 引用解析可读（读穿：blob:data 已删，命中 blob 层）
        let b64 = spark_core::plugindata::blob::read_blob(&x.storage, &cid)
            .unwrap()
            .expect("cid 引用可读");
        use base64::Engine as _;
        let got = base64::engine::general_purpose::STANDARD.decode(b64).unwrap();
        assert_eq!(got, data, "{name} 读出与源一致");
    }
    // 幂等：稳态下补登调和零处理
    assert_eq!(
        blob::reconcile_registrations(&mut a.storage, a.node, a.uid, tick_now()).unwrap(),
        0,
        "稳态补登幂等"
    );

    // 负向：被 pdoc 引用期间 GC 必存活
    let referenced = blob::collect_references(&b.storage).unwrap();
    assert!(referenced.contains(&cid));
    let cleared = blob::gc_unreferenced(&mut b.storage, b.node, b.uid, &referenced, tick_now()).unwrap();
    assert!(cleared.is_empty(), "被引用 cid GC 必不清");
    assert!(blob::has_blob_complete(&b.storage, &cid));

    // A 删除 pdoc（dlog 传播）→ 三端引用消失
    spark_core::sync::personal::delete_personal(&mut a.storage, a.node, &pdoc_key, tick_now())
        .unwrap();
    assert!(
        !blob::collect_references(&a.storage).unwrap().contains(&cid),
        "A 引用已消失"
    );
    pump(&mut a, &mut b, &key, &root);
    pump(&mut a, &mut c, &key, &root);
    for (name, x) in [("B", &b), ("C", &c)] {
        assert!(
            !blob::collect_references(&x.storage).unwrap().contains(&cid),
            "{name} 引用已消失"
        );
    }

    // 各端 GC（生产由 hello 调和节流触发；测试直调同一代码路径）
    for (x, node, uid) in [
        (&mut a, "node-a", "uid-a"),
        (&mut b, "node-b", "uid-b"),
        (&mut c, "node-c", "uid-c"),
    ] {
        let referenced = blob::collect_references(&x.storage).unwrap();
        let cleared = blob::gc_unreferenced(&mut x.storage, node, uid, &referenced, tick_now()).unwrap();
        assert_eq!(cleared, vec![cid.clone()], "无引用 blob 进 GC 集");
    }
    // presence 墓碑传播
    pump(&mut a, &mut b, &key, &root);
    pump(&mut a, &mut c, &key, &root);
    pump(&mut b, &mut c, &key, &root);
    for (name, x) in [("A", &a), ("B", &b), ("C", &c)] {
        assert!(
            blob::list_local_blobs(&x.storage).unwrap().is_empty(),
            "{name} manifest 已清"
        );
        assert!(
            blob::list_presence(&x.storage, &cid).unwrap().is_empty(),
            "{name} presence 已墓碑化"
        );
        // 记录已删则副本语义随记录生命周期结束：内容不可再读
        assert!(
            spark_core::plugindata::blob::read_blob(&x.storage, &cid)
                .unwrap()
                .is_none(),
            "{name} GC 后不可读"
        );
    }
}

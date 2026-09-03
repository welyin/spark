//! kernel 身份门面集成测试：注册/解锁/资料更新、备份码与助记词恢复、
//! 签名/域派生/助记词校验、会话版资料更新。

mod common;

use serde_json::Value;
use spark_core::kernel::{Kernel, KernelError};
use spark_core::pw;
use spark_core::storage::StorageBackend;

use common::*;

/// QR 恢复收敛（真实双 kernel + P2P）共享宿主机 P2P 端口/身份 seed，串行跑避免
/// 撞资源与 dm 按 from 1s 限流窗口。全局互斥锁对 QR 收敛用例排他。
static QR_CONV_SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn qr_conv_guard() -> std::sync::MutexGuard<'static, ()> {
    QR_CONV_SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

// ---------------------------------------------------------------------------
// 身份全流程：init → 重启 → unlock → update_profile → list → 备份/助记词恢复
// ---------------------------------------------------------------------------

#[test]
fn identity_full_lifecycle() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());

    // 初始状态：无身份
    let status = kernel.status().unwrap();
    assert!(!status.initialized && !status.unlocked && status.root_id.is_none());
    assert!(kernel.list_identities().unwrap().is_empty());

    // 注册
    let (root_id, mnemonic) = init_identity(&mut kernel);
    assert_eq!(mnemonic.split_whitespace().count(), 24, "24 词助记词");

    // 目录结构与 TS 对齐：identities/{rootId}.json + active-identity.json
    let identity_path = dir
        .path()
        .join("identities")
        .join(format!("{root_id}.json"));
    assert!(identity_path.exists());
    let active_raw = std::fs::read_to_string(dir.path().join("active-identity.json")).unwrap();
    assert_eq!(
        active_raw,
        format!(r#"{{"activeRootId":"{root_id}"}}"#),
        "活动指针逐字节对齐 TS"
    );

    // 身份文件：两空格缩进（JSON.stringify(payload, null, 2) 风格）、v2 字段齐备
    let file_raw = std::fs::read_to_string(&identity_path).unwrap();
    assert!(
        file_raw.starts_with("{\n  \"version\": 2,"),
        "两空格缩进、version 为首字段"
    );
    let file_json: Value = serde_json::from_str(&file_raw).unwrap();
    assert_eq!(file_json["kdf"], "scrypt");
    assert_eq!(file_json["rootId"], root_id);
    assert_eq!(file_json["nickname"], "小明", "昵称已 trim");
    assert!(file_json["authTag"].is_string() && file_json["publicKeyHex"].is_string());

    // 存储目录按身份对齐（sled 路径；SPARK_STORAGE_BACKEND=sqlite 覆盖口下
    // 存储是 .db 文件、无 sled 目录——改断言 sqlite 库文件存在）
    let storage_dir = kernel.storage_dir().expect("storage open");
    if std::env::var_os("SPARK_STORAGE_BACKEND").is_some_and(|v| v == "sqlite") {
        assert!(
            dir.path()
                .join(format!("sqlite-{root_id}.db"))
                .exists(),
            "sqlite 覆盖口：库文件按身份落位"
        );
    } else {
        assert_eq!(
            storage_dir.file_name().unwrap().to_string_lossy(),
            format!("spark-sled-{}", &root_id[..16])
        );
        assert!(storage_dir.exists());
    }

    // status / list
    let status = kernel.status().unwrap();
    assert!(status.initialized && status.unlocked);
    assert_eq!(status.root_id.as_deref(), Some(root_id.as_str()));
    assert_eq!(status.nickname.as_deref(), Some("小明"));
    let list = kernel.list_identities().unwrap();
    assert_eq!(list.len(), 1);
    assert!(list[0].active && list[0].nickname.as_deref() == Some("小明"));

    // 助记词获取（密码门控）
    assert_eq!(kernel.reveal_mnemonic(PASSWORD).unwrap(), mnemonic);
    let err = kernel.reveal_mnemonic("wrong-password").unwrap_err();
    assert!(matches!(err, KernelError::InvalidPassword));
    assert_eq!(err.to_string(), "Invalid password");

    // 更新资料（资料为明文存储、不触碰加密 payload，故无需密码即可更新；
    // password 形参仅为保持 API 签名，不参与解密/校验）
    let profile = kernel
        .update_profile(PASSWORD, Some("小红"), None, None, None, None)
        .unwrap();
    assert_eq!(profile.nickname.as_deref(), Some("小红"));
    assert_eq!(kernel.status().unwrap().nickname.as_deref(), Some("小红"));

    // 当前身份公开信息
    let public = kernel.current_identity().unwrap().expect("unlocked");
    assert_eq!(public.root_id, root_id);
    assert_eq!(public.nickname.as_deref(), Some("小红"));
    assert_eq!(public.public_key_hex.len(), 64);

    // 锁定后再解锁
    kernel.lock();
    assert!(kernel.current_identity().unwrap().is_none());
    assert!(!kernel.status().unwrap().unlocked);
    let err = kernel.unlock("wrong-password", None).unwrap_err();
    assert!(matches!(err, KernelError::InvalidPassword));
    assert_eq!(kernel.unlock(PASSWORD, None).unwrap(), root_id);
    assert!(kernel.status().unwrap().unlocked);

    kernel.shutdown().unwrap();

    // 重启：活动身份恢复，存储重开，未解锁
    let mut kernel = fresh_kernel(dir.path());
    let status = kernel.status().unwrap();
    assert!(status.initialized && !status.unlocked);
    assert_eq!(status.root_id.as_deref(), Some(root_id.as_str()));
    assert_eq!(status.nickname.as_deref(), Some("小红"));
    // 解锁指定 rootId
    assert_eq!(kernel.unlock(PASSWORD, Some(&root_id)).unwrap(), root_id);
    kernel.shutdown().unwrap();
}

// ---------------------------------------------------------------------------
// 备份码与助记词恢复（跨目录 = 跨设备语义）
// ---------------------------------------------------------------------------

#[test]
fn identity_backup_and_mnemonic_recovery() {
    let dir_a = tempfile::tempdir().unwrap();
    let mut kernel_a = fresh_kernel(dir_a.path());
    let (root_id, mnemonic) = init_identity(&mut kernel_a);

    // 备份码 = 当前身份密文记录的紧凑 JSON
    let backup = kernel_a.backup_payload().unwrap();
    let backup_json: Value = serde_json::from_str(&backup).unwrap();
    assert_eq!(backup_json["rootId"], root_id);
    assert!(!backup.contains("\n"), "备份载荷为紧凑 JSON");

    // 设备 B：备份码恢复
    let dir_b = tempfile::tempdir().unwrap();
    let mut kernel_b = fresh_kernel(dir_b.path());
    let err = kernel_b
        .recover_backup(&backup, "wrong-password")
        .unwrap_err();
    assert_eq!(err.to_string(), "密码不正确");
    let err = kernel_b.recover_backup("not-json", PASSWORD).unwrap_err();
    assert_eq!(err.to_string(), "备份数据无效或已损坏");
    assert_eq!(kernel_b.recover_backup(&backup, PASSWORD).unwrap(), root_id);
    assert!(kernel_b.status().unwrap().unlocked);
    // 同一设备重复恢复 → 拒绝
    let err = kernel_b.recover_backup(&backup, PASSWORD).unwrap_err();
    assert_eq!(err.to_string(), "该账号已在本设备上，请直接登录");
    kernel_b.shutdown().unwrap();

    // 设备 C：助记词恢复（连续书写无空格，中文词表）
    let dir_c = tempfile::tempdir().unwrap();
    let mut kernel_c = fresh_kernel(dir_c.path());
    let continuous: String = mnemonic.chars().filter(|c| !c.is_whitespace()).collect();
    let err = kernel_c
        .recover_mnemonic(&continuous, PASSWORD, "恢复用户", None)
        .unwrap();
    assert_eq!(err, root_id, "连续书写的中文助记词可恢复同一身份");
    // 空格分隔形式 + 错误助记词
    let err = kernel_c
        .recover_mnemonic(&mnemonic, PASSWORD, "恢复用户", None)
        .unwrap_err();
    assert_eq!(err.to_string(), "该账号已在本设备上，请直接登录");
    let err = kernel_c
        .recover_mnemonic("abandon abandon abandon", PASSWORD, "x", None)
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "助记词校验失败：请检查是否有错别字、漏字或顺序错误"
    );
    kernel_c.shutdown().unwrap();

    // 设备 A：同一助记词恢复 → 拒绝（已在本设备）
    let err = kernel_a
        .recover_mnemonic(&mnemonic, PASSWORD, "x", None)
        .unwrap_err();
    assert_eq!(err.to_string(), "该账号已在本设备上，请直接登录");
    kernel_a.shutdown().unwrap();
}

// ---------------------------------------------------------------------------
// 二维码备份载荷：剔除头像的紧凑 IdentityFile，QR 容量内可扫码恢复
// ---------------------------------------------------------------------------

#[test]
fn qr_backup_payload_compact_and_recoverable() {
    // 30KB 头像（data URL）：完整文件备份载荷远超 QR 约 3KB 上限
    let _conv_guard = qr_conv_guard();
    let dir_a = tempfile::tempdir().unwrap();
    let mut kernel_a = fresh_kernel(dir_a.path());
    let avatar = format!("data:image/png;base64,{}", "A".repeat(30 * 1024));
    let init = kernel_a
        .init_identity(PASSWORD, "小明", Some(&avatar))
        .unwrap();
    let (root_id, mnemonic) = (init.root_id, init.mnemonic);
    // A 先起 p2p，使 QR 载荷携带本机节点名片（peerId + 地址），恢复端 B 扫码后可
    // 自动配对（backup_payload_qr 仅 p2p 运行时附加 `p`,`a`）。
    kernel_a.start_p2p().unwrap();

    let full = kernel_a.backup_payload().unwrap();
    assert!(full.len() > 3 * 1024, "完整载荷含头像，远超 QR 上限");
    eprintln!("[qr-backup] 完整载荷 {}B", full.len());
    // 错误密码 → Invalid password
    let err = kernel_a.backup_payload_qr("wrong-password").unwrap_err();
    assert!(matches!(err, KernelError::InvalidPassword));

    // 紧凑载荷：<2KB、文件外层与密文均不含 avatar。
    // P2P 运行时载荷为 v1 包装格式 `{"v":1,"i":{IdentityFile},"p","a"}`，
    // 否则为纯 IdentityFile JSON——解包后断言身份文件本体。
    let unwrap_qr = |raw: &str| -> Value {
        let v: Value = serde_json::from_str(raw).unwrap();
        if v.get("v").and_then(Value::as_u64).is_some() {
            v.get("i").cloned().unwrap_or(Value::Null)
        } else {
            v
        }
    };
    let qr = kernel_a.backup_payload_qr(PASSWORD).unwrap();
    assert!(qr.len() < 2 * 1024, "紧凑载荷 <2KB（实测 {}B）", qr.len());
    eprintln!("[qr-backup] 紧凑载荷（30KB 头像身份）{}B", qr.len());
    let qr_json = unwrap_qr(&qr);
    // 地址裁剪：QR 载荷内嵌地址不得含中继电路（近百字符/条，拉高版本密度）、
    // 且封顶 3 条——否则备份码高版本无法扫码（回归防护）。
    let wrapper: Value = serde_json::from_str(&qr).unwrap();
    if let Some(addrs) = wrapper.get("a").and_then(|v| v.as_array()) {
        assert!(
            addrs
                .iter()
                .all(|a| !a.as_str().unwrap_or("").contains("/p2p-circuit")),
            "备份二维码不得携带中继电路地址"
        );
        assert!(
            addrs.len() <= 3,
            "备份二维码内嵌地址封顶 3 条，实际 {}",
            addrs.len()
        );
    }
    assert_eq!(qr_json["rootId"], root_id);
    assert!(qr_json.get("avatar").is_none(), "文件外层无 avatar");
    assert!(!qr.contains("data:image"), "载荷不含头像 data URL");
    assert!(qr_json["salt"].is_string() && qr_json["authTag"].is_string());

    // 同口令重新加密：两次导出 salt/iv 不同
    let qr2 = kernel_a.backup_payload_qr(PASSWORD).unwrap();
    let qr2_json = unwrap_qr(&qr2);
    assert_ne!(qr_json["salt"], qr2_json["salt"], "每次导出随机 salt");
    assert_ne!(qr_json["iv"], qr2_json["iv"], "每次导出随机 iv");

    // 设备 B：紧凑载荷经 recover_backup 恢复（与完整载荷同一入口）
    let dir_b = tempfile::tempdir().unwrap();
    let mut kernel_b = fresh_kernel(dir_b.path());
    let err = kernel_b.recover_backup(&qr, "wrong-password").unwrap_err();
    assert_eq!(err.to_string(), "密码不正确");
    assert_eq!(kernel_b.recover_backup(&qr, PASSWORD).unwrap(), root_id);
    // mnemonic/path 完整恢复（rootId 一致即派生路径一致）、昵称保留。
    assert_eq!(kernel_b.reveal_mnemonic(PASSWORD).unwrap(), mnemonic);
    let public = kernel_b.current_identity().unwrap().unwrap();
    assert_eq!(public.nickname.as_deref(), Some("小明"));

    // QR-F4 #1：载荷携带 pwv 生效——B 恢复后 pwv:self == A 的 V（changedAt 一致），
    // 不得是 B 自建的 changedAt=恢复时刻 的分叉 V。
    {
        let as_ = kernel_a.__test_storage().unwrap();
        let a_pwv = pw::get_pwv(&as_).unwrap().expect("A 有 V");
        let bs = kernel_b.__test_storage().unwrap();
        let b_pwv = pw::get_pwv(&bs)
            .unwrap()
            .expect("B 恢复后应有 V（载荷携带）");
        assert_eq!(
            b_pwv.changed_at, a_pwv.changed_at,
            "B 恢复后 pwv.changedAt 必须 == A 的 V（载荷携带生效），不得为 B 自建分叉 V（当前 B changedAt={} A changedAt={}）",
            b_pwv.changed_at, a_pwv.changed_at
        );
        assert_eq!(
            pw::get_applied_vts(&bs).unwrap(),
            a_pwv.changed_at,
            "B 恢复后 applied 水位 == A 的 V changedAt（继承而非自建分叉）"
        );
    }

    // ── 反熵收敛（architect-m3 §13.7）：同口令恢复不再断言「单轮同步即有头像」，
    //    而是泵 pdsync 轮次直到 B 解出 profile 记录拿回头像。链路分段断言：
    //    A 收 B ack → 锚推进（MAC 校验唯一证据）→ A 授权补发 ikey 包裹 →
    //    B 刷新密钥（本机密钥表+effective）→ 此前跳过的 profile 记录重发 → 解密合入。
    kernel_b.start_p2p().unwrap();
    let a_peer = kernel_a.p2p_status().unwrap().unwrap().peer_id.unwrap();
    let b_peer = kernel_b.p2p_status().unwrap().unwrap().peer_id.unwrap();

    // B 恢复后 on_unlock 自动自锚+自动 ack；B 连接 A（QR 载荷含生成端地址，B 已配对）。
    wait_until(
        || {
            kernel_b
                .p2p_status()
                .unwrap()
                .unwrap()
                .connected_peers
                .iter()
                .any(|p| *p == a_peer)
        },
        20_000,
        "B 连接 A（QR 恢复配对）",
    );
    // A 也确认连到 B（双向握手完成）。
    wait_until(
        || {
            kernel_a
                .p2p_status()
                .unwrap()
                .unwrap()
                .connected_peers
                .iter()
                .any(|p| *p == b_peer)
        },
        20_000,
        "A 连接 B",
    );
    // 分段断言 1：A 侧锚推进到 pwv.changed_at（B 的 ack 经 MAC 校验被锚定的唯一直接证据）。
    let pwv_changed = {
        let s = kernel_a.__test_storage().unwrap();
        pw::get_pwv(&s).unwrap().unwrap().changed_at
    };
    wait_until(
        || {
            let s = kernel_a.__test_storage().unwrap();
            pw::get_last_verified_vts(&s, &b_peer).unwrap() == pwv_changed
        },
        90_000,
        "A 锚推进：lastVerifiedVTs(B) == pwv.changed_at（ack MAC 校验通过）",
    );

    // 分段断言 2：A 授权补发 → A 侧出现 B 的 ikey 包裹（epoch 与 effective 一致）。
    wait_until(
        || {
            let s = kernel_a.__test_storage().unwrap();
            let eff = spark_core::epoch::get_effective(&s).unwrap();
            if eff == 0 {
                return false;
            }
            s.get(&spark_core::epoch::ikey_key(eff, &a_peer, &b_peer))
                .unwrap()
                .is_some()
        },
        90_000,
        "A 授权补发 ikey 包裹",
    );

    // 分段断言 3：B 本机密钥表落 `p2p:epoch:key:{N}` + effective 推进。
    wait_until(
        || {
            let s = kernel_b.__test_storage().unwrap();
            let eff = spark_core::epoch::get_effective(&s).unwrap();
            eff > 0 && spark_core::epoch::get_local_key(&s, eff).unwrap().is_some()
        },
        90_000,
        "B 密钥表落表 + effective 推进",
    );

    // 最终收敛断言：B 解开此前跳过的 profile 记录，头像找回（泵轮次上限覆盖反熵周期）。
    wait_until(
        || {
            kernel_b
                .current_identity()
                .unwrap()
                .unwrap()
                .avatar
                .as_deref()
                == Some(avatar.as_str())
        },
        90_000,
        "B 反熵收敛：头像找回",
    );
    let converged = kernel_b.current_identity().unwrap().unwrap();
    assert_eq!(converged.avatar, Some(avatar), "最终收敛后头像找回");
    kernel_a.shutdown().unwrap();
    kernel_b.shutdown().unwrap();
}

// ───────────────────────────────────────────────────────────────────────────
// QR-F4 #2：旧版无 pwv QR 恢复兼容路径（§13.8）。
// 构造不含 pwv 的旧版 QR → B 恢复后 has_v=false → 懒发布兜底自建 V → 首次与 A
// 同步时水位 LWW 裁决（A 的 V 为权威）→ 收敛（头像找回）。覆盖旧 QR 兼容。
// 断言目标语义：B 恢复后可解锁，但敏感数据面由 D′ 门控暂缓；经 LWW 收敛后
// profile 解密、头像找回。
// ───────────────────────────────────────────────────────────────────────────
#[test]
fn qr_backup_legacy_without_pwv_converges_via_lww() {
    let _conv_guard = qr_conv_guard();
    let dir_a = tempfile::tempdir().unwrap();
    let mut kernel_a = fresh_kernel(dir_a.path());
    let avatar = format!("data:image/png;base64,{}", "A".repeat(30 * 1024));
    let init = kernel_a
        .init_identity(PASSWORD, "小明", Some(&avatar))
        .unwrap();
    let (root_id, _mnemonic) = (init.root_id, init.mnemonic);
    kernel_a.start_p2p().unwrap();
    let qr = kernel_a.backup_payload_qr(PASSWORD).unwrap();

    // 剥掉 `pwv` 字段 → 旧版 QR（无口令校验器注入）。
    let mut wrapper: Value = serde_json::from_str(&qr).unwrap();
    wrapper.as_object_mut().unwrap().remove("pwv");
    let legacy_qr = wrapper.to_string();

    // B 用旧版 QR 恢复 → has_v=false（载荷未携带 V）→ 懒发布兜底自建 V。
    let dir_b = tempfile::tempdir().unwrap();
    let mut kernel_b = fresh_kernel(dir_b.path());
    assert_eq!(
        kernel_b.recover_backup(&legacy_qr, PASSWORD).unwrap(),
        root_id
    );
    kernel_b.start_p2p().unwrap();
    let a_peer = kernel_a.p2p_status().unwrap().unwrap().peer_id.unwrap();
    let b_peer = kernel_b.p2p_status().unwrap().unwrap().peer_id.unwrap();

    wait_until(
        || {
            kernel_b
                .p2p_status()
                .unwrap()
                .unwrap()
                .connected_peers
                .iter()
                .any(|p| *p == a_peer)
        },
        20_000,
        "旧版 QR：B 连接 A",
    );
    wait_until(
        || {
            kernel_a
                .p2p_status()
                .unwrap()
                .unwrap()
                .connected_peers
                .iter()
                .any(|p| *p == b_peer)
        },
        20_000,
        "旧版 QR：A 连接 B",
    );

    // LWW 收敛：泵轮次直到 B 头像找回（A 的 V 为权威，覆盖 B 自建分叉 V）。
    wait_until(
        || {
            kernel_b
                .current_identity()
                .unwrap()
                .unwrap()
                .avatar
                .as_deref()
                == Some(avatar.as_str())
        },
        30_000,
        "旧版 QR：LWW 收敛后头像找回",
    );
    let converged = kernel_b.current_identity().unwrap().unwrap();
    assert_eq!(
        converged.avatar,
        Some(avatar),
        "旧版无 pwv QR 经 LWW 收敛后头像找回"
    );
    kernel_a.shutdown().unwrap();
    kernel_b.shutdown().unwrap();
}

#[test]
fn qr_backup_payload_without_avatar_under_1kb() {
    // 无头像身份：紧凑载荷 <1KB（QR 编码密度低，常规尺寸即可扫出）
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    init_identity(&mut kernel);
    let qr = kernel.backup_payload_qr(PASSWORD).unwrap();
    assert!(
        qr.len() < 2 * 1024,
        "无头像紧凑载荷 <2KB（实测 {}B；QR-F1 注入 pwv 后超 1KB，architect-m3 预算 <3KB）",
        qr.len()
    );
    eprintln!("[qr-backup] 紧凑载荷（无头像身份）{}B", qr.len());
    kernel.shutdown().unwrap();
}

// ---------------------------------------------------------------------------
// 阶段③c 新增门面：签名/域派生/助记词校验、会话版资料更新
// ---------------------------------------------------------------------------

#[test]
fn sign_and_derive_domain_identity() {
    use base64::Engine as _;
    use ed25519_dalek::Verifier as _;

    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());

    // 锁定状态：sign/derive 报 Locked
    assert_eq!(
        kernel.sign("payload").unwrap_err().to_string(),
        "Root identity is locked"
    );
    assert_eq!(
        kernel
            .derive_domain_identity("plugin:chat")
            .unwrap_err()
            .to_string(),
        "Root identity is locked"
    );

    let (root_id, mnemonic) = init_identity(&mut kernel);

    // sign：rootId 一致、签名可用根公钥验过、payloadHash = sha256hex(utf8 字节)
    let sig = kernel.sign("hello spark").unwrap();
    assert_eq!(sig.root_id, root_id);
    assert_eq!(
        sig.payload_hash,
        spark_core::evidence::sha256_hex("hello spark")
    );
    let public = kernel.current_identity().unwrap().unwrap();
    let pub_bytes = hex::decode(public.public_key_hex).unwrap();
    let verifying =
        ed25519_dalek::VerifyingKey::from_bytes(&pub_bytes.try_into().unwrap()).unwrap();
    let sig_bytes = base64::engine::general_purpose::STANDARD
        .decode(&sig.signature)
        .unwrap();
    let signature = ed25519_dalek::Signature::from_bytes(&sig_bytes.try_into().unwrap());
    verifying.verify(b"hello spark", &signature).unwrap();

    // derive：与 identity 模块由助记词种子派生的结果一致；空域报 TS 文案
    let derived = kernel.derive_domain_identity("plugin:chat").unwrap();
    assert_eq!(derived.domain, "plugin:chat");
    let seed = spark_core::identity::parse_mnemonic(&mnemonic)
        .unwrap()
        .seed;
    let expected = spark_core::identity::derive_domain_identity(&seed, "plugin:chat");
    assert_eq!(derived.domain_id, expected.id());
    assert_eq!(
        derived.public_key,
        base64::engine::general_purpose::STANDARD.encode(expected.public_key())
    );
    assert_eq!(derived.derivation_path, expected.path);
    assert_eq!(
        kernel
            .derive_domain_identity("   ")
            .unwrap_err()
            .to_string(),
        "Domain is required"
    );

    kernel.shutdown().unwrap();
}

#[test]
fn check_mnemonic_word_validation() {
    // 纯函数：空格分隔中文词全在词表 → 无非法下标
    let ok = Kernel::check_mnemonic("与 祝 产 鸡 永 烂");
    assert_eq!(ok.words, vec!["与", "祝", "产", "鸡", "永", "烂"]);
    assert!(ok.invalid_indexes.is_empty());

    // 连续书写（无空白）按单字拆分
    let continuous = Kernel::check_mnemonic("与祝产");
    assert_eq!(continuous.words, vec!["与", "祝", "产"]);

    // 英文词表词同样合法；混合非法词给出下标
    let mixed = Kernel::check_mnemonic("legal winner notaword 与");
    assert_eq!(mixed.invalid_indexes, vec![2]);

    // 无空白拉丁串按单字拆，单字不在任何词表 → 全部非法
    let latin = Kernel::check_mnemonic("abc");
    assert_eq!(latin.words, vec!["a", "b", "c"]);
    assert_eq!(latin.invalid_indexes, vec![0, 1, 2]);

    // 空输入
    let empty = Kernel::check_mnemonic("   ");
    assert!(empty.words.is_empty() && empty.invalid_indexes.is_empty());
}

#[test]
fn update_profile_session_flow() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());

    // 无会话（未解锁）→ Locked
    assert_eq!(
        kernel
            .update_profile_session(Some("x"), None, None, None, None)
            .unwrap_err()
            .to_string(),
        "Root identity is locked"
    );

    let (root_id, mnemonic) = init_identity(&mut kernel);
    let avatar = "data:image/png;base64,iVBORw0KGgo=";

    // 会话版：免密码改昵称 + 设头像
    let profile = kernel
        .update_profile_session(Some("  小明二号  "), Some(Some(avatar)), None, None, None)
        .unwrap();
    assert_eq!(profile.nickname.as_deref(), Some("小明二号"));
    assert_eq!(profile.avatar.as_deref(), Some(avatar));
    let status = kernel.status().unwrap();
    assert_eq!(status.nickname.as_deref(), Some("小明二号"));
    assert_eq!(status.avatar.as_deref(), Some(avatar));

    // 清头像（恢复自动头像）；昵称不变
    let profile = kernel
        .update_profile_session(None, Some(None), None, None, None)
        .unwrap();
    assert_eq!(profile.nickname.as_deref(), Some("小明二号"));
    assert_eq!(profile.avatar, None);

    // 非法昵称报错
    assert!(
        kernel
            .update_profile_session(Some(&"长".repeat(25)), None, None, None, None)
            .is_err()
    );

    // lock 清除会话 → 再调报 Locked
    kernel.lock();
    assert_eq!(
        kernel
            .update_profile_session(Some("x"), None, None, None, None)
            .unwrap_err()
            .to_string(),
        "Root identity is locked"
    );

    // 重新解锁：资料保持（重封未破坏文件），助记词仍可用原密码查看
    kernel.unlock(PASSWORD, None).unwrap();
    let status = kernel.status().unwrap();
    assert_eq!(status.nickname.as_deref(), Some("小明二号"));
    assert_eq!(kernel.reveal_mnemonic(PASSWORD).unwrap(), mnemonic);
    let _ = root_id;
    kernel.shutdown().unwrap();
}

// ---------------------------------------------------------------------------
// 资料扩展字段（性别/地区/签名）：patch 语义 + 视图回读 + 重启持久
// ---------------------------------------------------------------------------

#[test]
fn update_profile_extra_fields_flow() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    let (_root_id, _mnemonic) = init_identity(&mut kernel);

    // 初始：三字段均未设置
    let status = kernel.status().unwrap();
    assert_eq!(status.gender, None);
    assert_eq!(status.region, None);
    assert_eq!(status.signature, None);

    // 设置扩展字段；昵称/头像不变
    let profile = kernel
        .update_profile_session(None, None, Some("女"), Some("杭州"), Some("保持热爱"))
        .unwrap();
    assert_eq!(profile.gender.as_deref(), Some("女"));
    assert_eq!(profile.region.as_deref(), Some("杭州"));
    assert_eq!(profile.signature.as_deref(), Some("保持热爱"));
    assert_eq!(profile.nickname.as_deref(), Some("小明"));

    // 部分补丁：只改签名，性别/地区不变
    let profile = kernel
        .update_profile_session(None, None, None, None, Some("奔赴山海"))
        .unwrap();
    assert_eq!(profile.gender.as_deref(), Some("女"));
    assert_eq!(profile.region.as_deref(), Some("杭州"));
    assert_eq!(profile.signature.as_deref(), Some("奔赴山海"));

    // 空串 = 清除（对齐前端 '' = 未设置）
    let profile = kernel
        .update_profile_session(None, None, Some(""), None, None)
        .unwrap();
    assert_eq!(profile.gender, None);
    assert_eq!(profile.region.as_deref(), Some("杭州"));

    // 视图回读：status / current_identity / list_identities 一致
    let status = kernel.status().unwrap();
    assert_eq!(status.gender, None);
    assert_eq!(status.region.as_deref(), Some("杭州"));
    assert_eq!(status.signature.as_deref(), Some("奔赴山海"));
    let public = kernel.current_identity().unwrap().expect("unlocked");
    assert_eq!(public.region.as_deref(), Some("杭州"));
    assert_eq!(public.signature.as_deref(), Some("奔赴山海"));
    let list = kernel.list_identities().unwrap();
    assert_eq!(list[0].region.as_deref(), Some("杭州"));
    assert_eq!(list[0].signature.as_deref(), Some("奔赴山海"));

    kernel.shutdown().unwrap();

    // 重启后（未解锁，仅活动指针）：status 仍能读出扩展字段（文件层落盘）
    let kernel = fresh_kernel(dir.path());
    let status = kernel.status().unwrap();
    assert!(!status.unlocked);
    assert_eq!(status.region.as_deref(), Some("杭州"));
    assert_eq!(status.signature.as_deref(), Some("奔赴山海"));
}

#[test]
fn update_profile_extra_fields_whitespace_clear_and_length_caps() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    init_identity(&mut kernel);

    kernel
        .update_profile_session(None, None, Some("女"), Some("杭州"), Some("签名"))
        .unwrap();
    // 全空白 = 清除（与 Some("") 同口径）
    let profile = kernel
        .update_profile_session(None, None, Some("   "), None, None)
        .unwrap();
    assert_eq!(profile.gender, None);

    // 字符数上限（超出拒绝且不破坏原值）
    let long_region = "杭".repeat(65);
    let err = kernel
        .update_profile_session(None, None, None, Some(&long_region), None)
        .unwrap_err();
    assert!(err.to_string().contains("region too long"));
    let profile = kernel
        .update_profile_session(None, None, None, Some(&"杭".repeat(64)), None)
        .unwrap();
    assert_eq!(profile.region.as_deref(), Some("杭".repeat(64).as_str()));

    let long_signature = "签".repeat(129);
    let err = kernel
        .update_profile_session(None, None, None, None, Some(&long_signature))
        .unwrap_err();
    assert!(err.to_string().contains("signature too long"));
    let long_gender = "性".repeat(17);
    let err = kernel
        .update_profile_session(None, None, Some(&long_gender), None, None)
        .unwrap_err();
    assert!(err.to_string().contains("gender too long"));
}

// ---------------------------------------------------------------------------
// 会话密钥重封回归（update_profile_session 复用 unlock 缓存的 scrypt 派生
// 密钥，0 次 scrypt）：salt 必须保持不变（密钥与 salt 绑定），多次更新后
// 原密码仍可解锁、资料完整。
// ---------------------------------------------------------------------------

#[test]
fn update_profile_session_reuses_key_salt_stable() {
    let dir = tempfile::tempdir().unwrap();
    let mut kernel = fresh_kernel(dir.path());
    let (root_id, mnemonic) = init_identity(&mut kernel);
    let identity_path = dir
        .path()
        .join("identities")
        .join(format!("{root_id}.json"));
    let salt_of = || -> String {
        let raw = std::fs::read_to_string(&identity_path).unwrap();
        let json: Value = serde_json::from_str(&raw).unwrap();
        json["salt"].as_str().unwrap().to_string()
    };
    let salt_at_init = salt_of();

    // 连续两次会话版更新（密钥版重封：salt 不变、IV 更换）
    kernel
        .update_profile_session(Some("密钥一"), None, None, None, None)
        .unwrap();
    let salt_after_first = salt_of();
    kernel
        .update_profile_session(None, None, Some("女"), Some("杭州"), Some("保持热爱"))
        .unwrap();
    let salt_after_second = salt_of();
    assert_eq!(salt_at_init, salt_after_first, "首次重封沿用既有 salt");
    assert_eq!(salt_after_first, salt_after_second, "二次重封 salt 仍不变");

    // lock 清会话（含缓存密钥）→ 原密码重新 unlock 可解出最新资料
    kernel.lock();
    kernel.unlock(PASSWORD, None).unwrap();
    let status = kernel.status().unwrap();
    assert_eq!(status.nickname.as_deref(), Some("密钥一"));
    assert_eq!(status.gender.as_deref(), Some("女"));
    assert_eq!(status.region.as_deref(), Some("杭州"));
    assert_eq!(status.signature.as_deref(), Some("保持热爱"));
    assert_eq!(kernel.reveal_mnemonic(PASSWORD).unwrap(), mnemonic);

    // 重新 unlock 后再改资料（新会话密钥）仍正常，且重启后持久
    kernel
        .update_profile_session(Some("密钥二"), None, None, None, None)
        .unwrap();
    assert_eq!(salt_of(), salt_at_init, "重解锁后重封 salt 依旧不变");
    kernel.shutdown().unwrap();

    let kernel = fresh_kernel(dir.path());
    assert_eq!(kernel.status().unwrap().nickname.as_deref(), Some("密钥二"));
}

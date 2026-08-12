//! dm_e2e 服务：会话密钥表管理 + 加解密门面。
//!
//! 对齐 p2p-dm §19.1.1 与 social-feed §4.1。本文件纯逻辑（存储泛型），不触碰
//! p2p/信封装配；会话密钥表写经 `put_personal` 携带 pmeta（pdsync `dm:e2e`
//! category）扩散到自设备。
//!
//! 密钥派生原语（X25519 DH + HKDF + 临时密钥对）在 [`super::derive`]，
//! AES-256-GCM 加解密原语（显式密钥）在 [`super::crypto`]；本文件通过
//! re-export 组合成密钥表管理 + `encrypt_outbound_body` /
//! `encrypt_body` / `decrypt_body` 门面。
//!
//! ## 会话密钥方向无关（2026-08-11 架构师裁决）
//!
//! HKDF info 用**排序后**的 from/to（字典序小在前）拼接，A→B 与 B→A 共用
//! 同一份会话密钥（密钥表每对 peer 一条记录）。feed 与 chat 通道共用这份
//! 方向无关的会话密钥。

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use ed25519_dalek::SigningKey;
use serde_json::Value;

use super::crypto::{decrypt_body_with_key, encrypt_body_with_key};
use super::derive::{
    derive_session_key, derive_session_key_ephemeral, generate_ephemeral_keypair,
};
use super::types::{E2E_KEY_PREFIX, SessionKeyRecord};
use crate::storage::StorageBackend;
use crate::sync::orgsync::ed_pk_to_x25519;
use crate::sync::put_personal;

/// dm_e2e 模块错误。
#[derive(Debug, thiserror::Error)]
pub enum DmE2eError {
    /// 会话密钥表无对应 peer 记录（加密/解密前置未协商）。
    #[error("no session key for peer")]
    NoSessionKey,
    /// 该 ts 时段无可用历史密钥（早于已淘汰的最旧密钥）。
    #[error("no session key covers ts {0}")]
    NoKeyForTs(i64),
    /// X25519 共享密钥落低阶点（全零）——防强制 DH 落可预测共享。
    #[error("low-order X25519 shared secret rejected")]
    LowOrderShared,
    /// AES-256-GCM 加解密失败（AAD/密钥/nonce 不匹配或数据被篡改）。
    #[error("aead failure: {0}")]
    Aead(String),
    /// 密文对象线形非法或 base64 解码失败。
    #[error("invalid ciphertext: {0}")]
    InvalidCiphertext(String),
    /// 解出的明文字节非合法 UTF-8 / JSON。
    #[error("invalid plaintext: {0}")]
    InvalidPlaintext(String),
    /// JSON 序列化/反序列化错误。
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    /// 存储后端错误。
    #[error(transparent)]
    Storage(#[from] crate::storage::StorageError),
    /// 同步写入（put_personal）错误。
    #[error(transparent)]
    Sync(#[from] crate::sync::SyncError),
}

/// dm_e2e 模块 Result 别名。
pub type Result<T> = std::result::Result<T, DmE2eError>;

/// 会话密钥表键 `dm:e2e:key:{peerRootId}`。
pub fn e2e_key_key(peer_root_id: &str) -> String {
    format!("{E2E_KEY_PREFIX}{peer_root_id}")
}

/// 读取 peer 的会话密钥表记录（缺失/损坏 → `None`）。
pub fn read_session_key_record<S: StorageBackend>(
    storage: &S,
    peer_root_id: &str,
) -> Result<Option<SessionKeyRecord>> {
    let Some(raw) = storage.get(&e2e_key_key(peer_root_id))? else {
        return Ok(None);
    };
    Ok(serde_json::from_str(&raw).ok())
}

/// 写 peer 的会话密钥表记录（经 `put_personal` 携带 pmeta，供 pdsync 自设备
/// 扩散到同一 rootId 的其它设备）。
pub fn write_session_key_record<S: StorageBackend>(
    storage: &mut S,
    peer_root_id: &str,
    record: &SessionKeyRecord,
    node_id: &str,
    now_ms: i64,
) -> Result<()> {
    let key = e2e_key_key(peer_root_id);
    put_personal(
        storage,
        node_id,
        &key,
        &serde_json::to_string(record)?,
        now_ms,
    )?;
    Ok(())
}

/// 入站验签通过后记录对端 root 公钥（`peerRootPub`，2026-08-11 架构师裁决：
/// E2E 由域身份派生改为 root 密钥直接转换，对端 root 公钥来源=信封 `pubKey`）。
///
/// 记录到 `dm:e2e:key:{peer_root_id}` 的 `peerRootPub` 字段，供本方**出站**
/// E2E 加密时读取对端 root 公钥做 X25519 转换。**仅当与已存值不同才写盘**
/// （避免每次通讯都写盘）；记录不存在时创建仅含对端公钥的占位记录
/// （`currentKey` 空，待出站 `ensure_session_key` 补全会话密钥）。
///
/// 好友关系建立必经 friend-request/friend-accept 信封交换（信封带 pubKey），
/// 故正常路径入站积累后出站必有对端 root 公钥。
pub fn record_inbound_peer_root_pub<S: StorageBackend>(
    storage: &mut S,
    peer_root_id: &str,
    peer_root_pub_b64: &str,
    node_id: &str,
    now_ms: i64,
) -> Result<()> {
    let mut record = read_session_key_record(storage, peer_root_id)?.unwrap_or(SessionKeyRecord {
        current_key: String::new(),
        current_since: now_ms,
        last_seen: now_ms,
        history_keys: Vec::new(),
        peer_root_pub: None,
    });
    // 已存值相同：不写盘（避免每次通讯都写）
    if record.peer_root_pub.as_deref() == Some(peer_root_pub_b64) {
        return Ok(());
    }
    record.peer_root_pub = Some(peer_root_pub_b64.to_string());
    write_session_key_record(storage, peer_root_id, &record, node_id, now_ms)?;
    Ok(())
}

/// 按 ts 选取解密密钥：`ts >= currentSince` 用 current；否则从历史密钥中选
/// `since <= ts` 且 since 最大的一段（覆盖该时段的密钥）。无匹配 → `None`
/// （该时段密钥已被淘汰，离线密文不可解）。
pub fn select_key_for_ts<'a>(record: &'a SessionKeyRecord, ts: i64) -> Option<&'a str> {
    if ts >= record.current_since {
        return Some(&record.current_key);
    }
    record
        .history_keys
        .iter()
        .filter(|h| h.since <= ts)
        .max_by_key(|h| h.since)
        .map(|h| h.key.as_str())
}

/// 确保 peer 的会话密钥就绪：协商一次后**原样复用**，不做时间判定、不重协商、
/// 不产生同值 history 条目（p2p-dm §19.1.1「长期会话密钥恒等不轮换」）。
///
/// 长期会话密钥由 root DH 派生、确定性恒等——重复重跑派生只得到同一份密钥，
/// 旧 current 移入 history 只是同值堆积、无安全增益。故已存在 current 密钥时
/// 直接返回复用，**不写盘**（避免空转轮换反复写盘扩散）；前向保密由
/// per-message 临时密钥（信封 `ephPub`）承担，与密钥表轮换无关。
///
/// 新密钥经 [`derive_session_key`]（root 密钥直接转换）派生——本函数是密钥表
/// 管理原语，临时交换派生由 [`derive_session_key_ephemeral`] /
/// [`derive_session_key_from_eph_pub`] 提供（per-message 前向保密，不回写密钥
/// 表，见 mod.rs 接线契约）。
///
/// `my_signing_key` 为本机 **root** 签名私钥；`peer_x25519_pub` 为对端
/// **root 公钥**的 X25519 形式（接线层从密钥表 `peer_root_pub` 读取转换）；
/// `peer_root_pub_b64` 为对端 root 公钥 base64（写入记录的 `peerRootPub`
/// 字段）。既有记录的 `peerRootPub` 被本入参值覆盖（接线层每次传当前已知值；
/// 占位记录即由此补全会话密钥）。
///
/// 返回 `true` 表示发生了首次协商/补全写盘，`false` 表示复用既有密钥（不写盘）。
pub fn ensure_session_key<S: StorageBackend>(
    storage: &mut S,
    my_signing_key: &SigningKey,
    peer_x25519_pub: &[u8; 32],
    peer_root_pub_b64: &str,
    from: &str,
    to: &str,
    peer_root_id: &str,
    node_id: &str,
    now_ms: i64,
) -> Result<bool> {
    let existing = read_session_key_record(storage, peer_root_id)?;
    match existing {
        // 无记录 → 协商（首次），写盘
        None => {
            let new_key = derive_session_key(my_signing_key, peer_x25519_pub, from, to)?;
            let record = SessionKeyRecord {
                current_key: B64.encode(new_key),
                current_since: now_ms,
                last_seen: now_ms,
                history_keys: Vec::new(),
                peer_root_pub: Some(peer_root_pub_b64.to_string()),
            };
            write_session_key_record(storage, peer_root_id, &record, node_id, now_ms)?;
            Ok(true)
        }
        // 占位记录（已通过入站积累对端公钥但尚未协商会话密钥）：补全会话密钥
        Some(rec) if rec.current_key.is_empty() => {
            let new_key = derive_session_key(my_signing_key, peer_x25519_pub, from, to)?;
            let record = SessionKeyRecord {
                current_key: B64.encode(new_key),
                current_since: now_ms,
                last_seen: now_ms,
                history_keys: Vec::new(),
                peer_root_pub: Some(peer_root_pub_b64.to_string()),
            };
            write_session_key_record(storage, peer_root_id, &record, node_id, now_ms)?;
            Ok(true)
        }
        // 已有 current 密钥 → 长期密钥恒等，原样复用，不重协商、不写盘
        Some(_) => Ok(false),
    }
}

/// 出站 E2E 信封构造核心（S6/S7 接线）：读密钥表对端 root 公钥 → X25519
/// 转换 → `ensure_session_key`（必要时协商/轮换）→ 生成临时密钥对 →
/// `derive_session_key_ephemeral`（我方临时私钥 + 对端 root 公钥 X25519）
/// 派生本次临时会话密钥 → `encrypt_body_with_key` 加密 body。
///
/// 返回 `(encrypted_body, eph_pub_b64)`——接线层据此 `build_envelope_with_eph`
/// 构造携带 `ephPub` 的签名信封。**无对端 root 公钥记录视为内部错误**
/// （`NoSessionKey`，不静默降级明文）。
///
/// `my_signing_key` 为本机 root 签名私钥；`to` 为收件人 rootId（= 密钥表
/// peer 键）。
pub fn encrypt_outbound_body<S: StorageBackend>(
    storage: &mut S,
    my_signing_key: &SigningKey,
    from: &str,
    to: &str,
    kind: &str,
    ts: i64,
    plaintext_body: &Value,
    node_id: &str,
    now_ms: i64,
) -> Result<(Value, String)> {
    // 读对端 root 公钥（base64 → 32B → X25519 转换）；无记录/无公钥 → 内部错误
    let record = read_session_key_record(storage, to)?.ok_or(DmE2eError::NoSessionKey)?;
    let peer_root_pub_b64 = record
        .peer_root_pub
        .as_deref()
        .ok_or(DmE2eError::NoSessionKey)?;
    let peer_root_pub_raw = B64
        .decode(peer_root_pub_b64)
        .map_err(|_| DmE2eError::InvalidCiphertext("peer root pub not base64".into()))?;
    let peer_root_pub: [u8; 32] = peer_root_pub_raw
        .try_into()
        .map_err(|_| DmE2eError::InvalidCiphertext("peer root pub not 32B".into()))?;
    let peer_x25519 = ed_pk_to_x25519(&peer_root_pub)
        .ok_or(DmE2eError::InvalidCiphertext("peer root pub not on curve".into()))?;
    // 协商/轮换（root 私钥 + 对端 root 公钥 X25519）
    ensure_session_key(
        storage,
        my_signing_key,
        &peer_x25519,
        peer_root_pub_b64,
        from,
        to,
        to,
        node_id,
        now_ms,
    )?;
    // 临时密钥对交换：本次临时会话密钥不回写密钥表
    let (eph_priv, eph_pub) = generate_ephemeral_keypair();
    let session_key = derive_session_key_ephemeral(&eph_priv, &peer_x25519, from, to)?;
    let encrypted = encrypt_body_with_key(&session_key, from, to, kind, ts, plaintext_body)?;
    Ok((encrypted, B64.encode(eph_pub)))
}

/// 构造加密后的 body 线形 `{encrypted:true, ciphertext, nonce}`（§19.1.1）。
///
/// `from`/`to`/`kind`/`ts` 同时参与 AAD（防跨 kind/跨对端/跨时间窗搬迁）与
/// 会话密钥选取（peer=to）。当前密钥从密钥表取；未协商 → `NoSessionKey`。
pub fn encrypt_body<S: StorageBackend>(
    storage: &S,
    from: &str,
    to: &str,
    kind: &str,
    ts: i64,
    plaintext_body: &Value,
) -> Result<Value> {
    let record = read_session_key_record(storage, to)?
        .ok_or(DmE2eError::NoSessionKey)?;
    let key_raw = B64.decode(&record.current_key).map_err(|_| {
        DmE2eError::InvalidCiphertext("current key not valid base64".into())
    })?;
    let key_arr: [u8; 32] = key_raw
        .try_into()
        .map_err(|_| DmE2eError::InvalidCiphertext("current key not 32B".into()))?;
    encrypt_body_with_key(&key_arr, from, to, kind, ts, plaintext_body)
}

/// 解密 body（§19.1.1 逆）：按 ts 选密钥（current 或历史），验 AAD 后解出
/// 明文 JSON。AAD 篡改 / 密钥不符 → `Aead` 错误；密文线形非法 → 对应错误。
/// `peer=from`（对端是发件人）。
///
/// S6 接线扩展：出站临时交换 / 入站按 ephPub 派生路径不读写密钥表，改由
/// 接线层显式传入 32B 会话密钥。本函数（按密钥表读取）保留给无 `ephPub`
/// 的 root 直接转换 DH 回退路径（见 [`encrypt_body`] 文档）。
pub fn decrypt_body<S: StorageBackend>(
    storage: &S,
    from: &str,
    to: &str,
    kind: &str,
    ts: i64,
    encrypted_body: &Value,
) -> Result<Value> {
    let record = read_session_key_record(storage, from)?
        .ok_or(DmE2eError::NoSessionKey)?;
    let key_b64 = select_key_for_ts(&record, ts).ok_or(DmE2eError::NoKeyForTs(ts))?;
    let key_raw = B64.decode(key_b64)
        .map_err(|_| DmE2eError::InvalidCiphertext("key not valid base64".into()))?;
    let key_arr: [u8; 32] = key_raw
        .try_into()
        .map_err(|_| DmE2eError::InvalidCiphertext("key not 32B".into()))?;
    decrypt_body_with_key(&key_arr, from, to, kind, ts, encrypted_body)
}

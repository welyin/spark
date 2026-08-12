//! E1 口令校验器（pwv / pwack）纯逻辑。
//!
//! 规格：`wiki/protocol/p2p/personal-data-sync.md` §13；
//! 设计：`wiki/architecture/identity/password-change-propagation.md` §10–§13。
//!
//! 本模块只含纯逻辑（密码学原语 + 线形 + 存储读写 + 门控谓词），不碰网络/p2p/Kernel。

use aes_gcm::aead::{Aead, Nonce};
use aes_gcm::{Aes256Gcm, KeyInit};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::storage::StorageBackend;
use crate::sync::personal::put_personal;
use crate::sync::SyncError;

/// scrypt log2(N)；N = 32768，与身份文件 v2 同档。
pub const SCRYPT_LOG_N: u8 = 15;
/// scrypt r 参数。
pub const SCRYPT_R: u32 = 8;
/// scrypt p 参数。
pub const SCRYPT_P: u32 = 1;

/// `pwv:self` 单记录键（pdsync whole/LWW，明文豁免）。
pub const PWV_KEY: &str = "pwv:self";
/// `pwack:{peer}` 键前缀（pdsync，明文豁免，每设备一条）。
pub const PWACK_PREFIX: &str = "pwack:";
/// 本地水位键：`p2p:pw:appliedVTs` = 已应用 V 的 changedAt（十进制数字）。
pub const APPLIED_VTS_KEY: &str = "p2p:pw:appliedVTs";
/// 本地 stale 标记键：`p2p:pw:stale` = `"true"` / `"false"`。
pub const STALE_KEY: &str = "p2p:pw:stale";
/// 每设备「最近一次成功验证过的 V」水位键前缀：`p2p:pw:lastVerifiedVTs:{peer}`。
/// `0` 表示该设备从未验证（默认）。
pub const LAST_VERIFIED_VTS_PREFIX: &str = "p2p:pw:lastVerifiedVTs:";
/// 本地 last-good V 键：`p2p:pw:lastGoodV` = 上一个已验证 V 的序列化副本。
pub const LAST_GOOD_V_KEY: &str = "p2p:pw:lastGoodV";
/// 本地 grace 窗口键：`p2p:pw:graceMs`，默认 7 天（604800000 ms）。
pub const GRACE_MS_KEY: &str = "p2p:pw:graceMs";
/// 默认 grace 窗口：7 天（毫秒）。
pub const DEFAULT_GRACE_MS: u64 = 7 * 24 * 60 * 60 * 1000;

/// V 中封装的公开常量明文（10 字节：`"spark-pwv1".len()`）。V 的正确性靠 scrypt 成本而非保密。
pub const PWV_PLAINTEXT: &str = "spark-pwv1";
/// Kack 派生的公开常量标签（11 字节：`"spark-pwack".len()`）。
pub const KACK_LABEL: &str = "spark-pwack";
/// ack MAC 输入的固定前缀（5 字节：`"pwack".len()`）。
pub const MAC_PREFIX: &str = "pwack";

/// 口令校验器模块错误。
#[derive(Debug, thiserror::Error)]
pub enum PwError {
    /// 存储后端错误。
    #[error(transparent)]
    Storage(#[from] crate::storage::StorageError),
    /// JSON 序列化/反序列化错误。
    #[error(transparent)]
    Serde(#[from] serde_json::Error),
    /// 个人域同步错误（put_personal 等）。
    #[error(transparent)]
    Sync(#[from] SyncError),
    /// 密码学错误（scrypt/AES-GCM 失败）。
    #[error("crypto: {0}")]
    Crypto(String),
}

/// 口令校验器模块 Result 别名。
pub type Result<T> = std::result::Result<T, PwError>;

// ── 线形类型 ───────────────────────────────────────────────────────────

/// `pwv:self` 值线形（camelCase）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PasswordVerifier {
    /// 格式版本，恒为 1。
    pub v: u64,
    /// KDF 名称，恒为 `"scrypt"`。
    pub kdf: String,
    /// 16B 盐，base64。
    pub salt: String,
    /// 12B nonce，base64。
    pub nonce: String,
    /// AES-256-GCM 密文（含 16B tag），base64。
    pub ct: String,
    /// 本次口令变更时刻（Unix 毫秒，u64）。
    pub changed_at: u64,
    /// 发起变更的设备 peerId（base58）。
    pub changed_by: String,
}

impl PasswordVerifier {
    /// 序列化为 JSON 字符串（字段序固定、camelCase）。
    pub fn to_json(&self) -> Result<String> {
        Ok(serde_json::to_string(self)?)
    }

    /// 从存储原始 JSON 解析。
    pub fn parse(raw: &str) -> Result<Self> {
        Ok(serde_json::from_str(raw)?)
    }
}

/// `pwack:{peer}` 值线形（camelCase）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PasswordAck {
    /// 格式版本，恒为 1。
    pub v: u64,
    /// 被证明的 V 的 `changedAt`（u64 毫秒）。
    pub v_ts: u64,
    /// 口令知识证明，HMAC-SHA256 32B，base64。
    pub mac: String,
}

impl PasswordAck {
    /// 序列化为 JSON 字符串。
    pub fn to_json(&self) -> Result<String> {
        Ok(serde_json::to_string(self)?)
    }

    /// 从存储原始 JSON 解析。
    pub fn parse(raw: &str) -> Result<Self> {
        Ok(serde_json::from_str(raw)?)
    }
}

// ── Kverify / V 构造与验证 ────────────────────────────────────────────

/// 派生 Kverify = scrypt(password, salt, N=32768, r=8, p=1)。
pub fn derive_kverify(password: &str, salt: &[u8; 16]) -> Result<[u8; 32]> {
    let params =
        scrypt::Params::new(SCRYPT_LOG_N, SCRYPT_R, SCRYPT_P)
            .map_err(|e| PwError::Crypto(format!("scrypt params: {e}")))?;
    let mut key = [0u8; 32];
    scrypt::scrypt(password.as_bytes(), salt, &params, &mut key)
        .map_err(|e| PwError::Crypto(format!("scrypt: {e}")))?;
    Ok(key)
}

/// 构造 `pwv:self` 值。
///
/// `salt` 16B、`nonce` 12B 由调用方生成；`changed_at` 与 `epoch:state.rotatedAt`
/// 同源（规格 §13.5）。
pub fn build_value(
    password: &str,
    salt: &[u8; 16],
    nonce: &[u8; 12],
    changed_at: u64,
    changed_by: &str,
) -> Result<PasswordVerifier> {
    let kverify = derive_kverify(password, salt)?;
    let ct = encrypt_with_kverify(&kverify, nonce)?;
    Ok(PasswordVerifier {
        v: 1,
        kdf: "scrypt".to_string(),
        salt: B64.encode(salt),
        nonce: B64.encode(nonce),
        ct,
        changed_at,
        changed_by: changed_by.to_string(),
    })
}

/// 用候选口令验证 V：成功返回 true。
pub fn verify_value(value: &PasswordVerifier, password: &str) -> bool {
    let salt = match B64.decode(&value.salt) {
        Ok(b) if b.len() == 16 => b.try_into().expect("16 bytes"),
        _ => return false,
    };
    let kverify = match derive_kverify(password, &salt) {
        Ok(k) => k,
        Err(_) => return false,
    };
    decrypt_with_kverify(&kverify, value)
        .map(|pt| pt == PWV_PLAINTEXT.as_bytes())
        .unwrap_or(false)
}

/// 用已派生的 Kverify 直接解密 V（供向量消费与 last-good 校验）。
pub fn decrypt_with_kverify(kverify: &[u8; 32], value: &PasswordVerifier) -> Option<Vec<u8>> {
    let nonce_bytes = B64.decode(&value.nonce).ok()?;
    let nonce: [u8; 12] = nonce_bytes.try_into().ok()?;
    let ct = B64.decode(&value.ct).ok()?;
    let cipher = Aes256Gcm::new_from_slice(kverify).ok()?;
    let n: Nonce<Aes256Gcm> = nonce.into();
    cipher.decrypt(&n, ct.as_ref()).ok()
}

fn encrypt_with_kverify(kverify: &[u8; 32], nonce: &[u8; 12]) -> Result<String> {
    let cipher = Aes256Gcm::new_from_slice(kverify)
        .map_err(|e| PwError::Crypto(format!("aes key: {e}")))?;
    let n: Nonce<Aes256Gcm> = (*nonce).into();
    let sealed = cipher
        .encrypt(&n, PWV_PLAINTEXT.as_bytes())
        .map_err(|e| PwError::Crypto(format!("aes-gcm encrypt: {e}")))?;
    Ok(B64.encode(sealed))
}


// ── Kack / ack MAC ─────────────────────────────────────────────────────

/// 派生 Kack = sha256(Kverify || "spark-pwack")。
pub fn derive_kack(kverify: &[u8; 32]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(kverify);
    hasher.update(KACK_LABEL.as_bytes());
    hasher.finalize().into()
}

/// 计算 ack MAC = HMAC-SHA256(Kack, "pwack" || peer_b58 || decimal_ascii(vTs))。
pub fn compute_ack_mac(kack: &[u8; 32], peer: &str, v_ts: u64) -> [u8; 32] {
    type HmacSha256 = Hmac<Sha256>;
    let mut mac = <HmacSha256 as hmac::digest::KeyInit>::new_from_slice(kack)
        .expect("HMAC accepts any key length");
    mac.update(MAC_PREFIX.as_bytes());
    mac.update(peer.as_bytes());
    mac.update(v_ts.to_string().as_bytes());
    mac.finalize().into_bytes().into()
}

/// 构造 `pwack:{peer}` 值。
pub fn build_ack(kverify: &[u8; 32], peer: &str, v_ts: u64) -> PasswordAck {
    let kack = derive_kack(kverify);
    let mac = compute_ack_mac(&kack, peer, v_ts);
    PasswordAck {
        v: 1,
        v_ts,
        mac: B64.encode(mac),
    }
}

/// 校验 ack MAC 与 vTs。
pub fn verify_ack_mac(kack: &[u8; 32], peer: &str, v_ts: u64, mac_b64: &str) -> bool {
    let expected = compute_ack_mac(kack, peer, v_ts);
    let Ok(got) = B64.decode(mac_b64) else {
        return false;
    };
    let Ok(got) = <[u8; 32]>::try_from(got) else {
        return false;
    };
    got == expected
}

/// 门控谓词：`ack` 的 `vTs` 覆盖 `pwv.changed_at`。
pub fn ack_covers(pwv: &PasswordVerifier, ack: Option<&PasswordAck>) -> bool {
    match ack {
        Some(a) => a.v_ts >= pwv.changed_at,
        None => false,
    }
}

// ── 存储读写 ───────────────────────────────────────────────────────────

/// 读取 `pwv:self`；缺失或损坏返回 `Ok(None)`。
pub fn get_pwv<S: StorageBackend>(storage: &S) -> Result<Option<PasswordVerifier>> {
    match storage.get(PWV_KEY)? {
        Some(raw) => Ok(Some(PasswordVerifier::parse(&raw)?)),
        None => Ok(None),
    }
}

/// 写入 `pwv:self`（pdsync 单记录 LWW）。
pub fn put_pwv<S: StorageBackend>(
    storage: &mut S,
    node_id: &str,
    pwv: &PasswordVerifier,
    now_ms: i64,
) -> Result<crate::sync::meta::DocMeta> {
    let raw = pwv.to_json()?;
    Ok(put_personal(storage, node_id, PWV_KEY, &raw, now_ms)?)
}

/// `pwack:{peer}` 完整键。
pub fn pwack_key(peer: &str) -> String {
    format!("{PWACK_PREFIX}{peer}")
}

/// 读取 `pwack:{peer}`；缺失返回 `Ok(None)`，损坏传播 `Err`。
pub fn get_pwack<S: StorageBackend>(storage: &S, peer: &str) -> Result<Option<PasswordAck>> {
    let key = pwack_key(peer);
    match storage.get(&key)? {
        Some(raw) => Ok(Some(PasswordAck::parse(&raw)?)),
        None => Ok(None),
    }
}

/// 写入 `pwack:{peer}`（pdsync，明文豁免）。
pub fn put_pwack<S: StorageBackend>(
    storage: &mut S,
    node_id: &str,
    peer: &str,
    ack: &PasswordAck,
    now_ms: i64,
) -> Result<crate::sync::meta::DocMeta> {
    let key = pwack_key(peer);
    let raw = ack.to_json()?;
    Ok(put_personal(storage, node_id, &key, &raw, now_ms)?)
}

/// 读取已应用 V 水位；缺失 → 0。
pub fn get_applied_vts<S: StorageBackend>(storage: &S) -> Result<u64> {
    match storage.get(APPLIED_VTS_KEY)? {
        Some(raw) => raw
            .parse::<u64>()
            .map_err(|e| PwError::Crypto(format!("applied vts parse: {e}"))),
        None => Ok(0),
    }
}

/// 写入已应用 V 水位（十进制数字）。
pub fn put_applied_vts<S: StorageBackend>(storage: &mut S, vts: u64) -> Result<()> {
    storage.put(APPLIED_VTS_KEY, &vts.to_string())?;
    Ok(())
}

/// 读取 stale 标记；缺失 → false。
pub fn get_stale<S: StorageBackend>(storage: &S) -> Result<bool> {
    match storage.get(STALE_KEY)? {
        Some(raw) => Ok(raw == "true"),
        None => Ok(false),
    }
}

/// 写入 stale 标记。
pub fn put_stale<S: StorageBackend>(storage: &mut S, stale: bool) -> Result<()> {
    storage.put(STALE_KEY, if stale { "true" } else { "false" })?;
    Ok(())
}

/// `p2p:pw:lastVerifiedVTs:{peer}` 完整键。
pub fn last_verified_vts_key(peer: &str) -> String {
    format!("{LAST_VERIFIED_VTS_PREFIX}{peer}")
}

/// 读取某设备最近一次成功验证的 V 水位；缺失/未验证 → `0`。
pub fn get_last_verified_vts<S: StorageBackend>(storage: &S, peer: &str) -> Result<u64> {
    match storage.get(&last_verified_vts_key(peer))? {
        Some(raw) => raw
            .parse::<u64>()
            .map_err(|e| PwError::Crypto(format!("last verified vts parse: {e}"))),
        None => Ok(0),
    }
}

/// 写入某设备最近一次成功验证的 V 水位。
pub fn put_last_verified_vts<S: StorageBackend>(storage: &mut S, peer: &str, vts: u64) -> Result<()> {
    storage.put(&last_verified_vts_key(peer), &vts.to_string())?;
    Ok(())
}

/// 读取本机 grace 窗口（毫秒）；缺失 → [`DEFAULT_GRACE_MS`]。
pub fn get_grace_ms<S: StorageBackend>(storage: &S) -> Result<u64> {
    match storage.get(GRACE_MS_KEY)? {
        Some(raw) => raw
            .parse::<u64>()
            .map_err(|e| PwError::Crypto(format!("grace ms parse: {e}"))),
        None => Ok(DEFAULT_GRACE_MS),
    }
}

/// 写入本机 grace 窗口（毫秒）。
pub fn put_grace_ms<S: StorageBackend>(storage: &mut S, grace_ms: u64) -> Result<()> {
    storage.put(GRACE_MS_KEY, &grace_ms.to_string())?;
    Ok(())
}

/// 读取 last-good V；缺失或损坏返回 `Ok(None)`。
pub fn get_last_good_v<S: StorageBackend>(storage: &S) -> Result<Option<PasswordVerifier>> {
    match storage.get(LAST_GOOD_V_KEY)? {
        Some(raw) => Ok(Some(PasswordVerifier::parse(&raw)?)),
        None => Ok(None),
    }
}

/// 写入 last-good V。
pub fn put_last_good_v<S: StorageBackend>(
    storage: &mut S,
    pwv: &PasswordVerifier,
) -> Result<()> {
    storage.put(LAST_GOOD_V_KEY, &pwv.to_json()?)?;
    Ok(())
}

// ── 应用与门控 ─────────────────────────────────────────────────────────

/// 应用一条 `pwv:self`（LWW + 水位单调防回放 + 未来 ts 拒收）。
///
/// - `incoming.changed_at <= applied_vts` → 回放，忽略，返回 `Ok(false)`。
/// - `incoming.changed_at > now_ms + ENVELOPE_TS_WINDOW_MS` → 未来 ts 拒收，返回
///   `Err(PwError::Crypto(...))`。
/// - 否则写入 `pwv:self` 并推进水位到 `incoming.changed_at`，返回 `Ok(true)`。
///
/// last-good 保留在调用方：调用方应在接受新 V 前备份当前已验证 V。
pub fn apply_value<S: StorageBackend>(
    storage: &mut S,
    node_id: &str,
    incoming: &PasswordVerifier,
    now_ms: i64,
) -> Result<bool> {
    let applied = get_applied_vts(storage)?;
    if incoming.changed_at <= applied {
        return Ok(false);
    }
    let upper_bound = (now_ms as u64).saturating_add(crate::kernel::dm_envelope::ENVELOPE_TS_WINDOW_MS as u64);
    if incoming.changed_at > upper_bound {
        return Err(PwError::Crypto(format!(
            "pwv future ts rejected: changed_at={} now={}",
            incoming.changed_at, now_ms
        )));
    }
    put_pwv(storage, node_id, incoming, now_ms)?;
    put_applied_vts(storage, incoming.changed_at)?;
    Ok(true)
}

/// 注入式写入 `pwv:self`（QR 恢复专用，规格 §13.8 QR-F2）。
///
/// 与 [`apply_value`] 的差异：**不推进 `appliedVTs`**——恢复设备是全新无 V
/// （`applied=0`），注入的 V 是生成端 A 的 V；B 首次 unlock 时经
/// `maybe_ack_on_unlock` 验证口令后自锚并 ack A 的 V（`changed_at > applied(0)`
/// 成立）。若这里推进了水位，B 会因 `changed_at <= applied` 早退永不 ack（
/// architect-m3 裁定明确「recover 不推进 appliedVTs」）。
///
/// 守卫对齐 `apply_value`（防恶意 QR 塞伪造 V 推死水位）：未来 ts 拒收 +
/// 水位单调（本地已有更新的 V 则忽略）。
pub fn inject_pwv<S: StorageBackend>(
    storage: &mut S,
    node_id: &str,
    incoming: &PasswordVerifier,
    now_ms: i64,
) -> Result<()> {
    let applied = get_applied_vts(storage)?;
    if incoming.changed_at <= applied {
        return Ok(());
    }
    let upper_bound = (now_ms as u64)
        .saturating_add(crate::kernel::dm_envelope::ENVELOPE_TS_WINDOW_MS as u64);
    if incoming.changed_at > upper_bound {
        return Err(PwError::Crypto(format!(
            "pwv future ts rejected: changed_at={} now={}",
            incoming.changed_at, now_ms
        )));
    }
    put_pwv(storage, node_id, incoming, now_ms)?;
    Ok(())
}

/// D′ 门控决策结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GateDecision {
    /// 目标设备已覆盖最新 V 或在 grace 内，可以授予新 epoch 密钥。
    Pass,
    /// 目标设备未覆盖最新 V，暂扣新 epoch 密钥。
    Gated {
        /// 最新 V 的 changedAt。
        latest_vts: u64,
        /// 目标设备可信锚 `lastVerifiedVTs:{peer}`；从未验证则为 `0`。
        last_verified_vts: u64,
        /// 剩余 grace（毫秒），仅用于诊断。
        /// - `None`：从未验证设备（lastVerifiedVTs == 0），无 grace 可剩。
        /// - `Some(0)`：曾验证但 grace 已耗尽。
        /// - `Some(>0)`：曾验证且仍在 grace 窗口内（放行走 `Pass`）。
        grace_remaining_ms: Option<u64>,
    },
}

/// D′ ack 门控谓词（writer 侧，可信锚模型）。
///
/// 规则（规格 §13.5/§13.8 最终版）：
/// - `pwv:self` 缺失（老设备/未设口令）→ `Pass`（不可回归项）。
/// - **只读可信锚 `p2p:pw:lastVerifiedVTs:{peer}`；ack 记录退出判定**。
/// - `lastVerifiedVTs:{peer} >= pwv.changed_at` → `Pass`。
/// - `lastVerifiedVTs:{peer} == 0`（从未验证，含新配对/重配对）→
///   `Gated(grace_remaining=None)`，必须先 verify 解锁。
/// - 曾验证设备：deadline = `lastVerifiedVTs:{peer} + graceMs`；
///   `now_ms <= deadline` → `Pass`，否则 `Gated(grace_remaining=Some(0))`。
///
/// `graceMs` 从本地键 [`GRACE_MS_KEY`] 读取，缺失用 [`DEFAULT_GRACE_MS`]（7 天）。
pub fn should_gate<S: StorageBackend>(
    storage: &S,
    peer: &str,
    now_ms: u64,
) -> Result<GateDecision> {
    let Some(pwv) = get_pwv(storage)? else {
        return Ok(GateDecision::Pass);
    };

    let latest_vts = pwv.changed_at;
    let last_verified = get_last_verified_vts(storage, peer)?;

    // 可信锚已覆盖最新 V → 放行。
    if last_verified >= latest_vts {
        return Ok(GateDecision::Pass);
    }

    // 从未验证设备（含新配对/重配对）立即暂扣，不吃 grace。
    if last_verified == 0 {
        return Ok(GateDecision::Gated {
            latest_vts,
            last_verified_vts: 0,
            grace_remaining_ms: None,
        });
    }

    // 曾验证设备：grace 从可信锚起算。
    let grace_ms = get_grace_ms(storage)?;
    let deadline = last_verified.saturating_add(grace_ms);
    if now_ms <= deadline {
        let _remaining = deadline.saturating_sub(now_ms);
        return Ok(GateDecision::Pass);
    }

    Ok(GateDecision::Gated {
        latest_vts,
        last_verified_vts: last_verified,
        grace_remaining_ms: Some(0),
    })
}

/// 校验 `pwack:{peer}` 的 MAC，通过则单调推进可信锚 `lastVerifiedVTs:{peer}`。
///
/// - 成功 → 返回 `Ok(true)`，并将锚更新为 `max(现有, ack.vTs)`。
/// - 失败/无 ack → 返回 `Ok(false)`，**零状态写入**。
pub fn verify_and_anchor_ack<S: StorageBackend>(
    storage: &mut S,
    peer: &str,
    kverify: &[u8; 32],
    _now_ms: i64,
) -> Result<bool> {
    let Some(ack) = get_pwack(storage, peer)? else {
        return Ok(false);
    };
    let kack = derive_kack(kverify);
    if !verify_ack_mac(&kack, peer, ack.v_ts, &ack.mac) {
        return Ok(false);
    }
    let existing = get_last_verified_vts(storage, peer)?;
    let new = existing.max(ack.v_ts);
    if new > existing {
        put_last_verified_vts(storage, peer, new)?;
    }
    Ok(true)
}

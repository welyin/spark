//! M3 epoch 选择性密钥轮换——密码学原语与线形（纯逻辑）。
//!
//! 规格：`wiki/protocol/p2p/personal-data-sync.md`；方案：
//! `wiki/architecture/identity/m3-epoch-rotation-plan.md` §5。
//! 本模块只含纯逻辑（密码学原语 + 键构造 + 状态线形），存储读写与编排
//! 见 kernel 层（`kernel/epoch_ops.rs`）。
//!
//! 密钥来源决策（方案 §2 决策①）：epoch 密钥为随机 32B，与身份 seed 无关；
//! 换密码不使旧密钥不可用，历史保留于本机密钥表 `p2p:epoch:key:{N}`。

use aes_gcm::aead::{Aead, Nonce};
use aes_gcm::{Aes256Gcm, KeyInit};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use curve25519_dalek::montgomery::MontgomeryPoint;
use rand::Rng;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

use crate::device::DeviceService;
use crate::plugindata::CollectionDeclaration;
use crate::pw::{self, GateDecision};
use crate::storage::{ScanOptions, StorageBackend};
use crate::sync::personal::put_personal;
use crate::sync::{SyncError, category_for_key};

/// Ed25519↔X25519 转换复用 orgsync access 层同口径实现
/// （TweetNaCl `crypto_sign_ed25519_*_to_curve25519` 口径）。
pub use crate::sync::orgsync::{ed_pk_to_x25519, ed_sk_to_x25519};

/// epoch 模块错误。
#[derive(Debug, thiserror::Error)]
pub enum EpochError {
    /// 存储后端错误。
    #[error(transparent)]
    Storage(#[from] crate::storage::StorageError),
    /// JSON 序列化/反序列化错误。
    #[error(transparent)]
    Serde(#[from] serde_json::Error),
    /// 个人域同步错误（put_personal 等）。
    #[error(transparent)]
    Sync(#[from] SyncError),
    /// 口令校验器错误（D′ ack 门控）。
    #[error(transparent)]
    Password(#[from] crate::pw::PwError),
    /// 安全日志等设备服务错误。
    #[error(transparent)]
    Device(#[from] crate::contact::ContactError),
    /// 其他逻辑错误。
    #[error("{0}")]
    Other(String),
}

/// epoch 模块 Result 别名。
pub type Result<T> = std::result::Result<T, EpochError>;

// ── 键常量（线形见规格文档）────────────────────────────────────────────

/// epoch:state pdsync 单记录键（whole 单记录，LWW 合并）。
pub const STATE_KEY: &str = "epoch:state";
/// ikey 包裹记录键前缀：`ikey:{epoch}:{writerPeer}:{recipientPeer}`。
pub const IKEY_PREFIX: &str = "ikey:";
/// 本机 epoch 密钥表键前缀：`p2p:epoch:key:{N}` = base64(32B)（本地键，不进同步流量）。
pub const LOCAL_KEY_PREFIX: &str = "p2p:epoch:key:";
/// 本机生效 epoch 键：`p2p:epoch:effective` = 十进制数字（本地键，缺省 0=不加密）。
pub const EFFECTIVE_KEY: &str = "p2p:epoch:effective";
/// 恢复加入方标记键（本地键，不进同步流量）：`p2p:epoch:recovered` = "1"。
/// 仅由 `recover_backup`（扫描备份二维码加入，口令+rootId 双因子）写入；存在时
/// `maybe_init_epoch_state` 跳过 init（被动等主导设备 ikey）——方案 Y 主从核心。
pub const RECOVERED_KEY: &str = "p2p:epoch:recovered";
/// 恢复等待开始时间键（本地键）：`p2p:epoch:recovered_at` = 毫秒。用于双恢复
/// 加入方超时自升主导兜底（默认 `RECOVERED_ESCAPE_MS` 后清除标记允许 init）。
pub const RECOVERED_AT_KEY: &str = "p2p:epoch:recovered_at";
/// 恢复加入方等待主导 epoch 的超时（ms）：超时后自升主导，避免双恢复加入方死锁。
pub const RECOVERED_ESCAPE_MS: i64 = 30_000;
/// 对端 hello 宣告 epoch 的本地持久化键前缀：`pdsync:epoch:{peer}` = 十进制数字。
pub const REMOTE_EPOCH_PREFIX: &str = "pdsync:epoch:";
/// 密文值判别字段与取值（`{$enc:"ikey",epoch,nonce,ct}`）。
pub const ENC_FIELD: &str = "$enc";
/// 密文值 `$enc` 字段取值。
pub const ENC_KIND_IKEY: &str = "ikey";

// ── epoch:state 线形 ─────────────────────────────────────────────────

/// 轮换原因（epoch:state.reason 线形取值）。
///
/// 自 E1 起扩展为 `init|revoke|password_change|password_reset|heal`，并带
/// `#[serde(other)] Unknown` 兜底——未知 reason 反序列化为 `Unknown`，避免
/// 旧实现收到新 reason 时 fail-closed 停摆（规格 §4 / §13.1）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RotationReason {
    /// 首次 unlock 初始化（epoch 0→1）。
    Init,
    /// 设备撤销触发轮换。
    Revoke,
    /// 换密码触发轮换。
    PasswordChange,
    /// 重置密码触发轮换（E1）。
    PasswordReset,
    /// D′ 自愈轮换（E1）。
    Heal,
    /// 未知 reason（serde 兜底）。
    #[serde(other)]
    Unknown,
}

impl RotationReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Init => "init",
            Self::Revoke => "revoke",
            Self::PasswordChange => "password_change",
            Self::PasswordReset => "password_reset",
            Self::Heal => "heal",
            Self::Unknown => "unknown",
        }
    }
}

/// epoch:state 记录线形（whole 单记录，LWW）：
/// `{"current":N,"rotatedAt":ms,"rotatedBy":peerId,"reason":"init|revoke|password_change"}`。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EpochState {
    pub current: u64,
    pub rotated_at: i64,
    pub rotated_by: String,
    pub reason: RotationReason,
}

// ── ikey 包裹记录线形 ────────────────────────────────────────────────

/// ikey 包裹记录值线形：`{"wrappedKey":b64,"nonce":b64,"ts":ms}`。
/// 无签名（pdsync 签名信封已证身份；wrap 内嵌 writerPeer 与信封 from 交叉验证）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IkeyRecord {
    pub wrapped_key: String,
    pub nonce: String,
    pub ts: i64,
}

/// ikey 包裹记录键 `ikey:{epoch}:{writerPeer}:{recipientPeer}`。
pub fn ikey_key(epoch: u64, writer_peer: &str, recipient_peer: &str) -> String {
    format!("{IKEY_PREFIX}{epoch}:{writer_peer}:{recipient_peer}")
}

/// 解析 ikey 键 → (epoch, writerPeer, recipientPeer)；非 ikey 键 → `None`。
/// peerId 不含冒号（base58），从左侧定长切分。
pub fn parse_ikey_key(key: &str) -> Option<(u64, String, String)> {
    let rest = key.strip_prefix(IKEY_PREFIX)?;
    let (epoch_str, rest) = rest.split_once(':')?;
    let epoch = epoch_str.parse::<u64>().ok()?;
    let (writer, recipient) = rest.split_once(':')?;
    if writer.is_empty() || recipient.is_empty() || recipient.contains(':') {
        return None;
    }
    Some((epoch, writer.to_string(), recipient.to_string()))
}

/// 本机密钥表键 `p2p:epoch:key:{N}`。
pub fn local_key_key(epoch: u64) -> String {
    format!("{LOCAL_KEY_PREFIX}{epoch}")
}

// ── 密文值判别 ───────────────────────────────────────────────────────

/// 值是否为 ikey 密文信封（判别规则：恰好含 `$enc:"ikey"` 且 epoch/nonce/ct
/// 类型正确的 JSON 对象）。插件明文值不含 `$enc` 字段（契约）。
pub fn is_ikey_ciphertext(value: &Value) -> bool {
    value.get(ENC_FIELD).and_then(Value::as_str) == Some(ENC_KIND_IKEY)
        && value.get("epoch").is_some_and(|e| e.is_u64())
        && value.get("nonce").and_then(Value::as_str).is_some()
        && value.get("ct").and_then(Value::as_str).is_some()
}

// ── ikey box/unbox（设备 DH 包裹）─────────────────────────────────────

/// 域分隔派生字节（H1b），逐字节规格：
/// `ikey-box\0{rootId}\0{epoch}\0{writerPeer}\0{recipientPeer}`
/// （`\0` 为单字节 0x00；epoch 为十进制 ASCII；rootId 为 64hex UTF-8；
/// peerId 为 base58 UTF-8）。绑定 root+epoch+双向设备，防包裹跨上下文搬迁。
pub fn box_domain_info(root_id: &str, epoch: u64, writer_peer: &str, recipient_peer: &str) -> Vec<u8> {
    let mut info = b"ikey-box\0".to_vec();
    info.extend_from_slice(root_id.as_bytes());
    info.push(0);
    info.extend_from_slice(epoch.to_string().as_bytes());
    info.push(0);
    info.extend_from_slice(writer_peer.as_bytes());
    info.push(0);
    info.extend_from_slice(recipient_peer.as_bytes());
    info
}

/// 域分隔共享 → AES-256 密钥（H1b）：`sha256(shared || domain_info)`。
fn derive_box_key(shared_bytes: &[u8; 32], domain_info: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(shared_bytes);
    hasher.update(domain_info);
    hasher.finalize().into()
}

/// DH 共享 + 域分隔派生（H1a 低阶点拒绝）：两侧对称。
fn derive_shared_box_key(
    peer_x25519: &[u8; 32],
    self_x25519_priv: &[u8; 32],
    domain_info: &[u8],
) -> Option<[u8; 32]> {
    let shared = MontgomeryPoint(*peer_x25519).mul_clamped(*self_x25519_priv);
    let shared_bytes = shared.to_bytes();
    // H1a：拒低阶点（X25519 低阶点乘积为全零共享）——防强制 DH 落低阶子群。
    if shared_bytes.iter().all(|&b| b == 0) {
        return None;
    }
    let box_key = derive_box_key(&shared_bytes, domain_info);
    Some(box_key)
}

/// crypto_box 包裹 epoch 密钥（方案 §5.2）：recipient 设备公钥 X25519 ×
/// writer 设备私钥 X25519 → DH；`sha256(shared || domainInfo)` → AES-256-GCM
/// 加密 32B epoch 密钥。24B 随机 nonce，前 12B 作 GCM nonce（与 orgkey box 同族）。
/// 返回 (wrappedKey base64, nonce24 base64)。低阶点 → `None`。
pub fn box_ikey(
    epoch_key: &[u8; 32],
    recipient_x25519_pub: &[u8; 32],
    writer_x25519_priv: &[u8; 32],
    root_id: &str,
    epoch: u64,
    writer_peer: &str,
    recipient_peer: &str,
) -> Option<(String, String)> {
    let mut nonce24 = [0u8; 24];
    rand::rng().fill_bytes(&mut nonce24);
    box_ikey_with_nonce(
        epoch_key,
        recipient_x25519_pub,
        writer_x25519_priv,
        root_id,
        epoch,
        writer_peer,
        recipient_peer,
        &nonce24,
    )
}

/// `box_ikey` 的 nonce 注入变体（golden vectors / 测试确定性用）。
#[doc(hidden)]
pub fn box_ikey_with_nonce(
    epoch_key: &[u8; 32],
    recipient_x25519_pub: &[u8; 32],
    writer_x25519_priv: &[u8; 32],
    root_id: &str,
    epoch: u64,
    writer_peer: &str,
    recipient_peer: &str,
    nonce24: &[u8; 24],
) -> Option<(String, String)> {
    let domain_info = box_domain_info(root_id, epoch, writer_peer, recipient_peer);
    let box_key = derive_shared_box_key(recipient_x25519_pub, writer_x25519_priv, &domain_info)?;
    let cipher = Aes256Gcm::new_from_slice(&box_key).ok()?;
    let nonce_arr: [u8; 12] = nonce24[..12].try_into().ok()?;
    let ct = cipher
        .encrypt(&Nonce::<Aes256Gcm>::from(nonce_arr), epoch_key.as_slice())
        .ok()?;
    Some((B64.encode(ct), B64.encode(nonce24)))
}

/// 解包 ikey（box_ikey 逆）：writer 设备公钥 X25519 × recipient 设备私钥
/// X25519 → 同域派生 → AES-256-GCM 解密 → 32B epoch 密钥。
/// 低阶点 / 域不匹配 / 密文损坏 → `None`。
pub fn unbox_ikey(
    wrapped_key: &str,
    nonce24: &str,
    writer_x25519_pub: &[u8; 32],
    recipient_x25519_priv: &[u8; 32],
    root_id: &str,
    epoch: u64,
    writer_peer: &str,
    recipient_peer: &str,
) -> Option<[u8; 32]> {
    let ct = B64.decode(wrapped_key).ok()?;
    let nonce_raw = B64.decode(nonce24).ok()?;
    if nonce_raw.len() < 12 {
        return None;
    }
    let nonce_arr: [u8; 12] = nonce_raw[..12].try_into().ok()?;
    let domain_info = box_domain_info(root_id, epoch, writer_peer, recipient_peer);
    let box_key = derive_shared_box_key(writer_x25519_pub, recipient_x25519_priv, &domain_info)?;
    let cipher = Aes256Gcm::new_from_slice(&box_key).ok()?;
    let plain = cipher
        .decrypt(&Nonce::<Aes256Gcm>::from(nonce_arr), ct.as_ref())
        .ok()?;
    plain.try_into().ok()
}

// ── 值 wrap/unwrap（AES-256-GCM，AAD=记录完整键）───────────────────────

/// 值加密（方案 §5.2）：AES-256-GCM，密钥 = epoch 密钥，明文 = 值 JSON
/// 序列化 UTF-8，AAD = 记录完整键 UTF-8（防密文跨记录搬迁），nonce 12B 随机。
/// 输出线形 `{"$enc":"ikey","epoch":N,"nonce":b64,"ct":b64}`。
pub fn wrap_value(epoch_key: &[u8; 32], record_key: &str, epoch: u64, plaintext: &str) -> Option<Value> {
    let mut nonce12 = [0u8; 12];
    rand::rng().fill_bytes(&mut nonce12);
    wrap_value_with_nonce(epoch_key, record_key, epoch, &nonce12, plaintext)
}

/// `wrap_value` 的 nonce 注入变体（golden vectors / 测试确定性用）。
#[doc(hidden)]
pub fn wrap_value_with_nonce(
    epoch_key: &[u8; 32],
    record_key: &str,
    epoch: u64,
    nonce12: &[u8; 12],
    plaintext: &str,
) -> Option<Value> {
    let cipher = Aes256Gcm::new_from_slice(epoch_key).ok()?;
    let payload = aes_gcm::aead::Payload {
        msg: plaintext.as_bytes(),
        aad: record_key.as_bytes(),
    };
    let ct = cipher
        .encrypt(&Nonce::<Aes256Gcm>::from(*nonce12), payload)
        .ok()?;
    Some(json!({
        ENC_FIELD: ENC_KIND_IKEY,
        "epoch": epoch,
        "nonce": B64.encode(nonce12),
        "ct": B64.encode(ct),
    }))
}

/// 值解密（wrap_value 逆）：验 AAD 后解出明文。线形不合法 / AAD 不匹配 /
/// 密钥不对 → `None`（接收侧按「不解不推进」丢弃）。
pub fn unwrap_value(epoch_key: &[u8; 32], record_key: &str, ciphertext: &Value) -> Option<String> {
    if !is_ikey_ciphertext(ciphertext) {
        return None;
    }
    let nonce_b64 = ciphertext.get("nonce")?.as_str()?;
    let ct_b64 = ciphertext.get("ct")?.as_str()?;
    let nonce_raw = B64.decode(nonce_b64).ok()?;
    let nonce_arr: [u8; 12] = nonce_raw.try_into().ok()?;
    let ct = B64.decode(ct_b64).ok()?;
    let cipher = Aes256Gcm::new_from_slice(epoch_key).ok()?;
    let payload = aes_gcm::aead::Payload {
        msg: ct.as_ref(),
        aad: record_key.as_bytes(),
    };
    let plain = cipher
        .decrypt(&Nonce::<Aes256Gcm>::from(nonce_arr), payload)
        .ok()?;
    Some(String::from_utf8(plain).ok()?)
}

/// 生成随机 32B epoch 密钥（决策①：随机密钥，非 KDF(seed)）。
pub fn generate_ikey() -> [u8; 32] {
    let mut k = [0u8; 32];
    rand::rng().fill_bytes(&mut k);
    k
}

// ── EpochState / IkeyRecord 序列化辅助 ─────────────────────────────────

impl EpochState {
    /// 序列化为 JSON 字符串（camelCase，个人域记录值）。
    pub fn to_json(&self) -> Result<String> {
        Ok(serde_json::to_string(self)?)
    }

    /// 从存储原始 JSON 解析。
    pub fn parse(raw: &str) -> Result<Self> {
        Ok(serde_json::from_str(raw)?)
    }
}

impl IkeyRecord {
    /// 序列化为 JSON 字符串（camelCase）。
    pub fn to_json(&self) -> Result<String> {
        Ok(serde_json::to_string(self)?)
    }

    /// 从存储原始 JSON 解析。
    pub fn parse(raw: &str) -> Result<Self> {
        Ok(serde_json::from_str(raw)?)
    }
}

// ── epoch:state 读写 ───────────────────────────────────────────────────

/// 读取 `epoch:state`；缺失或损坏返回 `Ok(None)`。
pub fn get_epoch_state<S: StorageBackend>(storage: &S) -> Result<Option<EpochState>> {
    let Some(raw) = storage.get(STATE_KEY)? else {
        return Ok(None);
    };
    Ok(Some(EpochState::parse(&raw)?))
}

/// 写入 `epoch:state`（pdsync 单记录，LWW）。
pub fn put_epoch_state<S: StorageBackend>(
    storage: &mut S,
    node_id: &str,
    state: &EpochState,
    now_ms: i64,
) -> Result<crate::sync::meta::DocMeta> {
    let raw = state.to_json()?;
    Ok(put_personal(storage, node_id, STATE_KEY, &raw, now_ms)?)
}

// ── 本机密钥表读写 ─────────────────────────────────────────────────────

/// 读取本机密钥表 `p2p:epoch:key:{N}`。
pub fn get_local_key<S: StorageBackend>(storage: &S, epoch: u64) -> Result<Option<[u8; 32]>> {
    let Some(raw) = storage.get(&local_key_key(epoch))? else {
        return Ok(None);
    };
    let bytes = B64.decode(raw).map_err(|e| EpochError::Other(e.to_string()))?;
    let arr: [u8; 32] = bytes.try_into().map_err(|_| {
        EpochError::Other(format!("local key for epoch {epoch} is not 32 bytes"))
    })?;
    Ok(Some(arr))
}

/// 写入本机密钥表 `p2p:epoch:key:{N}`（32B base64）。
pub fn put_local_key<S: StorageBackend>(storage: &mut S, epoch: u64, key: &[u8; 32]) -> Result<()> {
    storage.put(&local_key_key(epoch), &B64.encode(key))?;
    Ok(())
}

/// 扫描全部本机密钥表，按 epoch 升序返回。
pub fn list_local_keys<S: StorageBackend>(storage: &S) -> Result<BTreeMap<u64, [u8; 32]>> {
    let mut out = BTreeMap::new();
    for (key, raw) in storage.scan(&ScanOptions::prefix(LOCAL_KEY_PREFIX))? {
        let epoch = parse_local_key_epoch(&key).ok_or_else(|| {
            EpochError::Other(format!("invalid local key table entry: {key}"))
        })?;
        let bytes = B64.decode(raw).map_err(|e| EpochError::Other(e.to_string()))?;
        let arr: [u8; 32] = bytes.try_into().map_err(|_| {
            EpochError::Other(format!("local key for epoch {epoch} is not 32 bytes"))
        })?;
        out.insert(epoch, arr);
    }
    Ok(out)
}

/// 解析本地键 `p2p:epoch:key:{N}` → epoch。
pub fn parse_local_key_epoch(key: &str) -> Option<u64> {
    let rest = key.strip_prefix(LOCAL_KEY_PREFIX)?;
    rest.parse::<u64>().ok()
}

// ── effective 读写 ─────────────────────────────────────────────────────

/// 读取生效 epoch；缺失 → `0`（不加密）。
pub fn get_effective<S: StorageBackend>(storage: &S) -> Result<u64> {
    match storage.get(EFFECTIVE_KEY)? {
        Some(raw) => raw
            .parse::<u64>()
            .map_err(|e| EpochError::Other(format!("effective epoch parse: {e}"))),
        None => Ok(0),
    }
}

/// 写入生效 epoch（十进制字符串）。
pub fn put_effective<S: StorageBackend>(storage: &mut S, epoch: u64) -> Result<()> {
    storage.put(EFFECTIVE_KEY, &epoch.to_string())?;
    Ok(())
}

// ── 方案 Y 主从：恢复加入方跳过 init ──────────────────────────────────

/// 判断本机是否应跳过 epoch init（恢复加入方被动等主导 ikey）。
///
/// - `p2p:epoch:recovered` 标记不存在（主导/正常创建设备）→ false（照常 init）。
/// - 标记存在（恢复加入方）→ 记录等待开始时间（首见写 `recovered_at`），
///   若等待超 `RECOVERED_ESCAPE_MS` 则清除标记自升主导（返回 false，允许 init，
///   双恢复加入方兜底，竞态退化方案 X LWW 收敛）；未超时 → true（跳过 init）。
pub fn should_skip_init_as_recovered<S: StorageBackend>(storage: &mut S) -> Result<bool> {
    if storage.get(RECOVERED_KEY)?.is_none() {
        return Ok(false);
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let at = match storage.get(RECOVERED_AT_KEY)? {
        Some(raw) => raw.parse::<i64>().unwrap_or(0),
        None => {
            // 首见恢复标记：记录等待开始时间。
            storage.put(RECOVERED_AT_KEY, &now.to_string())?;
            now
        }
    };
    if now - at >= RECOVERED_ESCAPE_MS {
        // 等待主导 epoch 超时（双恢复加入方等死锁）：自升主导，清除标记与等待时间。
        storage.delete(RECOVERED_KEY)?;
        storage.delete(RECOVERED_AT_KEY)?;
        log::warn!(
            "[epoch] 恢复加入方等待主导 epoch 超时({RECOVERED_ESCAPE_MS}ms)，自升主导 init"
        );
        return Ok(false);
    }
    Ok(true)
}

// ── 对端 hello epoch 读写（pdsync:epoch:{peer}）────────────────────────

/// 读取对端在 hello 中宣告的 epoch；缺失 → `None`。
pub fn get_remote_epoch<S: StorageBackend>(storage: &S, peer: &str) -> Result<Option<u64>> {
    let key = format!("{REMOTE_EPOCH_PREFIX}{peer}");
    match storage.get(&key)? {
        Some(raw) => Ok(Some(
            raw.parse::<u64>()
                .map_err(|e| EpochError::Other(format!("remote epoch parse: {e}")))?,
        )),
        None => Ok(None),
    }
}

/// 持久化对端 hello 宣告的 epoch。
pub fn put_remote_epoch<S: StorageBackend>(storage: &mut S, peer: &str, epoch: u64) -> Result<()> {
    let key = format!("{REMOTE_EPOCH_PREFIX}{peer}");
    storage.put(&key, &epoch.to_string())?;
    Ok(())
}

// ── 授权设备与 EpochService ────────────────────────────────────────────

/// 授权设备信息（足够生成 ikey 包裹）。
#[derive(Clone, Copy, Debug)]
pub struct AuthorizedDevice<'a> {
    /// 设备 peerId（base58）。
    pub peer: &'a str,
    /// 设备 Ed25519 公钥（32B），缺失则无法投递 ikey 包裹。
    pub device_pub_key: Option<&'a [u8; 32]>,
}

/// 无状态 epoch 业务服务（纯逻辑：只操作 `StorageBackend`）。
#[derive(Clone, Copy, Debug)]
pub struct EpochService;

impl EpochService {
    /// 执行一轮 epoch 密钥轮换（S4 编排层在 io_lock 内调用）。
    ///
    /// 流程：读当前 state → +1 → 随机生成 ikey → 写 `epoch:state` →
    /// 写 `p2p:epoch:key:{N}` → 推进 `p2p:epoch:effective` →
    /// 为每个有 `device_pub_key` 的授权设备生成并写入 `ikey:` 包裹 →
    /// 记安全日志 `epoch_rotated`。
    ///
    /// 单个设备缺公钥或 Ed25519→X25519 转换失败仅跳过该设备，不失败整体轮换。
    pub fn rotate<S: StorageBackend>(
        storage: &mut S,
        root_id: &str,
        my_peer: &str,
        node_id: &str,
        now_ms: i64,
        reason: RotationReason,
        self_x25519_priv: &[u8; 32],
        devices: &[AuthorizedDevice<'_>],
        kverify: Option<&[u8; 32]>,
    ) -> Result<EpochState> {
        let current = get_epoch_state(storage)?.map(|s| s.current).unwrap_or(0);
        let next_epoch = current.saturating_add(1);
        let ikey = generate_ikey();

        let state = EpochState {
            current: next_epoch,
            rotated_at: now_ms,
            rotated_by: my_peer.to_string(),
            reason,
        };

        // 1. 写 epoch:state（pdsync 单记录）。
        put_epoch_state(storage, node_id, &state, now_ms)?;

        // 2. 写本机密钥表并立即生效（本机生成者天然持有最新密钥）。
        put_local_key(storage, next_epoch, &ikey)?;
        put_effective(storage, next_epoch)?;

        // 3. 为授权设备投递 ikey 包裹；本机自包裹无意义，排除。
        //    受 D′ ack 门控约束：未覆盖最新 pwv 的设备暂扣新 epoch 密钥。
        //
        //    D2 兜底锚定（幂等）：若持有会话 Kverify，对 ack.vTs 高于当前锚的
        //    设备先尝试 verify_and_anchor_ack，再读可信锚做门控判定。
        if let Some(k) = kverify {
            for dev in devices {
                if dev.peer == my_peer {
                    continue;
                }
                // 预筛：ack.vTs > lastVerifiedVTs 才值得试锚定；最终采信只走 MAC。
                let last = pw::get_last_verified_vts(storage, dev.peer)?;
                if let Some(ack) = pw::get_pwack(storage, dev.peer)? {
                    if ack.v_ts > last {
                        let _ = pw::verify_and_anchor_ack(storage, dev.peer, k, now_ms);
                    }
                }
            }
        }

        for dev in devices {
            if dev.peer == my_peer {
                continue;
            }

            match pw::should_gate(storage, dev.peer, now_ms as u64)? {
                GateDecision::Pass => {}
                GateDecision::Gated {
                    latest_vts,
                    last_verified_vts,
                    grace_remaining_ms,
                } => {
                    DeviceService::append_security_log(
                        storage,
                        "pw_grant_gated",
                        json!({
                            "peer": dev.peer,
                            "epoch": next_epoch,
                            "latestVTs": latest_vts,
                            "lastVerifiedVTs": last_verified_vts,
                            "graceRemainingMs": grace_remaining_ms,
                        }),
                        now_ms,
                    )?;
                    continue;
                }
            }

            let Some(ed_pk) = dev.device_pub_key else {
                continue;
            };
            let Some(recipient_x25519) = ed_pk_to_x25519(ed_pk) else {
                continue;
            };
            let Some((wrapped, nonce)) = box_ikey(
                &ikey,
                &recipient_x25519,
                self_x25519_priv,
                root_id,
                next_epoch,
                my_peer,
                dev.peer,
            ) else {
                continue;
            };
            let record = IkeyRecord {
                wrapped_key: wrapped,
                nonce,
                ts: now_ms,
            };
            let key = ikey_key(next_epoch, my_peer, dev.peer);
            let value = record.to_json()?;
            put_personal(storage, node_id, &key, &value, now_ms)?;
        }

        // 4. 安全日志。
        DeviceService::append_security_log(
            storage,
            "epoch_rotated",
            json!({
                "epoch": next_epoch,
                "reason": reason.as_str(),
                "rotatedBy": my_peer,
            }),
            now_ms,
        )?;

        Ok(state)
    }

    /// 尝试解包并激活一个 ikey 包裹（收到 `ikey:` 记录或刷新钩子时调用）。
    ///
    /// 成功：写 `p2p:epoch:key:{epoch}`、必要时推进 `p2p:epoch:effective`、
    /// 记 `epoch_key_activated`。
    /// 失败（低阶点/域错/AAD/密钥错）：记 `epoch_key_unwrap_failed`，返回 `Ok(false)`。
    pub fn try_activate_key<S: StorageBackend>(
        storage: &mut S,
        root_id: &str,
        now_ms: i64,
        writer_peer: &str,
        recipient_peer: &str,
        epoch: u64,
        record: &IkeyRecord,
        writer_x25519_pub: &[u8; 32],
        self_x25519_priv: &[u8; 32],
    ) -> Result<bool> {
        let Some(ikey) = unbox_ikey(
            &record.wrapped_key,
            &record.nonce,
            writer_x25519_pub,
            self_x25519_priv,
            root_id,
            epoch,
            writer_peer,
            recipient_peer,
        ) else {
            DeviceService::append_security_log(
                storage,
                "epoch_key_unwrap_failed",
                json!({
                    "epoch": epoch,
                    "writerPeer": writer_peer,
                    "recipientPeer": recipient_peer,
                }),
                now_ms,
            )?;
            return Ok(false);
        };

        put_local_key(storage, epoch, &ikey)?;
        let effective = get_effective(storage)?;
        if epoch > effective {
            put_effective(storage, epoch)?;
        }

        DeviceService::append_security_log(
            storage,
            "epoch_key_activated",
            json!({
                "epoch": epoch,
                "writerPeer": writer_peer,
                "recipientPeer": recipient_peer,
            }),
            now_ms,
        )?;

        Ok(true)
    }

    /// 密钥表刷新钩子（方案 §5.6）：收到 `epoch:/ikey:` 一批并落库后，扫描
    /// `ikey:{current}:*:{myPeer}` 尝试逐个 unbox；任一成功即写本机密钥表 +
    /// 记 `epoch_key_activated`。
    ///
    /// **修复（2026-08-13 同步风暴根因）**：不再用 `effective >= current` 短路。
    /// 此前每个设备 `rotate(Init)` 都 `put_effective` 推进到与 current 相同，导致
    /// `effective >= current` 恒成立、永不 unbox 对端同 epoch ikey → 两端各持不同
    /// 密钥 → `profile:self` Encrypt 互解不开（unwrap-value-failed）→ Concurrent
    /// 风暴。现在即使 effective == current 也扫对端 ikey 并 unbox。
    ///
    /// **方案 Y（2026-08-14 主从，见 m3 设计 §5.11，取代方案 X）**：只有主导设备
    /// （正常创建账号）init 生成 epoch 密钥，恢复加入方（`recover_backup` 标记
    /// `p2p:epoch:recovered`）不 init、被动等主导的 ikey。故 `try_refresh_keys` 始终
    /// 扫描并 unbox `writer == rotated_by` 的包裹（主从下单一 writer；过滤
    /// writer==rotated_by 是兜底防"双恢复加入方超时自升主导"的互换）。
    pub fn try_refresh_keys<S: StorageBackend>(
        storage: &mut S,
        root_id: &str,
        my_peer: &str,
        now_ms: i64,
    ) -> Result<()> {
        let state = get_epoch_state(storage)?.ok_or_else(|| {
            EpochError::Other("epoch state missing while refreshing keys".to_string())
        })?;
        let rotated_by = state.rotated_by.as_str();
        let self_x25519_priv = crate::p2p::identity_store::load_x25519_private_key(storage)
            .ok_or_else(|| {
                EpochError::Other("cannot load x25519 private key for refresh".to_string())
            })?;
        let target = state.current;
        let prefix = format!("{IKEY_PREFIX}{target}:");
        let items = storage.scan(&ScanOptions::prefix(&prefix))?;
        for (k, v) in items {
            if !k.ends_with(&format!(":{my_peer}")) {
                continue;
            }
            let Some((epoch, writer, recipient)) = parse_ikey_key(&k) else {
                continue;
            };
            if epoch != target {
                continue;
            }
            // 方案 Y：只采用主导（rotated_by）的包裹，其他 writer（如并发自升的
            // 第二主导）成孤儿忽略，防互换。
            if writer != rotated_by {
                continue;
            }
            let Ok(record) = serde_json::from_str::<IkeyRecord>(&v) else {
                continue;
            };
            let Some(writer_device) = DeviceService::get(storage, &writer).ok().flatten() else {
                continue;
            };
            let Some(writer_ed_pk_b64) = writer_device.device_pub_key.as_ref() else {
                continue;
            };
            let Ok(writer_ed_pk_bytes) = B64.decode(writer_ed_pk_b64) else {
                continue;
            };
            let Ok(writer_ed_pk) = writer_ed_pk_bytes.try_into() else {
                continue;
            };
            let Some(writer_x25519_pub) = ed_pk_to_x25519(&writer_ed_pk) else {
                continue;
            };
            let _ = Self::try_activate_key(
                storage,
                &root_id,
                now_ms,
                &writer,
                &recipient,
                epoch,
                &record,
                &writer_x25519_pub,
                &self_x25519_priv,
            );
        }
        Ok(())
    }

    /// 新设备补发钩子（方案 §5.7）：给定设备记录落库后，若 effective > 0、
    /// 未 revoked、devicePubKey 非空、且本机尚无给该设备的 ikey 包裹，
    /// 则本机作为 writer 补写一个 `ikey:{effective}:{my_peer}:{peer}` 包裹。
    ///
    /// G1 延迟开关当前未接入：内核不读前端 `spark.settings.*` 存储，
    /// 待前端设置透传通道（类似 recovery delayHours）后再接入。
    /// 现在行为等价于 `delay_hours = 0`（立即补发）。
    pub fn maybe_grant_epoch_key<S: StorageBackend>(
        storage: &mut S,
        root_id: &str,
        my_peer: &str,
        node_id: &str,
        now_ms: i64,
        peer: &str,
        device_pub_key_b64: Option<&str>,
        revoked_at: Option<i64>,
        kverify: Option<&[u8; 32]>,
    ) -> Result<bool> {
        let effective = get_effective(storage)?;
        if effective == 0 {
            return Ok(false);
        }
        if revoked_at.is_some() {
            return Ok(false);
        }
        let Some(device_pub_key_b64) = device_pub_key_b64 else {
            return Ok(false);
        };
        if device_pub_key_b64.is_empty() {
            return Ok(false);
        }
        // 已有包裹则不重复补发
        let existing_key = ikey_key(effective, my_peer, peer);
        if storage.get(&existing_key)?.is_some() {
            return Ok(false);
        }

        // D2 兜底锚定（幂等）：若持有会话 Kverify，且该设备 pwack.vTs 高于当前
        // 可信锚，先尝试 verify_and_anchor_ack。预筛只决定是否试锚定，采信只走 MAC。
        if let Some(k) = kverify {
            let last = pw::get_last_verified_vts(storage, peer)?;
            if let Some(ack) = pw::get_pwack(storage, peer)? {
                if ack.v_ts > last {
                    let _ = pw::verify_and_anchor_ack(storage, peer, k, now_ms);
                }
            }
        }

        // D′ ack 门控：未覆盖最新 pwv 的设备暂扣新 epoch 密钥（pwv 缺失恒过）。
        match pw::should_gate(storage, peer, now_ms as u64)? {
            GateDecision::Pass => {}
            GateDecision::Gated {
                latest_vts,
                last_verified_vts,
                grace_remaining_ms,
            } => {
                DeviceService::append_security_log(
                    storage,
                    "pw_grant_gated",
                    json!({
                        "peer": peer,
                        "epoch": effective,
                        "latestVTs": latest_vts,
                        "lastVerifiedVTs": last_verified_vts,
                        "graceRemainingMs": grace_remaining_ms,
                    }),
                    now_ms,
                )?;
                return Ok(false);
            }
        }

        let epoch_key = get_local_key(storage, effective)?.ok_or_else(|| {
            EpochError::Other(format!("local epoch key missing for {effective}"))
        })?;
        let self_x25519_priv = crate::p2p::identity_store::load_x25519_private_key(storage)
            .ok_or_else(|| {
                EpochError::Other("cannot load x25519 private key for grant".to_string())
            })?;
        let recipient_ed_pk_bytes = B64.decode(device_pub_key_b64).map_err(|e| {
            EpochError::Other(format!("invalid device_pub_key base64 for {peer}: {e}"))
        })?;
        let recipient_ed_pk: [u8; 32] = recipient_ed_pk_bytes.try_into().map_err(|_| {
            EpochError::Other(format!("device_pub_key for {peer} is not 32 bytes"))
        })?;
        let Some(recipient_x25519_pub) = ed_pk_to_x25519(&recipient_ed_pk) else {
            return Ok(false);
        };

        // 检查同 writer（避免已存在同 recipient 其他 writer 的包裹时重复）
        // 简单：只要本机（my_peer）作为 writer 的包裹不存在就发（上面已检查）。
        let Some((wrapped, nonce)) = box_ikey(
            &epoch_key,
            &recipient_x25519_pub,
            &self_x25519_priv,
            &root_id,
            effective,
            my_peer,
            peer,
        ) else {
            return Ok(false);
        };
        let record = IkeyRecord {
            wrapped_key: wrapped,
            nonce,
            ts: now_ms,
        };
        let value = record.to_json()?;
        put_personal(storage, node_id, &existing_key, &value, now_ms)?;
        DeviceService::append_security_log(
            storage,
            "epoch_key_granted",
            json!({
                "epoch": effective,
                "recipientPeer": peer,
                "delay_hours": 0,
            }),
            now_ms,
        )?;
        Ok(true)
    }
}

// ── should_encrypt 分类矩阵 ──────────────────────────────────────────

/// 某条个人域记录在推送前的处理决策（方案 §5.3 / §5.7）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EncryptDecision {
    /// 用当前 effective epoch 密钥加密后推送。
    Encrypt,
    /// 明文推送（基础设施/本地密钥/豁免记录）。
    Plain,
    /// 声明缺失或 key 不在注册 category，跳过不推。
    Skip,
}

/// 判断某条个人域记录是否应在推送前用当前 effective epoch 密钥加密。
///
/// 矩阵（方案 §5.3 / §13.3）：
/// - effective == 0 → Plain（未初始化）。
/// - `pdecl:` / `epoch:` / `ikey:` / `pwv:` / `pwack:` / `ldoc:` → Plain
///   （基础设施/口令校验器/本地）。
/// - `pdoc:` → 按对应 `pdecl:` 声明的 `sensitivity` 字段；声明缺失 → Skip。
/// - 其余已注册个人域 category（ct:, device, profile:self, msg:conv,
///   org:meta, ct:org, org:inv, orgkey, msg:item, msg:app）→ Encrypt。
/// - 未注册前缀 → Skip。
pub fn classify_for_push<S: StorageBackend>(
    storage: &S,
    key: &str,
    effective_epoch: u64,
) -> Result<EncryptDecision> {
    if effective_epoch == 0 {
        return Ok(EncryptDecision::Plain);
    }

    if key.starts_with("pdecl:")
        || key.starts_with("epoch:")
        || key.starts_with("ikey:")
        || key.starts_with("pwv:")
        || key.starts_with("pwack:")
    {
        return Ok(EncryptDecision::Plain);
    }
    if key.starts_with("ldoc:") {
        return Ok(EncryptDecision::Plain);
    }

    if key.starts_with("msg:item:") || key.starts_with("msg:app:") {
        return Ok(EncryptDecision::Encrypt);
    }

    if let Some(stripped) = key.strip_prefix("pdoc:") {
        return classify_pdoc(storage, stripped);
    }

    // 其余注册 category：ct:, device, profile:self, msg:conv, org:meta,
    // ct:org, org:inv, orgkey 均加密。
    if let Some(cat) = category_for_key(key) {
        if cat.name == "pdecl" || cat.name == "pdoc" {
            Ok(EncryptDecision::Skip)
        } else {
            Ok(EncryptDecision::Encrypt)
        }
    } else {
        Ok(EncryptDecision::Skip)
    }
}

/// 解析 `pdoc:` 记录键并查 `pdecl:` 声明判断 sensitivity。
/// 声明 Sensitive → Encrypt；声明缺失 → Skip；Normal → Plain（按方案 §5.3 / §9）。
fn classify_pdoc<S: StorageBackend>(storage: &S, stripped: &str) -> Result<EncryptDecision> {
    // stripped = "{name}@v{version}:{doc_key}"
    let Some((decl_stem, _)) = stripped.split_once(':') else {
        // 键格式异常，按不推处理。
        return Ok(EncryptDecision::Skip);
    };
    let decl_key = format!("pdecl:{decl_stem}");
    match storage.get(&decl_key)? {
        Some(raw) => {
            let decl: CollectionDeclaration =
                serde_json::from_str(&raw).map_err(EpochError::Serde)?;
            if decl.is_sensitive() {
                Ok(EncryptDecision::Encrypt)
            } else {
                Ok(EncryptDecision::Plain)
            }
        }
        None => Ok(EncryptDecision::Skip), // 声明缺失不推
    }
}

#[deprecated(note = "use classify_for_push which supports Skip semantics")]
#[doc(hidden)]
pub fn should_encrypt<S: StorageBackend>(
    storage: &S,
    key: &str,
    effective_epoch: u64,
) -> Result<bool> {
    Ok(matches!(
        classify_for_push(storage, key, effective_epoch)?,
        EncryptDecision::Encrypt
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::p2p::constants::P2P_IDENTITY_PRIVATE_KEY;
    use crate::storage::MemoryStorage;
    use libp2p::identity::Keypair;

    /// libp2p Ed25519 keypair（确定性 seed 的设备身份，模拟单台设备）。
    fn device(seed: u8) -> Keypair {
        Keypair::generate_ed25519()
    }

    /// device 的 X25519 私钥（epoch box/unbox 用；与 load_x25519_private_key 派生一致）。
    fn x_priv(key: &Keypair) -> [u8; 32] {
        let ed = key.clone().try_into_ed25519().unwrap();
        let bytes = ed.to_bytes();
        crate::sync::orgsync::ed_sk_to_x25519(&bytes[..32].try_into().unwrap())
    }

    /// device 的 X25519 公钥。
    fn x_pub(key: &Keypair) -> [u8; 32] {
        let ed = key.clone().try_into_ed25519().unwrap();
        crate::sync::orgsync::ed_pk_to_x25519(&ed.public().to_bytes()).unwrap()
    }

    /// 把设备 libp2p keypair 写入存储（P2P_IDENTITY_PRIVATE_KEY），使
    /// `load_x25519_private_key` 派生的私钥与 `x_priv(dev)` 一致。
    fn persist_identity(storage: &mut MemoryStorage, dev: &Keypair) {
        let raw = dev.to_protobuf_encoding().unwrap();
        storage.put(P2P_IDENTITY_PRIVATE_KEY, &B64.encode(raw)).unwrap();
    }

    /// 根因验证（2026-08-13）：两设备各自 `maybe_init`/`rotate(Init)` 到相同
    /// epoch 号，但各自 `generate_ikey()` 生成**不同**的 epoch 密钥 → 互相加密
    /// 后对方解不开 → `unwrap-value-failed` → 数据被静默丢弃 → vv 永不收敛 →
    /// Concurrent 风暴。
    #[test]
    fn divergent_epoch_key_breaks_cross_device_decrypt() {
        let dev_a = device(1); // 设备 A（W）
        let dev_b = device(2); // 设备 B（K）
        let root_id = "root-xyz";
        let epoch = 1u64;

        // 两端各自独立轮换（每个设备首次启动都 rotate(Init) 到 epoch 1）→ 各自
        // 生成自己的 epoch 密钥，且**不共享**（模拟 ikey 未送达 / 各自主导）。
        let key_a = generate_ikey();
        let key_b = generate_ikey();
        assert_ne!(key_a, key_b, "两端各自主导的 epoch 密钥应不同（根因前提）");

        // A 用 key_a 加密 profile:self，推给 B。
        let wrapped = wrap_value(&key_a, "profile:self", epoch, r#"{"nickname":"W"}"#)
            .expect("A wrap 应成功");
        // B 用自己主导的 key_b 解密 → 失败（复现 unwrap-value-failed）。
        assert!(
            unwrap_value(&key_b, "profile:self", &wrapped).is_none(),
            "两端各自主导不同 epoch 密钥时，对端解不开加密记录（unwrap-value-failed）"
        );

        // 对照：若 B 通过 ikey 正确拿到 A 的 key_a，则能解开（证明"密钥同步正确即收敛"）。
        let via_box = unwrap_value(&key_a, "profile:self", &wrapped).expect("正确密钥可解密");
        assert!(via_box.contains("W"));
    }

    /// 修复验证（2026-08-13，方案 X）：两端各自 `rotate(Init)` 到同 epoch（各生成
    /// 不同密钥、effective 都推进到与 current 相同），随后互灌 ikey，且 `epoch:state`
    /// 经 pdsync LWW 收敛到同一 rotated_by（假设 W 赢）。验证：
    /// - 赢家 W（rotated_by==自己）保留自己密钥 keyW；
    /// - 败者 K 只采用赢家 W 的包裹 → 收敛到 keyW；
    /// - 两端密钥一致 → profile:self 解密成功（风暴消除）。
    #[test]
    fn divergent_init_converges_via_ikey_refresh() {
        let dev_w = device(1); // 手机端 W
        let dev_k = device(2); // 桌面端 K
        let root_id = "root-conv";
        let epoch = 1u64;

        let mut s_w = MemoryStorage::new();
        let mut s_k = MemoryStorage::new();
        persist_identity(&mut s_w, &dev_w);
        persist_identity(&mut s_k, &dev_k);
        let w_pk = B64.encode(dev_w.clone().try_into_ed25519().unwrap().public().to_bytes());
        let k_pk = B64.encode(dev_k.clone().try_into_ed25519().unwrap().public().to_bytes());
        crate::device::DeviceService::upsert_self(&mut s_w, "peerW", 1000, "nodeW", "t", Some(w_pk.clone())).unwrap();
        crate::device::DeviceService::upsert_self(&mut s_k, "peerK", 1000, "nodeK", "t", Some(k_pk.clone())).unwrap();
        crate::device::DeviceService::upsert_self(&mut s_w, "peerK", 1000, "nodeK", "t", Some(k_pk.clone())).unwrap();
        crate::device::DeviceService::upsert_self(&mut s_k, "peerW", 1000, "nodeW", "t", Some(w_pk.clone())).unwrap();

        // 两端各 init 到 epoch 1：各自密钥不同，各自 effective=1。
        let key_w = generate_ikey();
        let key_k = generate_ikey();
        assert_ne!(key_w, key_k, "两端各自主导的 epoch 密钥应不同");
        put_local_key(&mut s_w, 1, &key_w).unwrap();
        put_effective(&mut s_w, 1).unwrap();
        put_local_key(&mut s_k, 1, &key_k).unwrap();
        put_effective(&mut s_k, 1).unwrap();

        // epoch:state 经 pdsync LWW 收敛到单一 rotated_by=peerW（W 是赢家）：
        // 两端 state 一致地认为 rotated_by=peerW。
        put_epoch_state(&mut s_w, "nodeW", &EpochState { current: 1, rotated_at: 2000, rotated_by: "peerW".into(), reason: RotationReason::Init }, 2000).unwrap();
        put_epoch_state(&mut s_k, "nodeK", &EpochState { current: 1, rotated_at: 2000, rotated_by: "peerW".into(), reason: RotationReason::Init }, 2000).unwrap();

        // 互灌 ikey：W 把 keyW box 给 K；K 把 keyK box 给 W。
        let (wrapped_wk, nonce_wk) = box_ikey(&key_w, &x_pub(&dev_k), &x_priv(&dev_w), root_id, 1, "peerW", "peerK").unwrap();
        let ikey_wk = IkeyRecord { wrapped_key: wrapped_wk, nonce: nonce_wk, ts: 1000 };
        s_k.put(&ikey_key(1, "peerW", "peerK"), &serde_json::to_string(&ikey_wk).unwrap()).unwrap();
        let (wrapped_kw, nonce_kw) = box_ikey(&key_k, &x_pub(&dev_w), &x_priv(&dev_k), root_id, 1, "peerK", "peerW").unwrap();
        let ikey_kw = IkeyRecord { wrapped_key: wrapped_kw, nonce: nonce_kw, ts: 1000 };
        s_w.put(&ikey_key(1, "peerK", "peerW"), &serde_json::to_string(&ikey_kw).unwrap()).unwrap();

        // 方案 X：赢家 W 保留自己密钥（rotated_by==自己直接返回）；败者 K 采用赢家 W 的包裹。
        EpochService::try_refresh_keys(&mut s_w, root_id, "peerW", 1001).unwrap();
        EpochService::try_refresh_keys(&mut s_k, root_id, "peerK", 1001).unwrap();

        // 两端应收敛到同一把 = keyW（赢家 W 的密钥）。
        let final_w = get_local_key(&s_w, 1).unwrap().expect("W 应有 epoch 1 密钥");
        let final_k = get_local_key(&s_k, 1).unwrap().expect("K 应有 epoch 1 密钥");
        assert_eq!(final_w, key_w, "赢家 W 保留自己密钥");
        assert_eq!(final_k, key_w, "败者 K 采用赢家 W 的密钥");
        assert_eq!(final_w, final_k, "两端 epoch 密钥收敛一致");

        // 收敛后互解成功（风暴消除）。
        let wrapped = wrap_value(&final_w, "profile:self", 1, r#"{"nickname":"W"}"#).unwrap();
        let plain = unwrap_value(&final_k, "profile:self", &wrapped).expect("收敛后对端可解密 profile:self");
        assert!(plain.contains("W"));
    }

    /// 用 box_ikey/unbox_ikey 验证：只要 ikey 送达，B 能正确拿到 A 的 epoch 密钥。
    #[test]
    fn ikey_delivery_gives_same_epoch_key() {
        let dev_a = device(1);
        let dev_b = device(2);
        let root_id = "root-abc";
        let epoch = 1u64;

        let key_a = generate_ikey();
        let (wrapped, nonce) = box_ikey(
            &key_a,
            &x_pub(&dev_b),
            &x_priv(&dev_a),
            root_id,
            epoch,
            "peerA",
            "peerB",
        )
        .expect("A box 给 B 应成功");

        let received = unbox_ikey(
            &wrapped,
            &nonce,
            &x_pub(&dev_a),
            &x_priv(&dev_b),
            root_id,
            epoch,
            "peerA",
            "peerB",
        )
        .expect("B unbox A 的 ikey 应成功");

        assert_eq!(received, key_a, "ikey 送达后 B 拿到与 A 一致的 epoch 密钥");
    }

    /// 方案 Y（2026-08-14）：恢复加入方跳过 init 语义。
    /// - 无恢复标记（主导/正常创建）→ should_skip = false（照常 init）；
    /// - 有恢复标记且未超时 → should_skip = true（跳过 init，等主导 ikey）；
    /// - 有恢复标记且超时 → 清除标记自升主导，should_skip = false。
    #[test]
    fn recovered_skip_init_semantics() {
        // 主导设备：无恢复标记 → 不跳过。
        let mut s = MemoryStorage::new();
        assert!(!should_skip_init_as_recovered(&mut s).unwrap(), "主导设备照常 init");

        // 恢复加入方：有标记、未超时 → 跳过。
        s.put(RECOVERED_KEY, "1").unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        s.put(RECOVERED_AT_KEY, &now.to_string()).unwrap();
        assert!(should_skip_init_as_recovered(&mut s).unwrap(), "恢复加入方未超时跳过 init");

        // 恢复加入方：等待超时（模拟已等 31s）→ 清除标记自升主导。
        s.put(RECOVERED_AT_KEY, &(now - RECOVERED_ESCAPE_MS - 1000).to_string()).unwrap();
        assert!(
            !should_skip_init_as_recovered(&mut s).unwrap(),
            "恢复加入方超时后自升主导 init"
        );
        assert!(
            s.get(RECOVERED_KEY).unwrap().is_none(),
            "超时自升后清除恢复标记"
        );
    }
}

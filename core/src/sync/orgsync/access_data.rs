//! O4 工作项 4：encrypted 集合本地加解密编排（数据平面缺口补齐）。
//!
//! 对应 org-orgsync.md §20.2.2（encrypted 记录值线形）与 org-data-sync §5
//! 「存储中间件透明加解密，save/read 不暴露密码学给插件」。分工（org-data-sync
//! §5）：内核管密码学机械层，插件只管名单。
//!
//! 本模块把 `access.rs` 的密码学原语（AES-256-GCM、AAD 绑定、epoch 标签）
//! 编排为数据平面的两个方向：
//!
//! - **save**：以当前 epoch 密钥加密插件明文 → 落 `orgd:` 密文对象
//!   `{epoch, nonce, ct}`；本机非 reader（无当前 epoch 密钥）→ `Err(KeyUnavailable)`
//!   ——AEAD 语义下密钥持有者集合 = 写权限集合，无密钥即无写权限；
//! - **get/query**：按记录线形内的 epoch 取对应 epoch 密钥解密 → 返回明文给
//!   插件；无该 epoch 密钥（非 reader 或历史 epoch 未达）→ `Err(KeyUnavailable)`。
//!
//! 纯逻辑（存储泛型），不触碰 p2p/签名信封；orgsync 合入路径保持密文透传
//! （本模块不解密——复制组流量只有密文，见 §20.2.2）。pdsync/驻留裁剪等路径
//! 对密文值透明（不尝试解析 value）。
//!
//! ## 与 access.rs 的分工
//!
//! - `access.rs`：密码学原语（`encrypt_value`/`decrypt_value`/`get_epoch_key`/
//!   acl 读写）；本模块做「当前 epoch 解析 + 密钥可达性 + 键域」的编排。

use serde_json::Value;

use crate::storage::StorageBackend;

use super::access::{
    acl_key, decrypt_value, encrypt_value, get_epoch_key, AclRecord,
};

/// 当前 epoch 密钥不可达（本机非读者 / 未收到该 epoch 密钥）——AEAD 语义下
/// 即无写权限 / 读权限。文案对齐 plugin-data-api §8 的 KeyUnavailable。
#[derive(Debug)]
pub enum AccessDataError {
    /// 本机持有该集合密钥集不完整 / 当前 epoch 密钥缺失。
    KeyUnavailable(String),
    /// 密文线形非法 / AAD 不匹配 / 密钥不匹配（AEAD 在读取方把关，丢弃）。
    BadCiphertext(String),
}

impl std::fmt::Display for AccessDataError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AccessDataError::KeyUnavailable(m) => write!(f, "Key unavailable: {m}"),
            AccessDataError::BadCiphertext(m) => write!(f, "Bad ciphertext: {m}"),
        }
    }
}

/// 读取当前 acl 的 epoch（该集合「当前 epoch」）。acl 缺失/无 epoch → `None`。
pub fn current_acl_epoch<S: StorageBackend>(
    storage: &S,
    org_id: &str,
    name: &str,
    version: &str,
) -> Option<u64> {
    let raw = storage.get(&acl_key(org_id, name, version)).ok()??;
    let acl: AclRecord = serde_json::from_str(&raw).ok()?;
    Some(acl.epoch)
}

/// 取当前 epoch 密钥：current epoch 从 acl 读取，密钥从 orgkey 表取。
/// 无 acl / 无密钥 → `Err(KeyUnavailable)`。
fn current_epoch_key<S: StorageBackend>(
    storage: &S,
    org_id: &str,
    name: &str,
    version: &str,
) -> Result<[u8; 32], AccessDataError> {
    let epoch = current_acl_epoch(storage, org_id, name, version).ok_or_else(|| {
        AccessDataError::KeyUnavailable(format!("no acl for {org_id}:{name}@v{version}"))
    })?;
    get_epoch_key(storage, org_id, name, version, epoch).ok_or_else(|| {
        AccessDataError::KeyUnavailable(format!(
            "no key for epoch {epoch} of {org_id}:{name}@v{version}"
        ))
    })
}

/// **save 方向**：把插件明文加密为 `{epoch, nonce, ct}` 密文 JSON 串（落
/// `orgd:` 用）。以当前 epoch 密钥加密。无当前 epoch 密钥（非 reader）→
/// `Err(KeyUnavailable)`——AEAD 语义下密钥持有者集合 = 写权限集合。
///
/// `rel_key` 为集合内相对数据键（AAD 绑定用）；`plaintext` 为插件 value 的
/// JSON 序列化串。
pub fn encrypt_orgd_value<S: StorageBackend>(
    storage: &S,
    org_id: &str,
    name: &str,
    version: &str,
    rel_key: &str,
    plaintext: &str,
) -> Result<String, AccessDataError> {
    let key = current_epoch_key(storage, org_id, name, version)?;
    let epoch = current_acl_epoch(storage, org_id, name, version)
        .expect("current_epoch_key 已确认 acl 存在");
    let col_full = format!("{name}@v{version}");
    let enc = encrypt_value(org_id, &col_full, rel_key, epoch, &key, plaintext)
        .ok_or_else(|| AccessDataError::KeyUnavailable("encrypt failed".to_string()))?;
    Ok(serde_json::to_string(&enc).unwrap_or_default())
}

/// **get/query 方向**：把 `orgd:` 密文值解密为插件明文 JSON 串。按线形内
/// `epoch` 取该 epoch 密钥解密；无该 epoch 密钥 → `Err(KeyUnavailable)`；
/// 密文非法/AAD/密钥不匹配 → `Err(BadCiphertext)`（AEAD 在读取方把关丢弃）。
pub fn decrypt_orgd_value<S: StorageBackend>(
    storage: &S,
    org_id: &str,
    name: &str,
    version: &str,
    rel_key: &str,
    ciphertext_json: &str,
) -> Result<String, AccessDataError> {
    let ct: Value = serde_json::from_str(ciphertext_json)
        .map_err(|_| AccessDataError::BadCiphertext("not json".to_string()))?;
    let epoch = ct
        .get("epoch")
        .and_then(Value::as_u64)
        .ok_or_else(|| AccessDataError::BadCiphertext("no epoch".to_string()))?;
    let key = get_epoch_key(storage, org_id, name, version, epoch).ok_or_else(|| {
        AccessDataError::KeyUnavailable(format!("no key for epoch {epoch}"))
    })?;
    let col_full = format!("{name}@v{version}");
    decrypt_value(org_id, &col_full, rel_key, &key, &ct)
        .ok_or_else(|| AccessDataError::BadCiphertext("decrypt failed".to_string()))
}

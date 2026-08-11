//! O4 encrypted 集合访问控制与密钥机械层（纯逻辑）。
//!
//! 对应 org-orgsync.md §20.2.2（encrypted 值线形）、§20.6（orgkey-deliver）、
//! §20.7（org:acl 记录与合并规则）、§20.9（常量）。分工（org-data-sync §5）：
//! 内核管密码学机械层（本模块），插件只管名单（见 kernel `data_grant_access`
//! 家族），owner 独立于 spark 管理员，重置接管只能重启读不到历史。
//!
//! 本模块**纯逻辑**（存储泛型 + 密码学原语），不触碰 p2p/签名信封装配——
//! orgkey-deliver 的实际投递由 kernel 层完成。
//!
//! ## 密钥域（personal 域，经 pdsync 自设备扩散，永不进 orgsync 组织流量）
//!
//! `orgkey:{orgId}:{name}@v{version}:{epoch}` = 32B 集合对称密钥（AES-256-GCM）。
//! acl 键 `org:acl:{orgId}:{name}@v{version}` = 授权名单（all-members 系统数据）。

use aes_gcm::aead::{Aead, Nonce};
use aes_gcm::{Aes256Gcm, KeyInit};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use curve25519_dalek::edwards::CompressedEdwardsY;
use curve25519_dalek::montgomery::MontgomeryPoint;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use rand::Rng;
use serde_json::{Value, json};
use sha2::{Digest, Sha256, Sha512};

/// orgkey 存储键前缀（personal 域；pdsync category 白名单含此前缀）。
pub const ORGKEY_PREFIX: &str = "orgkey:";
/// org:acl 存储键前缀（all-members 系统数据，进 orgsync 流量）。
pub const ORG_ACL_PREFIX: &str = "org:acl:";
/// orgkey-deliver dm kind。
pub const KIND_ORGKEY_DELIVER: &str = "orgkey-deliver";

/// orgkey 存储键 `orgkey:{orgId}:{name}@v{version}:{epoch}`。
pub fn orgkey_key(org_id: &str, name: &str, version: &str, epoch: u64) -> String {
    format!("{ORGKEY_PREFIX}{org_id}:{name}@v{version}:{epoch}")
}

/// orgkey 存储键前缀 `orgkey:{orgId}:{name}@v{version}:`（扫描本账号持有密钥集）。
pub fn orgkey_prefix(org_id: &str, name: &str, version: &str) -> String {
    format!("{ORGKEY_PREFIX}{org_id}:{name}@v{version}:")
}

/// acl 存储键 `org:acl:{orgId}:{name}@v{version}`。
pub fn acl_key(org_id: &str, name: &str, version: &str) -> String {
    format!("{ORG_ACL_PREFIX}{org_id}:{name}@v{version}")
}

/// 解析 acl 键 → (orgId, name, version)；非 acl 键 → `None`。
pub fn parse_acl_key(key: &str) -> Option<(String, String, String)> {
    let rest = key.strip_prefix(ORG_ACL_PREFIX)?;
    let (org_id, rest) = rest.split_once(':')?;
    let at = rest.rfind("@v")?;
    let name = &rest[..at];
    let version = &rest[at + 2..];
    Some((org_id.to_string(), name.to_string(), version.to_string()))
}

/// 授权名单记录（org-orgsync.md §20.7 线形，全员可见审计面）。
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AclRecord {
    /// 名单 owner（rootId；持有管理权，变更须其签名）。
    #[serde(default)]
    pub owners: Vec<String>,
    /// 读者（rootId；经 orgkey-deliver 收密钥）。
    #[serde(default)]
    pub readers: Vec<String>,
    /// 密钥代际（revoke 时 +1；重置接管重置为 1）。
    #[serde(default)]
    pub epoch: u64,
    /// 变更时间戳（whole 合并的胜负依据，大者胜）。
    #[serde(rename = "updatedAt", default)]
    pub updated_at: i64,
    /// 重置接管标记（spark 管理员写入时为管理员 rootId；正常 owner 变更为 null）。
    #[serde(rename = "resetBy", default)]
    pub reset_by: Option<String>,
    /// 变更方组织身份对固定键序载荷的签名（base64）。
    #[serde(default)]
    pub sig: String,
}

impl AclRecord {
    /// 该成员是否为 reader。
    pub fn is_reader(&self, root_id: &str) -> bool {
        self.readers.iter().any(|r| r == root_id)
    }

    /// 该成员是否为 owner。
    pub fn is_owner(&self, root_id: &str) -> bool {
        self.owners.iter().any(|r| r == root_id)
    }

    /// 名单是否为空（无任何 owner/reader——异常态，通常不发生）。
    pub fn is_empty(&self) -> bool {
        self.owners.is_empty() && self.readers.is_empty()
    }
}

/// acl 签名载荷（固定键序，org-orgsync.md §20.7）：
/// `{"epoch":..,"orgId":..,"collection":..,"owners":[...],"readers":[...],"resetBy":..,"updatedAt":..}`。
/// `collection` 为线上集合标识 `{name}@v{version}`。
pub fn acl_sign_payload(
    epoch: u64,
    org_id: &str,
    collection: &str,
    owners: &[String],
    readers: &[String],
    reset_by: Option<&str>,
    updated_at: i64,
) -> String {
    json!({
        "epoch": epoch,
        "orgId": org_id,
        "collection": collection,
        "owners": owners,
        "readers": readers,
        "resetBy": reset_by,
        "updatedAt": updated_at,
    })
    .to_string()
}

/// 以组织域身份对 acl 载荷签名。`domain` 为组织域身份域串
/// （见 `kernel::identity::profile::sign_with_domain_identity`）。
/// 返回 base64 签名。本函数纯签名（调用方提供签名私钥）——kernel 层用
/// `sign_with_domain_identity` 派生组织身份私钥后调用。
pub fn acl_sign(signing_key: &SigningKey, payload: &str) -> String {
    B64.encode(signing_key.sign(payload.as_bytes()).to_bytes())
}

/// 校验 acl 记录签名：载荷重建后按 `signer_pk`（组织身份公钥，32B）验签。
/// `collection` 为 `{name}@v{version}`。
pub fn acl_verify(
    acl: &AclRecord,
    org_id: &str,
    collection: &str,
    signer_pk: &VerifyingKey,
) -> bool {
    let payload = acl_sign_payload(
        acl.epoch,
        org_id,
        collection,
        &acl.owners,
        &acl.readers,
        acl.reset_by.as_deref(),
        acl.updated_at,
    );
    let Ok(sig_bytes) = B64.decode(&acl.sig) else {
        return false;
    };
    let Ok(sig_arr) = <[u8; 64]>::try_from(sig_bytes.as_slice()) else {
        return false;
    };
    signer_pk
        .verify(payload.as_bytes(), &Signature::from_bytes(&sig_arr))
        .is_ok()
}

/// 组织身份公钥 → X25519 公钥（Ed25519→X25519 标准转换，tweetnacl 口径）。
/// 即取 ed25519 公钥对应 Edwards 点转换到 Montgomery u 坐标。
pub fn ed_pk_to_x25519(ed_pk: &[u8; 32]) -> Option<[u8; 32]> {
    let edwards = CompressedEdwardsY(*ed_pk).decompress()?;
    Some(edwards.to_montgomery().to_bytes())
}

/// 组织身份私钥（ed25519 seed）→ X25519 私钥标量：`clamp(sha512(seed)[0..32])`，
/// 与 NaCl 的 ed25519_sk_to_curve25519（TweetNaCl `crypto_sign_ed25519_sk_to_curve25519`）
/// 同口径——该标量即 ed25519 私钥派生出的签名标量，与 `SigningKey::verifying_key()`
/// 对应，保证 box/unbox 的 DH 共享密钥一致。
pub fn ed_sk_to_x25519(ed_sk_seed: &[u8; 32]) -> [u8; 32] {
    let h = Sha512::digest(ed_sk_seed);
    let mut scalar = [0u8; 32];
    scalar.copy_from_slice(&h[..32]);
    scalar[0] &= 248;
    scalar[31] &= 127;
    scalar[31] |= 64;
    scalar
}

/// 值加密（org-orgsync.md §20.2.2）：
/// - 算法 AES-256-GCM，密钥 = 该集合该 epoch 的 32B 对称密钥；
/// - 明文字节 = 插件 value JSON 序列化 UTF-8；
/// - AAD = `"{orgId}:{collection}:{key}"`（collection 为 `{name}@v{version}`，
///   key 为集合内相对数据键；防密文跨记录/跨集合搬迁）；
/// - 输出线形 `{ "epoch":.., "nonce":"<base64 12B>", "ct":"<base64>" }`。
/// 失败（key 长度非法）→ `None`。
pub fn encrypt_value(
    org_id: &str,
    collection: &str,
    key: &str,
    epoch: u64,
    epoch_key: &[u8; 32],
    plaintext: &str,
) -> Option<Value> {
    let cipher = Aes256Gcm::new_from_slice(epoch_key).ok()?;
    let mut nonce_bytes = [0u8; 12];
    rand::rng().fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::<Aes256Gcm>::from(nonce_bytes);
    let aad = format!("{org_id}:{collection}:{key}");
    let payload = aes_gcm::aead::Payload {
        msg: plaintext.as_bytes(),
        aad: aad.as_bytes(),
    };
    let ct = cipher.encrypt(&nonce, payload).ok()?;
    Some(json!({
        "epoch": epoch,
        "nonce": B64.encode(nonce_bytes),
        "ct": B64.encode(ct),
    }))
}

/// 值解密（§20.2.2 逆）：验 AAD 后解出明文。密文线形不合法 / AAD 不匹配 /
/// 密钥不对 → `None`（AEAD 在读取方把关，非密钥持有者构造的写入解密失败被丢弃）。
pub fn decrypt_value(
    org_id: &str,
    collection: &str,
    key: &str,
    epoch_key: &[u8; 32],
    ciphertext: &Value,
) -> Option<String> {
    let _epoch = ciphertext.get("epoch")?.as_u64()?;
    let nonce_b64 = ciphertext.get("nonce")?.as_str()?;
    let ct_b64 = ciphertext.get("ct")?.as_str()?;
    let nonce_raw = B64.decode(nonce_b64).ok()?;
    let nonce_arr: [u8; 12] = nonce_raw.try_into().ok()?;
    let ct = B64.decode(ct_b64).ok()?;
    let cipher = Aes256Gcm::new_from_slice(epoch_key).ok()?;
    let aad = format!("{org_id}:{collection}:{key}");
    let payload = aes_gcm::aead::Payload {
        msg: ct.as_ref(),
        aad: aad.as_bytes(),
    };
    let plain = cipher
        .decrypt(&Nonce::<Aes256Gcm>::from(nonce_arr), payload)
        .ok()?;
    Some(String::from_utf8(plain).ok()?)
}

/// 读取本账号某集合某 epoch 的对称密钥（personal 域 orgkey 表）。
pub fn get_epoch_key<S: crate::storage::StorageBackend>(
    storage: &S,
    org_id: &str,
    name: &str,
    version: &str,
    epoch: u64,
) -> Option<[u8; 32]> {
    let raw = storage.get(&orgkey_key(org_id, name, version, epoch)).ok()??;
    let b64 = raw.trim();
    let bytes = B64.decode(b64).ok()?;
    bytes.try_into().ok()
}

/// 写 orgkey 表（personal 域；仅经 pdsync 自设备扩散）。
pub fn put_epoch_key<S: crate::storage::StorageBackend>(
    storage: &mut S,
    org_id: &str,
    name: &str,
    version: &str,
    epoch: u64,
    key: &[u8; 32],
) {
    let _ = storage.put(&orgkey_key(org_id, name, version, epoch), &B64.encode(key));
}

/// 生成随机 32B 集合对称密钥（AES-256-GCM）。
pub fn generate_epoch_key() -> [u8; 32] {
    let mut k = [0u8; 32];
    rand::rng().fill_bytes(&mut k);
    k
}

// ── O5 orgkey 离线投递 pending ─────────────────────────────────────────

/// orgkey 离线投递 pending 前缀（本机待重投的 orgkey-deliver 目标）：
/// `orgkey:pending:{orgId}:{collection}:{recipientRootId}:{epoch}` = `{ts}`。
/// 投递失败（收件人离线/无 accessKey）落此键，收到对方 orgsync-hello（上线）
/// 时扫描重投。本键是**本地键**（不进同步流量），重投成功后删除。
pub const ORGKEY_PENDING_PREFIX: &str = "orgkey:pending:";

/// orgkey pending 键 `orgkey:pending:{orgId}:{collection}:{recipientRootId}:{epoch}`。
pub fn orgkey_pending_key(
    org_id: &str,
    collection: &str,
    recipient_root_id: &str,
    epoch: u64,
) -> String {
    format!("{ORGKEY_PENDING_PREFIX}{org_id}:{collection}:{recipient_root_id}:{epoch}")
}

/// 记录一条待重投的 orgkey-deliver（离线/不可达时落；幂等覆盖同键）。
pub fn orgkey_pending_put<S: crate::storage::StorageBackend>(
    storage: &mut S,
    org_id: &str,
    collection: &str,
    recipient_root_id: &str,
    epoch: u64,
    ts: i64,
) {
    let _ = storage.put(
        &orgkey_pending_key(org_id, collection, recipient_root_id, epoch),
        &ts.to_string(),
    );
}

/// 删除一条已投递成功的 orgkey pending 键。
pub fn orgkey_pending_remove<S: crate::storage::StorageBackend>(
    storage: &mut S,
    org_id: &str,
    collection: &str,
    recipient_root_id: &str,
    epoch: u64,
) {
    let _ = storage.delete(&orgkey_pending_key(org_id, collection, recipient_root_id, epoch));
}

/// 读取本机某组织的全部 orgkey pending 条目 → `(collection, recipientRootId, epoch, ts)`。
/// 收到对端 orgsync-hello（上线）时按其 rootId 筛选重投。
pub fn orgkey_pending_for_org<S: crate::storage::StorageBackend>(
    storage: &S,
    org_id: &str,
) -> Vec<(String, String, u64, i64)> {
    let prefix = format!("{ORGKEY_PENDING_PREFIX}{org_id}:");
    storage
        .scan(&crate::storage::ScanOptions::prefix(&prefix))
        .map(|entries| {
            entries
                .into_iter()
                .filter_map(|(key, raw)| {
                    let rest = key.strip_prefix(&prefix)?;
                    // rest = {collection}:{recipientRootId}:{epoch}；collection
                    // 含插件前缀分隔符 `:`（如 ai-chat:payroll@v1.0.0），recipient
                    // 为 64hex（无冒号）、epoch 为数字——从**右**解析：
                    // 末段 epoch、次末段 recipient、剩余为 collection。
                    let (recipient_epoch, epoch_str) = rest.rsplit_once(':')?;
                    let (collection, recipient) = recipient_epoch.rsplit_once(':')?;
                    let epoch = epoch_str.parse::<u64>().ok()?;
                    let ts = raw.parse::<i64>().unwrap_or(0);
                    Some((collection.to_string(), recipient.to_string(), epoch, ts))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// 本账号 orgkey 表已知的最大 epoch（扫描 `orgkey:{orgId}:{name}@v{version}:` 前缀，
/// 解析各键尾部 epoch 取 max）。无任何密钥 → `None`。
///
/// O3：reset 接管用（不复用历史 epoch 号）——避免「旧读者旧 epoch-N 密钥 vs
/// 新 epoch-N 投递幂等丢弃」的双向分裂（§20.7 resetBy 例外显式化）。
pub fn max_known_epoch<S: crate::storage::StorageBackend>(
    storage: &S,
    org_id: &str,
    name: &str,
    version: &str,
) -> Option<u64> {
    let prefix = orgkey_prefix(org_id, name, version);
    let mut max: Option<u64> = None;
    for (key, _) in storage
        .scan(&crate::storage::ScanOptions::prefix(&prefix))
        .ok()?
    {
        if let Some(epoch_str) = key.strip_prefix(&prefix)
            && let Ok(epoch) = epoch_str.parse::<u64>()
        {
            max = Some(max.map_or(epoch, |m| m.max(epoch)));
        }
    }
    max
}

/// orgkey-deliver 签名载荷（固定键序，org-orgsync.md §20.6，含 recipientRootId
/// 防转投）：
/// `{"collection":..,"epoch":..,"nonce":..,"orgId":..,"recipientRootId":..,"ts":..,"wrappedKey":..}`。
pub fn deliver_sign_payload(
    collection: &str,
    epoch: u64,
    nonce: &str,
    org_id: &str,
    recipient_root_id: &str,
    ts: i64,
    wrapped_key: &str,
) -> String {
    json!({
        "collection": collection,
        "epoch": epoch,
        "nonce": nonce,
        "orgId": org_id,
        "recipientRootId": recipient_root_id,
        "ts": ts,
        "wrappedKey": wrapped_key,
    })
    .to_string()
}

/// orgkey-deliver 信封结构（§20.6）。`wrapped_key` = crypto_box(epochKey, nonce,
/// recipientOrgPubX25519, ownerOrgPrivX25519)；`sig` = sender 组织身份 Ed25519。
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct OrgkeyDeliver {
    #[serde(rename = "orgId")]
    pub org_id: String,
    /// 集合名（§20.6 线形用 name@v{version} 线上标识）。
    pub collection: String,
    pub epoch: u64,
    #[serde(rename = "wrappedKey")]
    pub wrapped_key: String,
    /// 24B 随机 nonce（base64）。
    pub nonce: String,
    #[serde(rename = "senderRootId")]
    pub sender_root_id: String,
    #[serde(rename = "recipientRootId")]
    pub recipient_root_id: String,
    pub ts: i64,
    pub sig: String,
}

/// 解析 orgkey-deliver 信封 body（§20.6）→ `OrgkeyDeliver`；线形非法 → `None`。
pub fn parse_orgkey_deliver(body: &serde_json::Value) -> Option<OrgkeyDeliver> {
    serde_json::from_value(body.clone()).ok()
}

/// 构建 orgkey-deliver body（§20.6 出站侧，纯逻辑）：
///
/// - `wrapped_key` = crypto_box(epochKey, nonce24, recipient 组织身份公钥 X25519,
///   owner 组织身份私钥 X25519)；
/// - `sig` = owner 组织身份 Ed25519 对固定键序载荷
///   `{"collection","epoch","nonce","orgId","recipientRootId","ts","wrappedKey"}`
///   的签名（含 recipientRootId 防转投）。
///
/// `owner_org_pk` 为 owner 组织身份公钥（Ed25519 32B，用于验签方取公钥，
/// 但不进 body——body 只带 wrappedKey/nonce/sig 等）。返回 (body, wrappedKey)。
///
/// `recipient_root_id` 为收件人 rootId；`collection` 为 `{name}@v{version}`。
pub fn build_orgkey_deliver(
    org_id: &str,
    name: &str,
    version: &str,
    epoch: u64,
    epoch_key: &[u8; 32],
    sender_root_id: &str,
    recipient_root_id: &str,
    recipient_x25519: &[u8; 32],
    owner_org_signing_key: &SigningKey,
    owner_x25519_priv: &[u8; 32],
    now_ms: i64,
) -> Option<Value> {
    let collection = format!("{name}@v{version}");
    let (wrapped_key, nonce24) = box_epoch_key(
        epoch_key,
        recipient_x25519,
        owner_x25519_priv,
        org_id,
        &collection,
        sender_root_id,
        recipient_root_id,
    )?;
    let ts = now_ms;
    let payload = deliver_sign_payload(
        &collection, epoch, &nonce24, org_id, recipient_root_id, ts, &wrapped_key,
    );
    let sig = B64.encode(owner_org_signing_key.sign(payload.as_bytes()).to_bytes());
    Some(json!({
        "orgId": org_id,
        "collection": collection,
        "epoch": epoch,
        "wrappedKey": wrapped_key,
        "nonce": nonce24,
        "senderRootId": sender_root_id,
        "recipientRootId": recipient_root_id,
        "ts": ts,
        "sig": sig,
    }))
}

/// 域分隔派生字节（H1b）：把 shared DH 结果与协议上下文绑定——
/// `orgkey-box\x00{org_id}\x00{collection}\x00{senderRoot}\x00{recipientRoot}`，
/// 防同一 DH 共享被复用于其它集合/对端（防跨组织/跨集合搬迁）。
/// 无 PFS 取舍：orgkey-deliver 是密钥投递而非会话加密，前向保密不在威胁模型
/// 内（注释说明，对齐 §20.6）。
fn box_domain_info(
    org_id: &str,
    collection: &str,
    sender_root_id: &str,
    recipient_root_id: &str,
) -> Vec<u8> {
    let mut info = b"orgkey-box\0".to_vec();
    info.extend_from_slice(org_id.as_bytes());
    info.push(0);
    info.extend_from_slice(collection.as_bytes());
    info.push(0);
    info.extend_from_slice(sender_root_id.as_bytes());
    info.push(0);
    info.extend_from_slice(recipient_root_id.as_bytes());
    info
}

/// 域分隔共享 → AES-256 密钥（H1b）：`sha256(shared || domain_info)`——双参数
/// 绑定上下文，防共享被跨上下文误用。
fn derive_box_key(shared_bytes: &[u8; 32], domain_info: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(shared_bytes);
    hasher.update(domain_info);
    hasher.finalize().into()
}

/// crypto_box 包裹（§20.6）：以 recipient 组织身份公钥 X25519 与 owner 组织身份
/// 私钥 X25519 派生共享密钥，AES-256-GCM 加密 32B epoch 密钥，nonce 24B 随机。
/// `recipient_x25519` 为 recipient 公钥的 X25519 形式；`owner_x25519_priv` 为
/// owner 私钥的 X25519 标量。返回 (wrappedKey, nonce24)。
/// `collection` 为 `{name}@v{version}`（H1b 域分隔上下文）。
pub fn box_epoch_key(
    epoch_key: &[u8; 32],
    recipient_x25519: &[u8; 32],
    owner_x25519_priv: &[u8; 32],
    org_id: &str,
    collection: &str,
    sender_root_id: &str,
    recipient_root_id: &str,
) -> Option<(String, String)> {
    let shared = MontgomeryPoint(*recipient_x25519).mul_clamped(*owner_x25519_priv);
    let shared_bytes = shared.to_bytes();
    // H1a：拒低阶点（X25519 低阶点乘积为全零/低阶共享）——防强制 DH 落低阶
    // 子群、共享密钥可预测。全零即低阶点特征；此处显式拒绝。
    if shared_bytes.iter().all(|&b| b == 0) {
        return None;
    }
    // H1b：域分隔派生（绑 org|collection|sender|recipient）
    let domain_info = box_domain_info(org_id, collection, sender_root_id, recipient_root_id);
    let box_key = derive_box_key(&shared_bytes, &domain_info);
    // H1a：恒零共享密钥拒（低阶点必然产生；双保险）
    if box_key.iter().all(|&b| b == 0) {
        return None;
    }
    let cipher = Aes256Gcm::new_from_slice(&box_key).ok()?;
    let mut nonce24 = [0u8; 24];
    rand::rng().fill_bytes(&mut nonce24);
    // 24B nonce 前 12B 作 AES-GCM nonce（与值加密同族）；AAD 绑定防 box 密文搬迁
    let mut nonce_arr = [0u8; 12];
    nonce_arr.copy_from_slice(&nonce24[..12]);
    let ct = cipher
        .encrypt(&Nonce::<Aes256Gcm>::from(nonce_arr), epoch_key.as_slice())
        .ok()?;
    Some((B64.encode(ct), B64.encode(nonce24)))
}

/// 解包 orgkey-deliver（§20.6）：recipient 组织身份私钥 X25519 + sender 组织身份
/// 公钥 X25519 派生共享密钥，AES-256-GCM 解密 wrappedKey → 32B epoch 密钥。
/// 失败 → `None`。
pub fn unbox_epoch_key(
    wrapped_key: &str,
    nonce24: &str,
    sender_x25519: &[u8; 32],
    recipient_x25519_priv: &[u8; 32],
    org_id: &str,
    collection: &str,
    sender_root_id: &str,
    recipient_root_id: &str,
) -> Option<[u8; 32]> {
    let ct = B64.decode(wrapped_key).ok()?;
    let nonce_raw = B64.decode(nonce24).ok()?;
    if nonce_raw.len() < 12 {
        return None;
    }
    let nonce_arr: [u8; 12] = nonce_raw[..12].try_into().ok()?;
    let shared = MontgomeryPoint(*sender_x25519).mul_clamped(*recipient_x25519_priv);
    let shared_bytes = shared.to_bytes();
    // H1a：拒低阶点（对称 box 侧，与 box_epoch_key 同口径）
    if shared_bytes.iter().all(|&b| b == 0) {
        return None;
    }
    // H1b：域分隔派生（与 box_epoch_key 同上下文，两侧 DH 一致才解得出）
    let domain_info = box_domain_info(org_id, collection, sender_root_id, recipient_root_id);
    let box_key = derive_box_key(&shared_bytes, &domain_info);
    if box_key.iter().all(|&b| b == 0) {
        return None;
    }
    let cipher = Aes256Gcm::new_from_slice(&box_key).ok()?;
    let plain = cipher.decrypt(&Nonce::<Aes256Gcm>::from(nonce_arr), ct.as_ref()).ok()?;
    plain.try_into().ok()
}

/// acl 单记录 whole 合并（§20.7）：`updatedAt` 大者胜；同值保留本地。
/// 验签由调用方在 kernel 层完成（需要签名者 ∈ 变更前 owners 的组织身份公钥，
/// 需访问组织身份派生——见 `kernel::data_grant_access`）。
pub fn acl_merge(current: &AclRecord, incoming: &AclRecord) -> AclRecord {
    if incoming.updated_at > current.updated_at {
        incoming.clone()
    } else {
        current.clone()
    }
}

#[cfg(test)]
#[path = "access_tests.rs"]
mod tests;

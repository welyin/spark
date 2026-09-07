//! 跨组织网关邮箱（阶段四E，org-gateway-mailbox 设计 + p2p-org-mail §21
//! 字节级协议）——信封线形/加密/签名面（纯逻辑，存储面在 `mailbox_store`）。
//!
//! 双层身份红线：rootId 不出本协议线上字段——信封与授权一律用**域身份**
//! （新域串 `org-mail:{orgId}`，与 `org-access:{orgId}` 域分隔）。
//!
//! 加密（§21.3）：两侧域身份 Ed25519→X25519 转换 + `mul_clamped` 静态 DH
//! （低阶点全零共享拒绝，H1a 同口径）→ `boxKey = sha256(shared ‖
//! domain_info)`（H1b 域分隔）→ AES-256-GCM（AAD 绑
//! `{to.orgAddress}:{to.domainId}:{id}` 防跨信封搬迁）。无 PFS 属明示取舍
//! （同 §20.6）。原语族与 orgkey-deliver 的 `box_epoch_key` 同构，上下文串
//! 不同——不复用其函数（域串/nonce 尺寸/AAD 均不同），复用
//! `ed_pk_to_x25519`/`ed_sk_to_x25519` 转换助手。

use aes_gcm::KeyInit as _;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use ed25519_dalek::{Signer, Verifier};
use rand::Rng as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// 邮箱域身份域串（§21.1）：`org-mail:{orgId}`。
pub fn org_mail_domain(org_id: &str) -> String {
    format!("org-mail:{org_id}")
}

/// 信封 TTL 缺省（7 天，对齐 dm_offline PENDING_TTL_MS）。
pub const MAIL_TTL_DEFAULT_MS: i64 = 604_800_000;
/// 信封 TTL 上限（30 天，超限截断非拒收）。
pub const MAIL_TTL_MAX_MS: i64 = 2_592_000_000;
/// 入站新鲜窗：|ts − now| ≤ 10 min（同 dm 信封口径）。
pub const MAIL_TS_FRESHNESS_WINDOW_MS: i64 = 600_000;

/// 邮箱信封（§21.2 字节级线形；serde 字段序 = 线上键序）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgMailEnvelope {
    /// 24 位小写 hex（12B 随机）——投递幂等键。
    pub id: String,
    /// 收件方（组织地址 + 收件人域身份公钥 b64）。
    pub to: OrgMailTo,
    /// 发送方（域身份公钥 b64 + 可省组织地址）。
    pub from: OrgMailFrom,
    /// 发送方本地 Unix 毫秒。
    pub ts: i64,
    /// 存活毫秒（缺省 7 天；超 30 天截断）。
    #[serde(default = "default_ttl")]
    pub ttl: i64,
    /// 12B 随机，base64 标准表。
    pub nonce: String,
    /// AES-256-GCM 输出（密文 ‖ 16B tag），base64 标准表。
    pub ct: String,
    /// 发送方域身份 Ed25519 签名 64B，base64。
    pub sig: String,
}

fn default_ttl() -> i64 {
    MAIL_TTL_DEFAULT_MS
}

/// 信封 `to` 段（线上键序 orgAddress → domainId）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgMailTo {
    /// 收件方组织地址（自认证地址记录线形）。
    #[serde(rename = "orgAddress")]
    pub org_address: String,
    /// 收件人域身份公钥 b64（路由键；拉取侧域名匹配依据）。
    #[serde(rename = "domainId")]
    pub domain_id: String,
}

/// 信封 `from` 段（线上键序 domainId → orgAddress；orgAddress 可省——
/// 缺省时**丢键**（非 null），与签名载荷口径一致）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgMailFrom {
    /// 发送方域身份公钥 b64（验签键 + 投递限流键）。
    #[serde(rename = "domainId")]
    pub domain_id: String,
    /// 发送方组织地址（展示/回信寻址提示，不参与授权）。
    #[serde(rename = "orgAddress", skip_serializing_if = "Option::is_none")]
    pub org_address: Option<String>,
}

/// 域身份公钥的 b64 线形（32B 原始字节，标准表含 padding）。
pub fn domain_id_of(verifying_key: &ed25519_dalek::VerifyingKey) -> String {
    B64.encode(verifying_key.to_bytes())
}

/// 域身份公钥 b64 → 32B（形状非法 → None）。
pub fn parse_domain_id(domain_id: &str) -> Option<[u8; 32]> {
    let raw = B64.decode(domain_id).ok()?;
    <[u8; 32]>::try_from(raw.as_slice()).ok()
}

// ---------------------------------------------------------------------------
// box/unbox（§21.3）
// ---------------------------------------------------------------------------

/// 域分隔派生上下文（§21.3 H1b）：`orgmail-box\x00{toOrgAddress}\x00
/// {fromDomainId}\x00{toDomainId}`（domainId 取 b64 字符串的 UTF-8 字节）。
fn orgmail_box_domain_info(
    to_org_address: &str,
    from_domain_id: &str,
    to_domain_id: &str,
) -> Vec<u8> {
    let mut info = b"orgmail-box\0".to_vec();
    info.extend_from_slice(to_org_address.as_bytes());
    info.push(0);
    info.extend_from_slice(from_domain_id.as_bytes());
    info.push(0);
    info.extend_from_slice(to_domain_id.as_bytes());
    info
}

/// boxKey = sha256(shared ‖ domain_info)（与 access.rs `derive_box_key` 同构）。
fn derive_orgmail_box_key(shared: &[u8; 32], domain_info: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(shared);
    hasher.update(domain_info);
    hasher.finalize().into()
}

/// 信封加密（§21.3）：发送方域身份私钥 × 收件人域身份公钥 DH → AES-256-GCM
/// （AAD 绑 `{to.orgAddress}:{to.domainId}:{id}`）。返回 (nonce12 b64, ct b64)。
/// 低阶点/全零共享密钥拒绝 → None。
pub fn orgmail_box(
    plaintext: &[u8],
    sender_signing_key: &ed25519_dalek::SigningKey,
    recipient_domain_id: &str,
    to_org_address: &str,
    envelope_id: &str,
) -> Option<(String, String)> {
    let mut nonce12 = [0u8; 12];
    rand::rng().fill_bytes(&mut nonce12);
    orgmail_box_with_nonce(
        plaintext,
        sender_signing_key,
        recipient_domain_id,
        to_org_address,
        envelope_id,
        &nonce12,
    )
}

/// [`orgmail_box`] 的确定性 nonce 变体（golden vectors 生成用；生产路径
/// 一律走随机 nonce 入口）。
pub fn orgmail_box_with_nonce(
    plaintext: &[u8],
    sender_signing_key: &ed25519_dalek::SigningKey,
    recipient_domain_id: &str,
    to_org_address: &str,
    envelope_id: &str,
    nonce12: &[u8; 12],
) -> Option<(String, String)> {
    use aes_gcm::aead::{Aead, Payload};
    let recipient_x25519 =
        crate::sync::orgsync::access::ed_pk_to_x25519(&parse_domain_id(recipient_domain_id)?)?;
    let sender_x25519_priv =
        crate::sync::orgsync::access::ed_sk_to_x25519(&sender_signing_key.to_bytes());
    let shared = curve25519_dalek::montgomery::MontgomeryPoint(recipient_x25519)
        .mul_clamped(sender_x25519_priv)
        .to_bytes();
    // H1a：低阶点全零共享拒绝
    if shared.iter().all(|&b| b == 0) {
        return None;
    }
    let sender_domain_id = domain_id_of(&sender_signing_key.verifying_key());
    let box_key = derive_orgmail_box_key(
        &shared,
        &orgmail_box_domain_info(to_org_address, &sender_domain_id, recipient_domain_id),
    );
    // H1a 双保险：恒零密钥拒
    if box_key.iter().all(|&b| b == 0) {
        return None;
    }
    let cipher = aes_gcm::Aes256Gcm::new_from_slice(&box_key).ok()?;
    let aad = format!("{to_org_address}:{recipient_domain_id}:{envelope_id}");
    let nonce = aes_gcm::aead::Nonce::<aes_gcm::Aes256Gcm>::from(*nonce12);
    let ct = cipher
        .encrypt(
            &nonce,
            Payload {
                msg: plaintext,
                aad: aad.as_bytes(),
            },
        )
        .ok()?;
    Some((B64.encode(nonce12), B64.encode(ct)))
}

/// 信封解密（§21.3 对称侧）：收件人域身份私钥 × 发送方域身份公钥。
/// 失败（含 AAD 不吻合/篡改）→ None。
pub fn orgmail_unbox(
    envelope: &OrgMailEnvelope,
    recipient_signing_key: &ed25519_dalek::SigningKey,
) -> Option<Vec<u8>> {
    use aes_gcm::aead::{Aead, Payload};
    let sender_x25519 =
        crate::sync::orgsync::access::ed_pk_to_x25519(&parse_domain_id(&envelope.from.domain_id)?)?;
    let recipient_x25519_priv =
        crate::sync::orgsync::access::ed_sk_to_x25519(&recipient_signing_key.to_bytes());
    let shared = curve25519_dalek::montgomery::MontgomeryPoint(sender_x25519)
        .mul_clamped(recipient_x25519_priv)
        .to_bytes();
    if shared.iter().all(|&b| b == 0) {
        return None;
    }
    let recipient_domain_id = domain_id_of(&recipient_signing_key.verifying_key());
    let box_key = derive_orgmail_box_key(
        &shared,
        &orgmail_box_domain_info(
            &envelope.to.org_address,
            &envelope.from.domain_id,
            &recipient_domain_id,
        ),
    );
    if box_key.iter().all(|&b| b == 0) {
        return None;
    }
    let nonce_raw = B64.decode(&envelope.nonce).ok()?;
    if nonce_raw.len() != 12 {
        return None;
    }
    let nonce: [u8; 12] = nonce_raw.try_into().ok()?;
    let ct = B64.decode(&envelope.ct).ok()?;
    let aad = format!(
        "{}:{}:{}",
        envelope.to.org_address, envelope.to.domain_id, envelope.id
    );
    let cipher = aes_gcm::Aes256Gcm::new_from_slice(&box_key).ok()?;
    cipher
        .decrypt(
            &nonce.into(),
            Payload {
                msg: &ct,
                aad: aad.as_bytes(),
            },
        )
        .ok()
}

// ---------------------------------------------------------------------------
// 发送方签名（§21.4）
// ---------------------------------------------------------------------------

/// 签名载荷（§21.4）：固定键序紧凑 JSON（顶层字典序 ct/from/id/nonce/to/ts/
/// ttl；嵌套 from/to 均 domainId 在前；from.orgAddress 缺省时丢键）。
pub fn orgmail_sign_payload(envelope: &OrgMailEnvelope) -> String {
    let from = match &envelope.from.org_address {
        Some(addr) => format!(
            "{{\"domainId\":{},\"orgAddress\":{}}}",
            serde_json::to_string(&envelope.from.domain_id).unwrap_or_default(),
            serde_json::to_string(addr).unwrap_or_default()
        ),
        None => format!(
            "{{\"domainId\":{}}}",
            serde_json::to_string(&envelope.from.domain_id).unwrap_or_default()
        ),
    };
    let to = format!(
        "{{\"domainId\":{},\"orgAddress\":{}}}",
        serde_json::to_string(&envelope.to.domain_id).unwrap_or_default(),
        serde_json::to_string(&envelope.to.org_address).unwrap_or_default()
    );
    format!(
        "{{\"ct\":{},\"from\":{},\"id\":{},\"nonce\":{},\"to\":{},\"ts\":{},\"ttl\":{}}}",
        serde_json::to_string(&envelope.ct).unwrap_or_default(),
        from,
        serde_json::to_string(&envelope.id).unwrap_or_default(),
        serde_json::to_string(&envelope.nonce).unwrap_or_default(),
        to,
        envelope.ts,
        envelope.ttl
    )
}

/// 发送方域身份签名（§21.4）：对签名载荷 UTF-8 字节 Ed25519 签名 → b64。
pub fn orgmail_sign(signing_key: &ed25519_dalek::SigningKey, envelope: &OrgMailEnvelope) -> String {
    let payload = orgmail_sign_payload(envelope);
    B64.encode(signing_key.sign(payload.as_bytes()).to_bytes())
}

/// 信封验签：验签键 = from.domainId（自包含）；失败 → false（§21.5
/// invalid-envelope）。
pub fn orgmail_verify(envelope: &OrgMailEnvelope) -> bool {
    let Some(pk_raw) = parse_domain_id(&envelope.from.domain_id) else {
        return false;
    };
    let Ok(pk) = ed25519_dalek::VerifyingKey::from_bytes(&pk_raw) else {
        return false;
    };
    let Ok(sig_raw) = B64.decode(&envelope.sig) else {
        return false;
    };
    let Ok(sig_arr) = <[u8; 64]>::try_from(sig_raw.as_slice()) else {
        return false;
    };
    let payload = orgmail_sign_payload(envelope);
    pk.verify(
        payload.as_bytes(),
        &ed25519_dalek::Signature::from_bytes(&sig_arr),
    )
    .is_ok()
}

/// 24 位小写 hex 随机信封 id（12B）。
pub fn new_envelope_id() -> String {
    let mut bytes = [0u8; 12];
    rand::rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

/// ttl 归一（§21.2：缺省 7 天；超 30 天截断；≤0 视为缺省）。
pub fn normalize_ttl(ttl: i64) -> i64 {
    if ttl <= 0 {
        return MAIL_TTL_DEFAULT_MS;
    }
    ttl.min(MAIL_TTL_MAX_MS)
}

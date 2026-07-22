//! 身份派生：root 身份与域身份。
//!
//! - root 路径：`m/44'/607'/0'/0'/0'`（逐层强化）
//! - 域路径：root 后追加 `/{idxA}'/{idxB}'`，idxA/idxB = sha256(domain) 前 8 字节
//!   两个 u32BE & 0x7fffffff
//! - keypair = ed25519 fromSeed(末级节点 key)；rootId = sha256hex(publicKey)

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use ed25519_dalek::{Signature, SigningKey, Verifier, VerifyingKey};
use sha2::{Digest, Sha256};

use super::slip10::{Slip10Node, format_derivation_path, parse_derivation_path};

/// root 身份派生路径。
pub const ROOT_DERIVATION_PATH: &str = "m/44'/607'/0'/0'/0'";

/// root 路径索引（[44, 607, 0, 0, 0]）。
pub const ROOT_PATH_INDICES: [u32; 5] = [44, 607, 0, 0, 0];

/// 一个派生出的身份（root 或域）。
#[derive(Clone)]
pub struct Identity {
    /// ed25519 签名私钥（由 SLIP-0010 末级节点 32B key 生成，nacl 兼容）。
    pub signing_key: SigningKey,
    /// 完整派生路径字符串。
    pub path: String,
}

impl Identity {
    /// ed25519 公钥（32 字节）。
    pub fn public_key(&self) -> [u8; 32] {
        self.signing_key.verifying_key().to_bytes()
    }

    /// 公钥 hex（小写，64 字符）。
    pub fn public_key_hex(&self) -> String {
        hex::encode(self.public_key())
    }

    /// 身份 ID = sha256(publicKey) 的 hex（root 身份即 rootId，域身份即 domainId）。
    pub fn id(&self) -> String {
        hex::encode(Sha256::digest(self.public_key()))
    }
}

impl std::fmt::Debug for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Identity")
            .field("path", &self.path)
            .field("public_key_hex", &self.public_key_hex())
            .finish_non_exhaustive()
    }
}

/// 计算域身份索引：h = sha256(utf8(domain))；
/// idxA = u32BE(h[0..4]) & 0x7fffffff，idxB = u32BE(h[4..8]) & 0x7fffffff。
pub fn domain_indices(domain: &str) -> (u32, u32) {
    let h = Sha256::digest(domain.as_bytes());
    let idx_a = u32::from_be_bytes([h[0], h[1], h[2], h[3]]) & 0x7fff_ffff;
    let idx_b = u32::from_be_bytes([h[4], h[5], h[6], h[7]]) & 0x7fff_ffff;
    (idx_a, idx_b)
}

/// 由 BIP39 种子派生 root 身份（`m/44'/607'/0'/0'/0'`）。
pub fn derive_root_identity(seed: &[u8]) -> Identity {
    let node = Slip10Node::master(seed).derive_path(&ROOT_PATH_INDICES);
    Identity {
        signing_key: SigningKey::from_bytes(&node.key),
        path: ROOT_DERIVATION_PATH.to_string(),
    }
}

/// 由 BIP39 种子派生域身份（root 路径后追加 `/{idxA}'/{idxB}'`）。
pub fn derive_domain_identity(seed: &[u8], domain: &str) -> Identity {
    let (idx_a, idx_b) = domain_indices(domain);
    let mut indices = ROOT_PATH_INDICES.to_vec();
    indices.push(idx_a);
    indices.push(idx_b);
    let node = Slip10Node::master(seed).derive_path(&indices);
    Identity {
        signing_key: SigningKey::from_bytes(&node.key),
        path: format_derivation_path(&indices),
    }
}

/// 由 BIP39 种子沿任意路径字符串派生身份（用于 v1 文件兼容等场景）。
pub fn derive_identity_at_path(seed: &[u8], path: &str) -> super::error::Result<Identity> {
    let indices = parse_derivation_path(path)?;
    let node = Slip10Node::master(seed).derive_path(&indices);
    Ok(Identity {
        signing_key: SigningKey::from_bytes(&node.key),
        path: format_derivation_path(&indices),
    })
}

/// 校验 ed25519 分离签名（TS root-id.ts `verifyEd25519Signature`）。
///
/// 纯函数，任意验签方可用：payload 按 UTF-8 字节取，签名（64B）与公钥（32B）
/// 为 base64。base64 解码失败或长度不符一律返回 `false`，不报错（对齐 TS
/// try/catch 语义）。
pub fn verify_ed25519_signature(
    payload: &str,
    signature_base64: &str,
    public_key_base64: &str,
) -> bool {
    let (Ok(sig_bytes), Ok(pk_bytes)) = (
        B64.decode(signature_base64),
        B64.decode(public_key_base64),
    ) else {
        return false;
    };
    let (Ok(sig_arr), Ok(pk_arr)) = (
        <[u8; 64]>::try_from(sig_bytes.as_slice()),
        <[u8; 32]>::try_from(pk_bytes.as_slice()),
    ) else {
        return false;
    };
    let Ok(verifying_key) = VerifyingKey::from_bytes(&pk_arr) else {
        return false;
    };
    verifying_key
        .verify(payload.as_bytes(), &Signature::from_bytes(&sig_arr))
        .is_ok()
}

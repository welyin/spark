//! Ed25519↔X25519 转换机械层（纯逻辑）。
//!
//! C7（community-affairs.md §5，决策 3）：org 集合级 `encrypted` 轴已整体退役
//! ——orgkey 密钥表、orgkey-deliver 信封、org:acl 授权名单、readers 过滤、
//! grantAccess 家族全部移除；新模型下保密边界 = 域边界。
//!
//! 本文件只保留两侧复用的转换原语：
//! - M3 身份级 device epoch（`epoch/mod.rs`，ikey 包裹，m3-epoch-rotation-plan §5.2）；
//! - 跨组织网关邮箱（`org/mailbox.rs`，org-mail 域身份 DH）。
//!
//! X25519 DH + H1a/H1b + AES-256-GCM 的 box/unbox 实现已各自内化到上述模块
//! （域串不同，不复用同一函数），此处不再导出。

use curve25519_dalek::edwards::CompressedEdwardsY;
use sha2::{Digest, Sha512};

/// 身份公钥 → X25519 公钥（Ed25519→X25519 标准转换，tweetnacl 口径）。
/// 即取 ed25519 公钥对应 Edwards 点转换到 Montgomery u 坐标。
pub fn ed_pk_to_x25519(ed_pk: &[u8; 32]) -> Option<[u8; 32]> {
    let edwards = CompressedEdwardsY(*ed_pk).decompress()?;
    Some(edwards.to_montgomery().to_bytes())
}

/// 身份私钥（ed25519 seed）→ X25519 私钥标量：`clamp(sha512(seed)[0..32])`，
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

#[cfg(test)]
mod tests {
    use super::*;
    use curve25519_dalek::montgomery::MontgomeryPoint;
    use ed25519_dalek::SigningKey;

    /// 转换口径一致性：sk 侧与 pk 侧各自转换后做 X25519 DH，两端共享密钥一致
    /// （box/unbox 成立的前提，M3 ikey 与 org-mail 信封同依赖）。
    #[test]
    fn ed_x25519_conversion_dh_agrees() {
        let a = SigningKey::from_bytes(&[5u8; 32]);
        let b = SigningKey::from_bytes(&[6u8; 32]);
        let a_priv = ed_sk_to_x25519(&a.to_bytes());
        let b_priv = ed_sk_to_x25519(&b.to_bytes());
        let a_pub = ed_pk_to_x25519(&a.verifying_key().to_bytes()).unwrap();
        let b_pub = ed_pk_to_x25519(&b.verifying_key().to_bytes()).unwrap();
        let shared_ab = MontgomeryPoint(b_pub).mul_clamped(a_priv).to_bytes();
        let shared_ba = MontgomeryPoint(a_pub).mul_clamped(b_priv).to_bytes();
        assert_eq!(shared_ab, shared_ba, "DH 共享密钥两侧一致");
        assert!(
            !shared_ab.iter().all(|&x| x == 0),
            "正常密钥对不落低阶全零共享"
        );
    }

    /// 非法 ed25519 公钥（y=2，x²=(y²-1)/(dy²+1) 为非平方剩余，不可解压）
    /// → None，不 panic。注意 dalek v4 的 decompress 不拒绝非规范编码
    /// （y ≥ p），0xff..ff 这类值仍能解压，不能当反例。
    #[test]
    fn ed_pk_to_x25519_rejects_undecodable() {
        let mut pk = [0u8; 32];
        pk[0] = 2;
        assert!(ed_pk_to_x25519(&pk).is_none());
    }
}

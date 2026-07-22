//! 插件更新信任链（对齐 TS desktop/src/main/plugin-market/trust.ts +
//! updater/signature.ts 的 verifyManifestSignature）。
//!
//! - 信任公钥：内置默认公钥（与旧 TS 主进程同一枚）；环境变量
//!   `SPARK_PLUGIN_UPDATE_PUBLIC_KEY_PEM` 可整体覆盖（`@@` 分隔多枚，任一通过即可）。
//! - 验签算法：Ed25519 detached（manifest 文本 UTF-8 字节 ←→ base64 签名）。
//!   复用内核 `spark_core::identity::verify_ed25519_signature` 原语；
//!   PEM（SPKI）→ 原始 32 字节公钥的解析在本模块完成（Ed25519 SPKI 为
//!   固定 12 字节前缀 `302a300506032b6570032100` + 32 字节密钥）。

use base64::Engine;

/// 内置默认公钥（TS trust.ts `DEFAULT_PLUGIN_PUBLIC_KEYS_PEM`，与旧世界一致）。
pub(crate) const DEFAULT_PLUGIN_PUBLIC_KEYS_PEM: [&str; 1] = [
    "-----BEGIN PUBLIC KEY-----\nMCowBQYDK2VwAyEAEIZeVVpcZ4HdWRzYhxNcXRNOH56yhcP8QQnAjvZSHBY=\n-----END PUBLIC KEY-----",
];

/// Ed25519 SPKI DER 前缀（id-Ed25519 算法标识 + bit string 头）。
const ED25519_SPKI_DER_PREFIX: [u8; 12] = [
    0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
];

/// 解析信任配置：env `SPARK_PLUGIN_UPDATE_PUBLIC_KEY_PEM`（`@@` 分隔）优先，
/// 缺省回落内置公钥（TS `getPluginTrustConfig`；启动时解析一次注入服务）。
pub fn get_plugin_trust_config() -> Vec<String> {
    let from_env: Vec<String> = std::env::var("SPARK_PLUGIN_UPDATE_PUBLIC_KEY_PEM")
        .ok()
        .map(|raw| {
            raw.split("@@")
                .map(|item| item.trim().to_string())
                .filter(|item| !item.is_empty())
                .collect()
        })
        .unwrap_or_default();
    if from_env.is_empty() {
        DEFAULT_PLUGIN_PUBLIC_KEYS_PEM
            .iter()
            .map(|s| s.to_string())
            .collect()
    } else {
        from_env
    }
}

/// SPKI PEM → Ed25519 原始公钥（base64）。非 Ed25519/坏 PEM 返回 None
/// （对齐 TS `crypto.createPublicKey` 抛错被 try/catch 吞掉判 false 的语义）。
fn spki_pem_to_ed25519_raw_base64(pem: &str) -> Option<String> {
    let body: String = pem
        .lines()
        .filter(|line| !line.starts_with("-----"))
        .map(str::trim)
        .collect();
    let der = base64::engine::general_purpose::STANDARD.decode(body).ok()?;
    if der.len() != ED25519_SPKI_DER_PREFIX.len() + 32 || !der.starts_with(&ED25519_SPKI_DER_PREFIX) {
        return None;
    }
    Some(base64::engine::general_purpose::STANDARD.encode(&der[ED25519_SPKI_DER_PREFIX.len()..]))
}

/// 校验清单分离签名：任一信任公钥通过即真（TS `verifyManifestSignature`）。
pub fn verify_manifest_signature(
    manifest_text: &str,
    signature_base64: &str,
    public_keys_pem: &[String],
) -> bool {
    let signature = signature_base64.trim();
    public_keys_pem.iter().any(|pem| {
        spki_pem_to_ed25519_raw_base64(pem).is_some_and(|raw_public_key| {
            spark_core::identity::verify_ed25519_signature(manifest_text, signature, &raw_public_key)
        })
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    /// 测试密钥对（固定种子，确定性）→ (SPKI PEM, SigningKey)。
    pub(crate) fn test_keypair(seed: u8) -> (String, SigningKey) {
        let signing_key = SigningKey::from_bytes(&[seed; 32]);
        let mut der = ED25519_SPKI_DER_PREFIX.to_vec();
        der.extend_from_slice(signing_key.verifying_key().as_bytes());
        let pem = format!(
            "-----BEGIN PUBLIC KEY-----\n{}\n-----END PUBLIC KEY-----",
            base64::engine::general_purpose::STANDARD.encode(der)
        );
        (pem, signing_key)
    }

    pub(crate) fn sign_text(signing_key: &SigningKey, text: &str) -> String {
        base64::engine::general_purpose::STANDARD.encode(signing_key.sign(text.as_bytes()).to_bytes())
    }

    #[test]
    fn verify_roundtrip_and_negatives() {
        let (pem, signing_key) = test_keypair(7);
        let payload = "{\"pluginId\":\"weibo-core\"}";
        let signature = sign_text(&signing_key, payload);

        // 正：签名有效
        assert!(verify_manifest_signature(payload, &signature, std::slice::from_ref(&pem)));
        // 反：篡改清单
        assert!(!verify_manifest_signature(
            "{\"pluginId\":\"evil\"}",
            &signature,
            std::slice::from_ref(&pem)
        ));
        // 反：别的密钥
        let (other_pem, _) = test_keypair(8);
        assert!(!verify_manifest_signature(payload, &signature, &[other_pem]));
        // 反：坏 base64 签名 / 坏 PEM
        assert!(!verify_manifest_signature(payload, "!!!", std::slice::from_ref(&pem)));
        assert!(!verify_manifest_signature(payload, &signature, &["not-a-pem".to_string()]));
        // 多 key 任一通过（对齐 TS some()）
        let (pem2, _) = test_keypair(9);
        assert!(verify_manifest_signature(payload, &signature, &[pem2, pem]));
    }

    #[test]
    fn trust_config_env_override_with_at_separator() {
        // 本测试是唯一读写该环境变量的用例（进程内串行假设），其他用例均显式注入密钥。
        let original = std::env::var("SPARK_PLUGIN_UPDATE_PUBLIC_KEY_PEM").ok();
        std::env::set_var(
            "SPARK_PLUGIN_UPDATE_PUBLIC_KEY_PEM",
            "  key-one @@key-two@@  ",
        );
        assert_eq!(get_plugin_trust_config(), vec!["key-one", "key-two"]);
        match original {
            Some(value) => std::env::set_var("SPARK_PLUGIN_UPDATE_PUBLIC_KEY_PEM", value),
            None => std::env::remove_var("SPARK_PLUGIN_UPDATE_PUBLIC_KEY_PEM"),
        }
        // 未设置 env 时回落内置默认公钥
        assert_eq!(get_plugin_trust_config().len(), 1);
        assert!(get_plugin_trust_config()[0].contains("MCowBQYDK2VwAyEAEIZeVVpc"));
    }
}

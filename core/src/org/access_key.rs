//! org_user_id 与 `OrganizationAccessKey` 验绑（A16，membership §4.4 三联动；
//! 派生地基 identity 篇 §4.4）。
//!
//! - `org_user_id` = `sha256hex(derive_domain_identity(seed, "org-access:{orgId}").公钥)`
//!   ——组织内标识符，与 rootId 无关联性（域串派生，不同组织不同值）；
//! - `OrganizationAccessKey` = 域公钥 + 根密钥对 `"org-access:{orgId}:{publicKey}"`
//!   的绑定签名 + 根公钥（验绑锚点）；
//! - 合入侧验绑两步：① `sha256hex(rootPubkey) == 名册键 rootId`（锚定到名册成员）；
//!   ② 绑定签名用 rootPubkey 验签（证明根密钥持有者发布了该域身份）。
//!
//! 注意区分：`org/genesis.rs` 的 `OrgDomainIdentity::derive` 是**组织根私钥**派生
//! **组织域**身份（`community:{id}` 等），密钥来源与算法均不同，不是本模块资产。

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use ed25519_dalek::Signer as _;
use sha2::{Digest, Sha256};

use super::types::{OrganizationAccessKey, access_key_bind_payload};
use crate::identity::derive::{derive_domain_identity, derive_root_identity};
use crate::identity::verify_ed25519_signature;

/// 成员的 org-access 域串（`derive_domain_identity` 的 domain 参数）。
pub fn org_access_domain(org_id: &str) -> String {
    format!("org-access:{org_id}")
}

/// org_user_id = 域身份 Ed25519 公钥的 sha256hex（64 小写 hex，与 rootId 同构）。
pub fn org_user_id_from_pubkey(pubkey: &[u8; 32]) -> String {
    hex::encode(Sha256::digest(pubkey))
}

/// 从 seed 派生成员的 `OrganizationAccessKey`（域公钥 + 根私钥绑定签名 + 根公钥）。
///
/// 纯函数：同 seed 同 orgId 恒同值（多设备一致性地基）。
pub fn derive_access_key(seed: &[u8], org_id: &str) -> OrganizationAccessKey {
    let domain_identity = derive_domain_identity(seed, &org_access_domain(org_id));
    let public_key = B64.encode(domain_identity.signing_key.verifying_key().to_bytes());
    let root_identity = derive_root_identity(seed);
    let payload = access_key_bind_payload(org_id, &public_key);
    let bind_sig = B64.encode(
        root_identity
            .signing_key
            .sign(payload.as_bytes())
            .to_bytes(),
    );
    OrganizationAccessKey {
        public_key,
        bind_sig,
        root_pubkey: Some(B64.encode(root_identity.signing_key.verifying_key().to_bytes())),
    }
}

/// 合入侧验绑：access_key 必须可锚定到名册键 `member_root_id` 且绑定签名有效。
///
/// - `rootPubkey` 缺失（A16 前的存量/他端旧版发布）→ `false`（不采信；
///   本人设备重新发布即补齐）；
/// - `sha256hex(rootPubkey) != member_root_id` → `false`（张冠李戴）；
/// - 绑定签名验签失败（错域串/错密钥/篡改）→ `false`。
pub fn verify_access_key_binding(
    org_id: &str,
    member_root_id: &str,
    access_key: &OrganizationAccessKey,
) -> bool {
    let Some(root_pubkey_b64) = access_key.root_pubkey.as_deref() else {
        return false;
    };
    let Ok(root_pubkey) = B64.decode(root_pubkey_b64) else {
        return false;
    };
    if hex::encode(Sha256::digest(&root_pubkey)) != member_root_id {
        return false;
    }
    let payload = access_key_bind_payload(org_id, &access_key.public_key);
    verify_ed25519_signature(&payload, &access_key.bind_sig, root_pubkey_b64)
}

/// 成员的 org_user_id（由已验绑/自发布的 accessKey 派生；未发布 → `None`）。
///
/// 解码失败（脏数据）同样返回 `None`，不 panic。
pub fn member_org_user_id(access_key: &OrganizationAccessKey) -> Option<String> {
    let pubkey = B64.decode(&access_key.public_key).ok()?;
    let pubkey: [u8; 32] = pubkey.try_into().ok()?;
    Some(org_user_id_from_pubkey(&pubkey))
}

/// 切换窗口条件①的可判定形式（membership §4.4-4 存量迁移）：名册全部
/// **个人成员**的 org_user_id 均可派生（accessKey 已发布且域公钥可解码）。
/// `kind = org` 成员不参与判定——其 rootId 槽位承载的是该组织在本域的域
/// 身份 id（org-genesis §4），本就非个人 rootId，无 accessKey 补齐义务。
pub fn roster_fully_mapped(record: &super::types::OrganizationRecord) -> bool {
    record
        .members
        .iter()
        .filter(|m| m.member_kind() == super::types::MemberKind::Person)
        .all(|m| m.org_user_id().is_some())
}

/// 合入侧执法：剥除名册中验绑失败的 accessKey（whole 记录入站路径）。
/// 返回剥除条数（0 = 无改动，调用方据此决定是否回写）。
pub fn strip_unverified_access_keys(record: &mut super::types::OrganizationRecord) -> usize {
    let org_id = record.org_id.clone();
    let mut stripped = 0usize;
    for member in &mut record.members {
        let invalid = member.access_key.as_ref().is_some_and(|ak| {
            !verify_access_key_binding(&org_id, &member.root_id, ak)
        });
        if invalid {
            log::warn!(
                "[ORG-ACCESS-KEY] 验绑失败已剥除 | member={}",
                &member.root_id[..std::cmp::min(16, member.root_id.len())]
            );
            member.access_key = None;
            stripped += 1;
        }
    }
    stripped
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEED: [u8; 32] = [42u8; 32];
    const ORG: &str = "org_0123456789abcdef";

    fn root_id_of(seed: &[u8]) -> String {
        let root = derive_root_identity(seed);
        hex::encode(Sha256::digest(root.signing_key.verifying_key().to_bytes()))
    }

    #[test]
    fn derive_is_deterministic_and_org_scoped() {
        let a1 = derive_access_key(&SEED, ORG);
        let a2 = derive_access_key(&SEED, ORG);
        assert_eq!(a1, a2, "同 seed 同 org 恒同值（多设备一致性）");
        let other = derive_access_key(&SEED, "org_fedcba9876543210");
        assert_ne!(a1.public_key, other.public_key, "不同组织不同域身份（无关联性）");
    }

    #[test]
    fn org_user_id_is_sha256_of_domain_pubkey() {
        let key = derive_access_key(&SEED, ORG);
        let uid = member_org_user_id(&key).expect("org_user_id 可派生");
        assert_eq!(uid.len(), 64, "64 hex");
        assert!(uid.chars().all(|c| c.is_ascii_hexdigit()));
        // 与 rootId 不同值（不泄露关联）。
        assert_ne!(uid, root_id_of(&SEED));
    }

    #[test]
    fn binding_verifies_and_negative_matrix() {
        let key = derive_access_key(&SEED, ORG);
        let root_id = root_id_of(&SEED);
        assert!(
            verify_access_key_binding(ORG, &root_id, &key),
            "自发布验绑通过"
        );

        // 错 orgId（域串不匹配）。
        assert!(!verify_access_key_binding(
            "org_fedcba9876543210",
            &root_id,
            &key
        ));
        // 错名册键（张冠李戴：另一账号的 rootId）。
        let other_root = root_id_of(&[7u8; 32]);
        assert!(!verify_access_key_binding(ORG, &other_root, &key));
        // 篡改域公钥（绑定签名不匹配）。
        let mut tampered = key.clone();
        tampered.public_key = derive_access_key(&[9u8; 32], ORG).public_key;
        assert!(!verify_access_key_binding(ORG, &root_id, &tampered));
        // 缺 rootPubkey（存量）→ 不采信。
        let legacy = OrganizationAccessKey {
            root_pubkey: None,
            ..key.clone()
        };
        assert!(!verify_access_key_binding(ORG, &root_id, &legacy));
    }
}

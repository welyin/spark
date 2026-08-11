//! dm 入站编排（orgkey-deliver 系，O4 工作项 3）：encrypted 集合密钥定向投递
//! 的接收侧（名单 owner → reader）。
//!
//! 对应 org-orgsync.md §20.6。接收侧规则（§20.6）：`senderRootId` ∈ 当前 acl
//! `owners` → 验签（`deliver_sign_payload`，用 sender 成员表 accessKey 公钥）→
//! 解 box（`unbox_epoch_key`）→ 落 `orgkey:` 表；sender 非 owner / 验签失败 /
//! 本地已有 ≥ epoch 密钥 → **静默丢弃**（不暴露资格探测 oracle）。
//!
//! ## 与 kernel/host 的分工（seed 不可达）
//!
//! 本层为**纯逻辑**（存储泛型）：只做资格/验签/幂等判定，产出
//! [`OrgkeyUnbox`] 指令（wrappedKey/nonce24/senderX25519 等），由 host 层用本机
//! 组织身份私钥（seed 派生）实际解 box 并写 orgkey 表——解包需要 recipient
//! 私钥（seed），纯逻辑层不可达。响应恒 `{"ok":true}`（丢弃与受理同应答，
//! 消除资格探测 oracle）。
//!
//! ## 幂等
//!
//! 本地已有 ≥ epoch 密钥即丢弃（密钥只增不减）；重投/乱序到达无害。

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use ed25519_dalek::{Verifier as _, VerifyingKey};
use serde_json::Value;

use super::{InboundContext, InboundDmResult, Result, done, ok_response};
use crate::org::OrganizationService;
use crate::storage::StorageBackend;

/// 待 host 解包落库的指令（seed 不可达：纯逻辑层只做资格/验签判定）。
#[derive(Clone, Debug)]
pub struct OrgkeyUnbox {
    /// 目标组织。
    pub org_id: String,
    /// 集合名（§20.6 线形用 name）。
    pub name: String,
    /// 代际。
    pub version: String,
    /// 该密文密钥的代际。
    pub epoch: u64,
    /// crypto_box wrappedKey（base64）。
    pub wrapped_key: String,
    /// 24B nonce（base64）。
    pub nonce24: String,
    /// sender（owner）rootId（H1b 域分隔上下文 + 验签锚匹配）。
    pub sender_root_id: String,
    /// sender（owner）组织身份公钥的 X25519 形式（解 box 用）。
    pub sender_x25519: [u8; 32],
}

/// orgkey-deliver 入站（**reader 侧**）：资格/验签/幂等判定后产出解包指令。
///
/// 验签：
/// 1. `senderRootId` ∈ 当前 acl `owners`（非 owner → 静默丢弃）；
/// 2. 签名 = sender 组织身份 Ed25519 对固定键序载荷
///    `{"collection","epoch","nonce","orgId","recipientRootId","ts","wrappedKey"}`
///    的签名，用 sender 成员表 accessKey 公钥验签（失败 → 静默丢弃）；
/// 3. 本地 orgkey 表已有 ≥ epoch 密钥 → 静默丢弃（幂等，密钥只增不减）。
///
/// 通过后返回 `Ok(Some(OrgkeyUnbox))`（host 解包落库）；资格不符返回
/// `Ok(None)`（静默丢弃）。存储错误经 `Result` 传播。
pub fn handle_orgkey_deliver<S: StorageBackend>(
    storage: &mut S,
    ctx: &InboundContext<'_>,
    from: &str,
    body: &Value,
) -> Result<InboundDmResult> {
    let Some(deliver) = crate::sync::orgsync::parse_orgkey_deliver(body) else {
        // 线形非法：静默丢弃（与资格不符同应答，不暴露解析细节）
        return done(ok_response(), Vec::new());
    };
    // 校验 recipient 必须为本机（防转投；§20.6 recipientRootId 防转投）
    if deliver.recipient_root_id != ctx.my_root_id {
        return done(ok_response(), Vec::new());
    }
    // H1c：body 内 senderRootId 必须与 dm 信封 from 一致（不一致丢弃）——
    // 防攻击者改 body.senderRootId 冒用他人身份（验签锚/域分隔上下文都绑
    // senderRootId，二者必须一致）。
    if deliver.sender_root_id != from {
        return done(ok_response(), Vec::new());
    }
    // 解析集合 name/version
    let Some((name, version)) = split_collection_full(&deliver.collection) else {
        return done(ok_response(), Vec::new());
    };
    // 公共前置：org 存在且 from ∈ 成员表
    let Ok(Some(record)) = OrganizationService::get_record(storage, &deliver.org_id) else {
        return done(ok_response(), Vec::new());
    };
    if record.find_member(from).is_none() {
        return done(ok_response(), Vec::new());
    }
    // 1. sender ∈ 当前 acl owners（非 owner → 静默丢弃）
    let current: crate::sync::orgsync::AclRecord = storage
        .get(&crate::sync::orgsync::acl_key(&deliver.org_id, name, version))
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or(crate::sync::orgsync::AclRecord {
            owners: Vec::new(),
            readers: Vec::new(),
            epoch: 0,
            updated_at: 0,
            reset_by: None,
            sig: String::new(),
        });
    if !current.is_owner(from) {
        log::info!(
            "[ORGKEY] deliver discarded: sender={} not owner of {}:{}",
            &from[..std::cmp::min(16, from.len())],
            deliver.org_id,
            deliver.collection
        );
        return done(ok_response(), Vec::new());
    }
    // 2. 验签（sender accessKey 公钥）
    let sender_pk = record
        .find_member(from)
        .and_then(|m| m.access_key.as_ref())
        .and_then(|ak| {
            let Ok(bytes) = B64.decode(&ak.public_key) else {
                return None;
            };
            let Ok(arr) = <[u8; 32]>::try_from(bytes.as_slice()) else {
                return None;
            };
            VerifyingKey::from_bytes(&arr).ok()
        });
    let Some(sender_pk) = sender_pk else {
        return done(ok_response(), Vec::new());
    };
    let payload = crate::sync::orgsync::deliver_sign_payload(
        &deliver.collection,
        deliver.epoch,
        &deliver.nonce,
        &deliver.org_id,
        &deliver.recipient_root_id,
        deliver.ts,
        &deliver.wrapped_key,
    );
    let Ok(sig_bytes) = B64.decode(&deliver.sig) else {
        return done(ok_response(), Vec::new());
    };
    let Ok(sig_arr) = <[u8; 64]>::try_from(sig_bytes.as_slice()) else {
        return done(ok_response(), Vec::new());
    };
    if sender_pk
        .verify(payload.as_bytes(), &ed25519_dalek::Signature::from_bytes(&sig_arr))
        .is_err()
    {
        log::info!(
            "[ORGKEY] deliver discarded: bad sig from={} col={}",
            &from[..std::cmp::min(16, from.len())],
            deliver.collection
        );
        return done(ok_response(), Vec::new());
    }
    // 3. 本地已有 ≥ epoch → 静默丢弃（幂等；H1d 对齐 §20.6 规格「≥ epoch」：
    // 本地已知最大 epoch 已 ≥ 投递 epoch 即丢弃——已有更高 epoch 密钥时低
    // epoch 投递也可丢，密钥只增不减）。
    if crate::sync::orgsync::max_known_epoch(storage, &deliver.org_id, name, version)
        .is_some_and(|max| max >= deliver.epoch)
    {
        return done(ok_response(), Vec::new());
    }
    // sender 组织身份公钥 → X25519（解 box 用）
    let Some(sender_x25519) = crate::sync::orgsync::ed_pk_to_x25519(&sender_pk.to_bytes()) else {
        return done(ok_response(), Vec::new());
    };
    let unbox = OrgkeyUnbox {
        org_id: deliver.org_id.clone(),
        name: name.to_string(),
        version: version.to_string(),
        epoch: deliver.epoch,
        wrapped_key: deliver.wrapped_key.clone(),
        nonce24: deliver.nonce.clone(),
        sender_root_id: from.to_string(),
        sender_x25519,
    };
    Ok(InboundDmResult {
        response: ok_response(),
        events: Vec::new(),
        auto_accept: None,
        self_profile: None,
        device_sync_reply: None,
        device_notice_broadcast: false,
        profile_sync_reply: None,
        pdsync_out: Vec::new(),
        orgsync_out: Vec::new(),
        profile_applied: false,
        orgkey_unbox: Some(unbox),
    })
}

/// 解析 `{name}@v{version}` → (name, version)。
fn split_collection_full(col_full: &str) -> Option<(&str, &str)> {
    let at = col_full.rfind("@v")?;
    Some((&col_full[..at], &col_full[at + 2..]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::STANDARD as B64;
    use crate::identity::derive_domain_identity;
    use crate::org::types::{OrganizationMember, OrganizationRecord, OrganizationRole, OrganizationAccessKey};
    use crate::storage::MemoryStorage;
    use serde_json::json;

    const MY_ROOT: &str = "reader-root";
    const OWNER_ROOT: &str = "owner-root";

    fn org_record(owner_access_key: Option<OrganizationAccessKey>) -> OrganizationRecord {
        OrganizationRecord {
            org_id: "org_0000000000000001".to_string(),
            name: "t".to_string(),
            description: String::new(),
            avatar: String::new(),
            base_plugin_domain: None,
            created_at: 1000,
            created_by: MY_ROOT.to_string(),
            updated_at: 1000,
            members: vec![
                OrganizationMember {
                    root_id: OWNER_ROOT.to_string(),
                    role: OrganizationRole::Admin,
                    joined_at: 1000,
                    added_by: MY_ROOT.to_string(),
                    node_info: None,
                    nickname: None,
                    avatar: None,
                    signature: None,
                    gender: None,
                    region: None,
                    use_personal_identity: None,
                    access_key: owner_access_key,
                    extra: Default::default(),
                },
                OrganizationMember {
                    root_id: MY_ROOT.to_string(),
                    role: OrganizationRole::Member,
                    joined_at: 1000,
                    added_by: MY_ROOT.to_string(),
                    node_info: None,
                    nickname: None,
                    avatar: None,
                    signature: None,
                    gender: None,
                    region: None,
                    use_personal_identity: None,
                    access_key: None,
                    extra: Default::default(),
                },
            ],
            sync: None,
            gateways: vec![],
            data_accounts: vec![],
            org_address: None,
            is_public: false,
            extra: Default::default(),
        }
    }

    fn ctx<'a>(online: &'a std::collections::HashSet<String>) -> InboundContext<'a> {
        InboundContext {
            my_root_id: MY_ROOT,
            my_nickname: "me",
            remote_peer_id: "peer-a",
            online_peers: online,
            node_id: "local-node",
            now_ms: 2000,
        }
    }

    /// O4 工作项 3：合法 orgkey-deliver（owner 签名 + box 包裹）到达 reader →
    /// 资格/验签/幂等全过 → 产出 `OrgkeyUnbox` 指令（host 解包落库）。非 owner
    /// sender → 静默丢弃（无 unbox 指令）。
    #[test]
    fn handle_orgkey_deliver_valid_produces_unbox_instruction() {
        let owner_seed = [7u8; 64];
        let owner_org = derive_domain_identity(&owner_seed, &crate::kernel::Kernel::org_access_domain("org_0000000000000001"));
        let owner_pk_b64 = B64.encode(owner_org.public_key());
        let ak = OrganizationAccessKey {
            public_key: owner_pk_b64,
            bind_sig: "bind".to_string(),
        };
        let mut s = MemoryStorage::new();
        let record = org_record(Some(ak.clone()));
        crate::org::OrganizationService::save_record(&mut s, &record).unwrap();
        // acl：owners=[owner]
        let acl = crate::sync::orgsync::AclRecord {
            owners: vec![OWNER_ROOT.to_string()],
            readers: vec![MY_ROOT.to_string()],
            epoch: 1,
            updated_at: 100,
            reset_by: None,
            sig: String::new(),
        };
        s.put(
            &crate::sync::orgsync::acl_key("org_0000000000000001", "fin:pay", "1"),
            &serde_json::to_string(&acl).unwrap(),
        )
        .unwrap();
        // owner 构造 orgkey-deliver（epoch 1）
        let owner_x25519 = crate::sync::orgsync::ed_sk_to_x25519(&owner_org.signing_key.to_bytes());
        let my_pub = crate::sync::orgsync::ed_pk_to_x25519(&owner_org.signing_key.verifying_key().to_bytes()).unwrap();
        let body = crate::sync::orgsync::build_orgkey_deliver(
            "org_0000000000000001",
            "fin:pay",
            "1",
            1,
            &[9u8; 32],
            OWNER_ROOT,
            MY_ROOT,
            &my_pub,
            &owner_org.signing_key,
            &owner_x25519,
            1500,
        )
        .unwrap();
        let online = std::collections::HashSet::new();
        let res = handle_orgkey_deliver(&mut s, &ctx(&online), OWNER_ROOT, &body).unwrap();
        assert_eq!(res.response["ok"], json!(true));
        let unbox = res.orgkey_unbox.expect("合法投递产出 unbox 指令");
        assert_eq!(unbox.epoch, 1);
        assert_eq!(unbox.name, "fin:pay");
        assert_eq!(unbox.version, "1");
        // sender_x25519 与 owner 公钥一致
        assert_eq!(unbox.sender_x25519, crate::sync::orgsync::ed_pk_to_x25519(&owner_org.public_key()).unwrap());
    }

    /// O4 工作项 3：非 owner sender → 静默丢弃（无 unbox 指令，不落 orgkey 表）。
    #[test]
    fn handle_orgkey_deliver_non_owner_discarded() {
        let owner_seed = [8u8; 64];
        let owner_org = derive_domain_identity(&owner_seed, &crate::kernel::Kernel::org_access_domain("org_0000000000000001"));
        let ak = OrganizationAccessKey {
            public_key: B64.encode(owner_org.public_key()),
            bind_sig: "bind".to_string(),
        };
        let mut s = MemoryStorage::new();
        let record = org_record(Some(ak));
        crate::org::OrganizationService::save_record(&mut s, &record).unwrap();
        // acl：owners=[owner]，但 sender 是"member-b"（非 owner）
        let acl = crate::sync::orgsync::AclRecord {
            owners: vec![OWNER_ROOT.to_string()],
            readers: vec![MY_ROOT.to_string()],
            epoch: 1,
            updated_at: 100,
            reset_by: None,
            sig: String::new(),
        };
        s.put(
            &crate::sync::orgsync::acl_key("org_0000000000000001", "fin:pay", "1"),
            &serde_json::to_string(&acl).unwrap(),
        )
        .unwrap();
        // 非 owner 成员（"member-b"）伪造投递 → 静默丢弃
        let body = json!({
            "orgId": "org_0000000000000001",
            "collection": "fin:pay@v1",
            "epoch": 1,
            "wrappedKey": "x",
            "nonce": "y",
            "senderRootId": "member-b",
            "recipientRootId": MY_ROOT,
            "ts": 1500,
            "sig": "z",
        });
        let online = std::collections::HashSet::new();
        let res = handle_orgkey_deliver(&mut s, &ctx(&online), "member-b", &body).unwrap();
        assert_eq!(res.response["ok"], json!(true));
        assert!(res.orgkey_unbox.is_none(), "非 owner 投递静默丢弃");
        assert!(
            crate::sync::orgsync::get_epoch_key(&s, "org_0000000000000001", "fin:pay", "1", 1).is_none(),
            "被丢弃投递不落 orgkey 表"
        );
    }
}

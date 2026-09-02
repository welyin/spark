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

/// 入站判定结论（内部；F3 残余 §7.1 把「同步未到」与「真拒绝」分开）。
enum OrgkeyJudge {
    /// 资格/验签/幂等全过：host 解包落库。
    Unbox(OrgkeyUnbox),
    /// 「同步未到」等待态：owners 校验失败（本地 acl 缺失/陈旧）或 sender
    /// accessKey 未同步——暂存待 acl/org:meta 合入后重评估。
    Wait,
    /// 真拒绝/幂等丢弃：线形非法、recipient 不符、sender 不符、非成员、
    /// 签名错误、已有 ≥ epoch——**不暂存**（暂存绝不绕过密码学把关）。
    Drop,
}

/// orgkey-deliver 入站（**reader 侧**）：资格/验签/幂等判定后产出解包指令。
///
/// 验签：
/// 1. `senderRootId` ∈ 当前 acl `owners`（非 owner → 见下「等待态」）；
/// 2. 签名 = sender 组织身份 Ed25519 对固定键序载荷
///    `{"collection","epoch","nonce","orgId","recipientRootId","ts","wrappedKey"}`
///    的签名，用 sender 成员表 accessKey 公钥验签（失败 → 静默丢弃）；
/// 3. 本地 orgkey 表已有 ≥ epoch 密钥 → 静默丢弃（幂等，密钥只增不减）。
///
/// 通过后返回 `Ok(Some(OrgkeyUnbox))`（host 解包落库）。
///
/// F3 残余（org-acl-genesis-fix §7.1）：owners 校验失败（含 acl 缺失/陈旧）
/// 与 sender accessKey 缺失两个「同步未到」点位**暂存原始 body**
/// （`orgkey-deliver-stash:` 本地键），acl / org:meta（成员表 accessKey 段）
/// 合入后由 [`reevaluate_orgkey_stash`] 重放本校验链；真拒绝/幂等丢弃一律
/// 不暂存。存储错误经 `Result` 传播。
pub fn handle_orgkey_deliver<S: StorageBackend>(
    storage: &mut S,
    ctx: &InboundContext<'_>,
    from: &str,
    body: &Value,
) -> Result<InboundDmResult> {
    match judge_orgkey_deliver(storage, ctx, from, body)? {
        OrgkeyJudge::Unbox(unbox) => Ok(InboundDmResult {
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
            orgkey_unbox: vec![unbox],
            feed_blob_out: None,
        }),
        OrgkeyJudge::Wait => {
            // 暂存原始 body（键维度 (sender, epoch) 去重，ts 新者覆盖）
            if let Some(deliver) = crate::sync::orgsync::parse_orgkey_deliver(body) {
                crate::sync::orgsync::orgkey_stash_put(
                    storage,
                    &deliver.org_id,
                    &deliver.collection,
                    &deliver.sender_root_id,
                    deliver.epoch,
                    body,
                );
            }
            done(ok_response(), Vec::new())
        }
        OrgkeyJudge::Drop => done(ok_response(), Vec::new()),
    }
}

/// F3 残余 §7.1：acl / org:meta（成员表 accessKey 段）合入后，重放该 org
/// 暂存的 orgkey-deliver 校验链（重放跳过信封新鲜度窗口——dm 层时效在首次
/// 入站已验；body 签名校验与时间无关，不受重放影响）。
///
/// - 够格 → 产出 unbox 指令（调用方/host 解包落库）并删除暂存；
/// - 仍处等待态（acl/accessKey 仍未到）→ 保留暂存等下次触发；
/// - 真拒绝/幂等（签名错、已有 ≥ epoch、recipient 不符等）→ 删除暂存
///   （永久不够格，不留存垃圾）。
pub fn reevaluate_orgkey_stash<S: StorageBackend>(
    storage: &mut S,
    ctx: &InboundContext<'_>,
    org_id: &str,
) -> Result<Vec<OrgkeyUnbox>> {
    let mut unboxes = Vec::new();
    for (collection, sender, epoch, body) in
        crate::sync::orgsync::orgkey_stash_for_org(storage, org_id)
    {
        let Ok(body) = serde_json::from_str::<Value>(&body) else {
            // 损坏暂存永不可判 → 直接清
            crate::sync::orgsync::orgkey_stash_remove(storage, org_id, &collection, &sender, epoch);
            continue;
        };
        match judge_orgkey_deliver(storage, ctx, &sender, &body)? {
            OrgkeyJudge::Unbox(unbox) => {
                crate::sync::orgsync::orgkey_stash_remove(storage, org_id, &collection, &sender, epoch);
                unboxes.push(unbox);
            }
            OrgkeyJudge::Wait => {} // 仍不够格 → 保留暂存
            OrgkeyJudge::Drop => {
                crate::sync::orgsync::orgkey_stash_remove(storage, org_id, &collection, &sender, epoch);
            }
        }
    }
    Ok(unboxes)
}

/// handle/reevaluate 共用的判定链（纯判定，不写暂存）。
fn judge_orgkey_deliver<S: StorageBackend>(
    storage: &mut S,
    ctx: &InboundContext<'_>,
    from: &str,
    body: &Value,
) -> Result<OrgkeyJudge> {
    let Some(deliver) = crate::sync::orgsync::parse_orgkey_deliver(body) else {
        // 线形非法：静默丢弃（与资格不符同应答，不暴露解析细节）
        return Ok(OrgkeyJudge::Drop);
    };
    // 校验 recipient 必须为本机（防转投；§20.6 recipientRootId 防转投）
    if deliver.recipient_root_id != ctx.my_root_id {
        return Ok(OrgkeyJudge::Drop);
    }
    // H1c：body 内 senderRootId 必须与 dm 信封 from 一致（不一致丢弃）——
    // 防攻击者改 body.senderRootId 冒用他人身份（验签锚/域分隔上下文都绑
    // senderRootId，二者必须一致）。
    if deliver.sender_root_id != from {
        return Ok(OrgkeyJudge::Drop);
    }
    // 解析集合 name/version
    let Some((name, version)) = split_collection_full(&deliver.collection) else {
        return Ok(OrgkeyJudge::Drop);
    };
    // 公共前置：org 存在且 from ∈ 成员表
    let Ok(Some(record)) = OrganizationService::get_record(storage, &deliver.org_id) else {
        return Ok(OrgkeyJudge::Drop);
    };
    if record.find_member(from).is_none() {
        return Ok(OrgkeyJudge::Drop);
    }
    // 1. sender ∈ 当前 acl owners——失败含「acl 缺失/陈旧」（同步未到）与
    // 「sender 真非 owner」两种；暂存方案下统一按等待态处理（重评估时若
    // sender 仍非 owner 且 acl 已到位……仍为 Wait——保守方向：acl 可能
    // 仍陈旧。真非 owner 的暂存由「重评估不过即清」在 acl 更新覆盖后仍
    // Wait 而保留——这是有界的（成员 + 键维度去重），不授予任何权限。
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
            "[ORGKEY] deliver stashed: sender={} not owner of {}:{} (acl not arrived?)",
            &from[..std::cmp::min(16, from.len())],
            deliver.org_id,
            deliver.collection
        );
        return Ok(OrgkeyJudge::Wait);
    }
    // 2. 验签（sender accessKey 公钥）；accessKey 缺失 = 同步未到 → 等待态
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
        return Ok(OrgkeyJudge::Wait);
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
        return Ok(OrgkeyJudge::Drop);
    };
    let Ok(sig_arr) = <[u8; 64]>::try_from(sig_bytes.as_slice()) else {
        return Ok(OrgkeyJudge::Drop);
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
        return Ok(OrgkeyJudge::Drop);
    }
    // 3. 本地已有 ≥ epoch → 静默丢弃（幂等；H1d 对齐 §20.6 规格「≥ epoch」：
    // 本地已知最大 epoch 已 ≥ 投递 epoch 即丢弃——已有更高 epoch 密钥时低
    // epoch 投递也可丢，密钥只增不减）。
    if crate::sync::orgsync::max_known_epoch(storage, &deliver.org_id, name, version)
        .is_some_and(|max| max >= deliver.epoch)
    {
        return Ok(OrgkeyJudge::Drop);
    }
    // sender 组织身份公钥 → X25519（解 box 用）
    let Some(sender_x25519) = crate::sync::orgsync::ed_pk_to_x25519(&sender_pk.to_bytes()) else {
        return Ok(OrgkeyJudge::Drop);
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
    Ok(OrgkeyJudge::Unbox(unbox))
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
            kverify: None,
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
        let unbox = res.orgkey_unbox.into_iter().next().expect("合法投递产出 unbox 指令");
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
        assert!(res.orgkey_unbox.is_empty(), "非 owner 投递静默丢弃");
        assert!(
            crate::sync::orgsync::get_epoch_key(&s, "org_0000000000000001", "fin:pay", "1", 1).is_none(),
            "被丢弃投递不落 orgkey 表"
        );
    }

    // ── F3 残余 §7.1：「同步未到」暂存 + 到达重评估 ─────────────────────

    const ORG: &str = "org_0000000000000001";

    /// 合法投递 body（owner 签名 + box 包裹）+ owner accessKey。
    fn owner_deliver_body() -> (Value, OrganizationAccessKey) {
        let owner_org = derive_domain_identity(&[7u8; 64], &crate::kernel::Kernel::org_access_domain(ORG));
        let ak = OrganizationAccessKey {
            public_key: B64.encode(owner_org.public_key()),
            bind_sig: "bind".to_string(),
        };
        let owner_x25519 = crate::sync::orgsync::ed_sk_to_x25519(&owner_org.signing_key.to_bytes());
        let my_pub = crate::sync::orgsync::ed_pk_to_x25519(&owner_org.signing_key.verifying_key().to_bytes()).unwrap();
        let body = crate::sync::orgsync::build_orgkey_deliver(
            ORG, "fin:pay", "1", 1, &[9u8; 32], OWNER_ROOT, MY_ROOT, &my_pub,
            &owner_org.signing_key, &owner_x25519, 1500,
        )
        .unwrap();
        (body, ak)
    }

    fn put_acl(s: &mut MemoryStorage) {
        let acl = crate::sync::orgsync::AclRecord {
            owners: vec![OWNER_ROOT.to_string()],
            readers: vec![MY_ROOT.to_string()],
            epoch: 1,
            updated_at: 100,
            reset_by: None,
            sig: String::new(),
        };
        s.put(
            &crate::sync::orgsync::acl_key(ORG, "fin:pay", "1"),
            &serde_json::to_string(&acl).unwrap(),
        )
        .unwrap();
    }

    /// acl 晚到场景（联调 F3 实测形态）：deliver 先到、本地无 acl → 暂存
    /// （不丢）；重评估仍不够格保留；acl 到达后重评估产出 unbox、暂存清除。
    #[test]
    fn deliver_before_acl_stashed_then_reevaluated() {
        let (body, ak) = owner_deliver_body();
        let mut s = MemoryStorage::new();
        crate::org::OrganizationService::save_record(&mut s, &org_record(Some(ak))).unwrap();
        let online = std::collections::HashSet::new();

        // acl 未到 → owners 校验失败（缺省空 acl）→ 暂存，不产生 unbox
        let res = handle_orgkey_deliver(&mut s, &ctx(&online), OWNER_ROOT, &body).unwrap();
        assert_eq!(res.response["ok"], json!(true));
        assert!(res.orgkey_unbox.is_empty(), "同步未到：不产生 unbox");
        let stashed = crate::sync::orgsync::orgkey_stash_for_org(&s, ORG);
        assert_eq!(stashed.len(), 1, "投递被暂存（而非永久丢弃）");
        assert_eq!(stashed[0].1, OWNER_ROOT);

        // 重评估仍不够格（acl 仍未到）→ 保留暂存
        let unboxes = reevaluate_orgkey_stash(&mut s, &ctx(&online), ORG).unwrap();
        assert!(unboxes.is_empty());
        assert_eq!(crate::sync::orgsync::orgkey_stash_for_org(&s, ORG).len(), 1, "仍不够格保留暂存");

        // acl 到达（orgsync 合入）→ 重评估够格 → unbox + 暂存清除
        put_acl(&mut s);
        let unboxes = reevaluate_orgkey_stash(&mut s, &ctx(&online), ORG).unwrap();
        assert_eq!(unboxes.len(), 1, "acl 到达后重评估产出 unbox");
        assert_eq!(unboxes[0].epoch, 1);
        assert!(crate::sync::orgsync::orgkey_stash_for_org(&s, ORG).is_empty(), "暂存清除");
        // 幂等：再评估无产出
        assert!(reevaluate_orgkey_stash(&mut s, &ctx(&online), ORG).unwrap().is_empty());
    }

    /// sender accessKey 晚到同构场景：acl 已在、成员表 sender 无 accessKey →
    /// 暂存；accessKey 随 org:meta 到达后重评估产出 unbox。
    #[test]
    fn deliver_stashed_until_sender_access_key_arrives() {
        let (body, ak) = owner_deliver_body();
        let mut s = MemoryStorage::new();
        // 成员表 owner 无 accessKey（未同步到）
        crate::org::OrganizationService::save_record(&mut s, &org_record(None)).unwrap();
        put_acl(&mut s);
        let online = std::collections::HashSet::new();

        let res = handle_orgkey_deliver(&mut s, &ctx(&online), OWNER_ROOT, &body).unwrap();
        assert!(res.orgkey_unbox.is_empty());
        assert_eq!(crate::sync::orgsync::orgkey_stash_for_org(&s, ORG).len(), 1, "accessKey 未到 → 暂存");

        // 重评估仍缺 accessKey → 保留
        assert!(reevaluate_orgkey_stash(&mut s, &ctx(&online), ORG).unwrap().is_empty());
        assert_eq!(crate::sync::orgsync::orgkey_stash_for_org(&s, ORG).len(), 1);

        // accessKey 到达（org:meta 合入）→ 重评估够格
        crate::org::OrganizationService::save_record(&mut s, &org_record(Some(ak))).unwrap();
        let unboxes = reevaluate_orgkey_stash(&mut s, &ctx(&online), ORG).unwrap();
        assert_eq!(unboxes.len(), 1, "accessKey 到达后重评估产出 unbox");
        assert!(crate::sync::orgsync::orgkey_stash_for_org(&s, ORG).is_empty());
    }

    /// 真拒绝/幂等一律不暂存：签名错误、已有 ≥ epoch、recipient 不符——
    /// 暂存绝不绕过密码学把关。
    #[test]
    fn true_rejects_are_never_stashed() {
        let (body, ak) = owner_deliver_body();
        let online = std::collections::HashSet::new();

        // 签名错误（篡改 sig 为合法 base64 的零签名）→ Drop，不暂存
        let mut s = MemoryStorage::new();
        crate::org::OrganizationService::save_record(&mut s, &org_record(Some(ak.clone()))).unwrap();
        put_acl(&mut s);
        let mut bad = body.clone();
        bad["sig"] = json!(B64.encode([0u8; 64]));
        let res = handle_orgkey_deliver(&mut s, &ctx(&online), OWNER_ROOT, &bad).unwrap();
        assert!(res.orgkey_unbox.is_empty());
        assert!(crate::sync::orgsync::orgkey_stash_for_org(&s, ORG).is_empty(), "签名错误不暂存");

        // 已有 ≥ epoch（幂等）→ Drop，不暂存
        crate::sync::orgsync::put_epoch_key(&mut s, ORG, "fin:pay", "1", 1, &[9u8; 32]);
        let res = handle_orgkey_deliver(&mut s, &ctx(&online), OWNER_ROOT, &body).unwrap();
        assert!(res.orgkey_unbox.is_empty());
        assert!(crate::sync::orgsync::orgkey_stash_for_org(&s, ORG).is_empty(), "已有 ≥ epoch 不暂存");

        // recipient 不符（防转投）→ Drop，不暂存
        let mut s2 = MemoryStorage::new();
        crate::org::OrganizationService::save_record(&mut s2, &org_record(Some(ak))).unwrap();
        put_acl(&mut s2);
        let owner_org = derive_domain_identity(&[7u8; 64], &crate::kernel::Kernel::org_access_domain(ORG));
        let owner_x25519 = crate::sync::orgsync::ed_sk_to_x25519(&owner_org.signing_key.to_bytes());
        let other_pub = crate::sync::orgsync::ed_pk_to_x25519(&owner_org.signing_key.verifying_key().to_bytes()).unwrap();
        let forwarded = crate::sync::orgsync::build_orgkey_deliver(
            ORG, "fin:pay", "1", 1, &[9u8; 32], OWNER_ROOT, "someone-else", &other_pub,
            &owner_org.signing_key, &owner_x25519, 1500,
        )
        .unwrap();
        let res = handle_orgkey_deliver(&mut s2, &ctx(&online), OWNER_ROOT, &forwarded).unwrap();
        assert!(res.orgkey_unbox.is_empty());
        assert!(crate::sync::orgsync::orgkey_stash_for_org(&s2, ORG).is_empty(), "recipient 不符不暂存");
    }
}

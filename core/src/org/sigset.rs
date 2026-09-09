//! 组织签名包（OrgSigSet）五步验证链线上实现
//! （wiki/protocol/community/org-signature.md §2–§5；策略文档与 policyHash 链见
//! org-genesis §5；纯逻辑，不读存储、不碰网络）。
//!
//! 本模块是 [`crate::credential::OrgSigSetVerifier`] 的线上实现：
//! credential 模块（C2）只定义线形与注入接口，五步链（结构 → 策略定位 →
//! 分量验签 → 名册回查 → 策略求值）在此落地；legacy 组织的自认证闭环缺失
//! 按 §5.1 降级标注（接受与否的最终信任裁决归消费方）。

use std::collections::BTreeSet;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::genesis::{
    PolicyVersion, SigningPolicy, genesis_policy_hash, is_genesis_form_org_id,
    verify_genesis_signature,
};
use super::types::is_valid_org_id;
use crate::credential::{OrgSigSet, OrgSigSetVerifier, RosterAnchor, RosterMember};
use crate::evidence::normalize_object;
use crate::identity::verify_ed25519_signature;

/// legacy orgId 降级标注值（org-signature §5.1，逐字稳定）。
pub const DEGRADED_LEGACY_ORG_ID: &str = "legacy-orgId";

/// 五步链拒绝原因（消费方可按需细分；trait 接线只关心通过/拒绝）。
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SigSetReject {
    /// 第 1 步结构：字段形状/绑定校验失败。
    #[error("malformed sigSet: {0}")]
    Malformed(&'static str),

    /// 第 2 步策略定位：按 policyHash 找不到策略文档。
    #[error("policy not found for policyHash")]
    PolicyNotFound,

    /// 第 2 步策略定位：prevPolicyHash 链断裂（指向前版的哈希无对应文档）。
    #[error("policy chain broken at prevPolicyHash")]
    PolicyChainBroken,

    /// 第 2 步策略定位：创世哈希型组织但链尾创世哈希 ≠ orgId 哈希段
    /// （自认证闭环失败）或创世记录签名无效。
    #[error("genesis record is not self-certifying for orgId")]
    GenesisMismatch,

    /// 第 4 步名册回查：快照缺失或 memberSetHash 复算不匹配承诺。
    #[error("roster commitment mismatch")]
    RosterMismatch,

    /// 第 4 步名册回查：存证锚根复算不匹配（sync-evidence §7）。
    #[error("anchor root mismatch")]
    AnchorMismatch,

    /// 第 5 步策略求值：有效管理员签名数不足阈值（含 m>n 或 n>在册
    /// 管理员数导致的「策略在当下名册不可执行」，org-signature §4）。
    #[error("signing policy threshold not met")]
    ThresholdNotMet,
}

/// 验证裁决（§5.1 降级如实标注）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OrgSigSetVerdict {
    /// legacy 组织（`org_<16hex>`）：orgId 无自认证性，名册承诺的可信锚
    /// 退化为传输层可信锚——`true` 时 [`Self::degraded_reason`] 恒为
    /// [`DEGRADED_LEGACY_ORG_ID`]。消费方自行决定是否接受降级证明。
    pub degraded: bool,

    /// 降级标注（未降级 = None）。
    pub degraded_reason: Option<&'static str>,
}

impl OrgSigSetVerdict {
    fn legacy() -> Self {
        Self {
            degraded: true,
            degraded_reason: Some(DEGRADED_LEGACY_ORG_ID),
        }
    }

    fn clean() -> Self {
        Self {
            degraded: false,
            degraded_reason: None,
        }
    }
}

/// §2.1 分量签名载荷：固定 8 键 canonical（绑定名册承诺与策略版本，
/// 防「拿旧名册冒充当下资格」与跨包搬签）。
pub fn component_sign_payload(sig_set: &OrgSigSet) -> String {
    normalize_object(&json!({
        "sigSetV": sig_set.sig_set_v,
        "orgId": sig_set.org_id,
        "subject": sig_set.subject,
        "policyHash": sig_set.policy_hash,
        "memberSetHash": sig_set.roster.member_set_hash,
        "anchorRoot": sig_set.roster.anchor.anchor_root,
        "anchorTs": sig_set.roster.anchor.ts,
        "signedAt": sig_set.signed_at,
    }))
}

/// §3 名册状态承诺复算：`sha256hex(normalizeObject(按 identity 字典序排序的
/// 成员条目数组))`；条目只含 identity 与 role 两键。
pub fn roster_member_set_hash(members: &[RosterMember]) -> String {
    let mut entries: Vec<_> = members
        .iter()
        .map(|m| json!({ "identity": m.identity, "role": m.role }))
        .collect();
    entries.sort_by(|a, b| a["identity"].as_str().cmp(&b["identity"].as_str()));
    hex::encode(Sha256::digest(
        normalize_object(&Value::Array(entries)).as_bytes(),
    ))
}

fn is_hex64(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// 线上验证上下文：策略链 + 锚/名册来源注入（纯逻辑；存储读写由调用方
/// 以闭包形式供给，保持本模块无状态可测）。
pub struct OrgSigSetVerifyContext<'a> {
    /// 已知策略版本（创世 + 全部修订，任意顺序；按哈希沿 prevPolicyHash
    /// 链接回溯，org-genesis §5）。legacy 组织自愿发布的创世记录同样承载。
    pub policies: &'a [PolicyVersion],

    /// 存证锚根复算（sync-evidence §7）：锚引用 → 本地持锚记录复算锚根
    /// 是否匹配。锚缺失/过期的降级处置由实现闭包决定（返回 false = 拒绝）。
    pub anchor_matches: &'a dyn Fn(&RosterAnchor) -> bool,

    /// 名册快照本地来源（org-signature §2 roster.snapshot 可省）：包内未
    /// 随带快照时按锚引用取验证方本地副本。两种路径都复算 memberSetHash。
    pub roster_lookup: &'a dyn Fn(&RosterAnchor) -> Option<Vec<RosterMember>>,
}

impl OrgSigSetVerifyContext<'_> {
    /// 五步验证链（org-signature §5，按序全过才接受）；返回裁决（含降级标注）。
    pub fn verify_detailed(&self, sig_set: &OrgSigSet) -> Result<OrgSigSetVerdict, SigSetReject> {
        self.check_structure(sig_set)?; // 第 1 步
        let verdict = self.locate_policy(sig_set)?; // 第 2 步
        let valid_signers = self.verify_components(sig_set)?; // 第 3 步
        let (snapshot, valid_admins) = self.check_roster(sig_set, &valid_signers)?; // 第 4 步
        self.evaluate_policy(sig_set, &snapshot, valid_admins.len())?; // 第 5 步
        Ok(verdict)
    }

    /// 第 1 步 结构：字段形状 + signer↔publicKey 绑定（去重是计数口径，
    /// 见 verify_components）。
    fn check_structure(&self, sig_set: &OrgSigSet) -> Result<(), SigSetReject> {
        if sig_set.sig_set_v != 1 {
            return Err(SigSetReject::Malformed("sigSetV must be 1"));
        }
        if !is_valid_org_id(&sig_set.org_id) {
            return Err(SigSetReject::Malformed("orgId dual-form"));
        }
        if !is_hex64(&sig_set.subject) {
            return Err(SigSetReject::Malformed("subject must be 64 hex"));
        }
        if !is_hex64(&sig_set.policy_hash) || !is_hex64(&sig_set.roster.member_set_hash) {
            return Err(SigSetReject::Malformed(
                "policyHash/memberSetHash must be 64 hex",
            ));
        }
        if !is_hex64(&sig_set.roster.anchor.anchor_root) {
            return Err(SigSetReject::Malformed("anchorRoot must be 64 hex"));
        }
        if sig_set.roster.anchor.org_id != sig_set.org_id {
            return Err(SigSetReject::Malformed(
                "anchor orgId must equal sigSet orgId",
            ));
        }
        // signer↔publicKey 绑定逐分量校验；去重只影响计数（验签步跳过重复
        // signer，org-signature §2「同 signer 重复 = 只计一次」）。
        for comp in &sig_set.signatures {
            let Ok(raw) = base64::Engine::decode(
                &base64::engine::general_purpose::STANDARD,
                &comp.public_key,
            ) else {
                return Err(SigSetReject::Malformed("publicKey base64"));
            };
            if raw.len() != 32 || hex::encode(Sha256::digest(&raw)) != comp.signer {
                return Err(SigSetReject::Malformed("signer != sha256(publicKey)"));
            }
        }
        Ok(())
    }

    /// 第 2 步 策略定位：按 policyHash 定位版本，沿 prevPolicyHash 链回溯
    /// （创世哈希型须闭环至创世记录且 orgId 哈希段 == policyHash₀；legacy
    /// 链尾为最早已知版本，自认证闭环缺失 → 降级标注）。
    fn locate_policy(&self, sig_set: &OrgSigSet) -> Result<OrgSigSetVerdict, SigSetReject> {
        let hash_of = |p: &PolicyVersion| p.policy_hash().unwrap_or_default();
        let mut current = self
            .policies
            .iter()
            .find(|p| hash_of(p) == sig_set.policy_hash)
            .ok_or(SigSetReject::PolicyNotFound)?;
        // 链回溯（visited 防环：链本身必须是 DAG）
        let mut visited = BTreeSet::from([sig_set.policy_hash.clone()]);
        let terminal: &PolicyVersion = loop {
            let Some(prev) = current.prev_policy_hash() else {
                break current;
            };
            if !visited.insert(prev.to_string()) {
                return Err(SigSetReject::PolicyChainBroken);
            }
            current = self
                .policies
                .iter()
                .find(|p| hash_of(p) == prev)
                .ok_or(SigSetReject::PolicyChainBroken)?;
        };

        if is_genesis_form_org_id(&sig_set.org_id) {
            // 自认证：链尾必须是创世记录、签名有效且 orgId 哈希段 == policyHash₀
            let PolicyVersion::Genesis(genesis) = terminal else {
                return Err(SigSetReject::GenesisMismatch);
            };
            if !verify_genesis_signature(genesis)
                || genesis_policy_hash(genesis).ok().as_deref() != Some(&sig_set.org_id[4..])
            {
                return Err(SigSetReject::GenesisMismatch);
            }
            Ok(OrgSigSetVerdict::clean())
        } else {
            // legacy：orgId 无自认证性，无法闭环至创世哈希——如实降级标注。
            if let PolicyVersion::Genesis(genesis) = terminal {
                // legacy 自愿发布的创世记录仍须是有效的策略公开锚。
                if !verify_genesis_signature(genesis) {
                    return Err(SigSetReject::GenesisMismatch);
                }
            }
            Ok(OrgSigSetVerdict::legacy())
        }
    }

    /// 第 3 步 分量验签：重建 §2.1 载荷逐分量 detached verify；返回验签通过的
    /// 去重 signer 集合（非 admin 不剔除——角色回查是第 4 步的事）。
    fn verify_components<'s>(
        &self,
        sig_set: &'s OrgSigSet,
    ) -> Result<BTreeSet<&'s str>, SigSetReject> {
        let payload = component_sign_payload(sig_set);
        let mut valid = BTreeSet::new();
        for comp in &sig_set.signatures {
            if valid.contains(comp.signer.as_str()) {
                continue; // 去重：同 signer 重复只计一次
            }
            if verify_ed25519_signature(&payload, &comp.sig, &comp.public_key) {
                valid.insert(comp.signer.as_str());
            }
        }
        Ok(valid)
    }

    /// 第 4 步 名册回查：快照（随包或本地副本）复算 memberSetHash 匹配承诺 →
    /// 锚根复算匹配 → 返回快照与「验签通过且在快照中为 admin」的签名者集合。
    fn check_roster<'s>(
        &self,
        sig_set: &OrgSigSet,
        valid_signers: &BTreeSet<&'s str>,
    ) -> Result<(Vec<RosterMember>, BTreeSet<&'s str>), SigSetReject> {
        let snapshot = match &sig_set.roster.snapshot {
            Some(s) => s.clone(),
            None => {
                (self.roster_lookup)(&sig_set.roster.anchor).ok_or(SigSetReject::RosterMismatch)?
            }
        };
        if snapshot
            .iter()
            .any(|m| m.role != "admin" && m.role != "member")
        {
            return Err(SigSetReject::Malformed("roster role must be admin|member"));
        }
        if roster_member_set_hash(&snapshot) != sig_set.roster.member_set_hash {
            return Err(SigSetReject::RosterMismatch);
        }
        if !(self.anchor_matches)(&sig_set.roster.anchor) {
            return Err(SigSetReject::AnchorMismatch);
        }
        // 双键兼容（A16 双写过渡）：签名者可按 rootId（旧包/未迁移端）或
        // org_user_id（域私钥新签）命中名册——任一键在册即认可。
        let admins: BTreeSet<&str> = snapshot
            .iter()
            .filter(|m| m.role == "admin")
            .flat_map(|m| {
                [m.identity.as_str()]
                    .into_iter()
                    .chain(m.org_user_id.as_deref())
            })
            .collect();
        let valid_admins: BTreeSet<&'s str> = valid_signers
            .iter()
            .filter(|s| admins.contains(**s))
            .copied()
            .collect();
        Ok((snapshot, valid_admins))
    }

    /// 第 5 步 策略求值（org-signature §4）：any-admin ≥1 个有效 admin 分量；
    /// m-of-n ≥m 个且策略在当下名册可执行（m ≤ n ≤ 在册 admin 数）。
    fn evaluate_policy(
        &self,
        sig_set: &OrgSigSet,
        snapshot: &[RosterMember],
        valid_admin_count: usize,
    ) -> Result<(), SigSetReject> {
        let policy = self
            .policies
            .iter()
            .find(|p| p.policy_hash().unwrap_or_default() == sig_set.policy_hash)
            .ok_or(SigSetReject::PolicyNotFound)?;
        let admin_count = snapshot.iter().filter(|m| m.role == "admin").count();
        let valid_admin_sigs = valid_admin_count as u32;
        match policy.signing_policy() {
            SigningPolicy::AnyAdmin if valid_admin_sigs >= 1 => Ok(()),
            SigningPolicy::AnyAdmin => Err(SigSetReject::ThresholdNotMet),
            SigningPolicy::MOfN { m, n } => {
                // 策略在当下名册不可执行（m>n 或 n>在册 admin 数）→ 拒
                // （走失联恢复：delayed-veto 修改策略，affair §5.3 硬校验）。
                if m > n || *n as usize > admin_count || valid_admin_sigs < *m {
                    Err(SigSetReject::ThresholdNotMet)
                } else {
                    Ok(())
                }
            }
        }
    }
}

impl OrgSigSetVerifier for OrgSigSetVerifyContext<'_> {
    /// credential §4 合入校验要求完整通过：legacy 降级证明同样通过五步链
    /// （降级标注经 [`Self::verify_detailed`] 暴露，信任裁决归消费方）。
    fn verify_org_sig_set(&self, sig_set: &OrgSigSet) -> bool {
        self.verify_detailed(sig_set).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credential::ComponentSignature;
    use crate::org::genesis::{
        GenesisPolicyRecord, SigningPolicy as GenesisSigningPolicy, TransitionDecl, VetoThreshold,
        genesis_org_id, sign_genesis_record,
    };
    use crate::org::types::DomainType;
    use base64::Engine as _;
    use ed25519_dalek::{Signer as _, SigningKey};

    const NOW: i64 = 1_720_000_000_000;

    fn key(byte: u8) -> SigningKey {
        SigningKey::from_bytes(&[byte; 32])
    }

    fn b64(bytes: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

    fn identity_of(k: &SigningKey) -> String {
        hex::encode(Sha256::digest(k.verifying_key().to_bytes()))
    }

    struct Fixture {
        org_id: String,
        genesis: GenesisPolicyRecord,
        admins: Vec<SigningKey>,
        policy_hash: String,
    }

    /// 创世哈希型组织：3 管理员 any-admin。
    fn community_fixture() -> Fixture {
        let root = key(0x31);
        let admins = vec![key(0x41), key(0x42), key(0x43)];
        let genesis = GenesisPolicyRecord {
            genesis_v: 1,
            name: "共同体甲".to_string(),
            description: "测试共同体".to_string(),
            domain_type: DomainType::Community,
            root_public_key: b64(&root.verifying_key().to_bytes()),
            org_address: "a".repeat(55),
            signing_policy: GenesisSigningPolicy::AnyAdmin,
            transition: Some(TransitionDecl {
                kind: "delayed-veto".to_string(),
                delay_ms: 259_200_000,
                veto_threshold: VetoThreshold { count: 1 },
                when_members_exceed: 1,
            }),
            born_of: None,
            created_by: "ab".repeat(32),
            created_at: NOW,
            sig: String::new(),
        };
        let mut genesis = genesis;
        sign_genesis_record(&mut genesis, &root);
        let org_id = genesis_org_id(&genesis).unwrap();
        let policy_hash = genesis_policy_hash(&genesis).unwrap();
        Fixture {
            org_id,
            genesis,
            admins,
            policy_hash,
        }
    }

    /// 构造签名包：org_id / subject / 快照由调用方给定（分量载荷绑定 orgId，
    /// 签名后替换字段必败——legacy 用例直接用 legacy orgId 构造）。
    fn sign_org_sig_set(
        fixture: &Fixture,
        org_id: &str,
        subject: &str,
        snapshot: &[RosterMember],
        signers: &[&SigningKey],
    ) -> OrgSigSet {
        let mut sig_set = OrgSigSet {
            sig_set_v: 1,
            org_id: org_id.to_string(),
            subject: subject.to_string(),
            policy_hash: fixture.policy_hash.clone(),
            roster: crate::credential::RosterCommitment {
                member_set_hash: roster_member_set_hash(snapshot),
                anchor: RosterAnchor {
                    org_id: org_id.to_string(),
                    anchor_root: "cd".repeat(32),
                    ts: NOW,
                },
                snapshot: Some(snapshot.to_vec()),
            },
            signed_at: NOW,
            signatures: Vec::new(),
        };
        re_sign(&mut sig_set, signers);
        sig_set
    }

    fn re_sign(sig_set: &mut OrgSigSet, signers: &[&SigningKey]) {
        let payload = component_sign_payload(sig_set);
        sig_set.signatures = signers
            .iter()
            .map(|k| ComponentSignature {
                signer: identity_of(k),
                public_key: b64(&k.verifying_key().to_bytes()),
                sig: b64(&k.sign(payload.as_bytes()).to_bytes()),
            })
            .collect();
    }

    fn full_admin_roster(fixture: &Fixture) -> Vec<RosterMember> {
        fixture
            .admins
            .iter()
            .map(|k| RosterMember {
                identity: identity_of(k),
                role: "admin".to_string(),
                org_user_id: None,
            })
            .collect()
    }

    fn ctx<'a>(policies: &'a [PolicyVersion]) -> OrgSigSetVerifyContext<'a> {
        OrgSigSetVerifyContext {
            policies,
            anchor_matches: &|_| true, // 单测锚根用替身（向量为固定替身口径）
            roster_lookup: &|_| None,
        }
    }

    #[test]
    fn any_admin_single_signature_passes() {
        let fx = community_fixture();
        let policies = vec![PolicyVersion::Genesis(fx.genesis.clone())];
        let snapshot = full_admin_roster(&fx);
        let sig_set = sign_org_sig_set(
            &fx,
            &fx.org_id,
            &"ab".repeat(32),
            &snapshot,
            &[&fx.admins[0]],
        );
        let verdict = ctx(&policies).verify_detailed(&sig_set).unwrap();
        assert!(!verdict.degraded);
        assert!(ctx(&policies).verify_org_sig_set(&sig_set));
    }

    /// A16 双写过渡：域私钥签名（signer=org_user_id）按名册 orgUserId 命中；
    /// 同名册上 rootId 旧签同样可验（双键兼容）；member 角色的 orgUserId 不算
    /// admin 签名。
    #[test]
    fn domain_signer_and_legacy_root_signer_both_pass_dual_roster() {
        let fx = community_fixture();
        let policies = vec![PolicyVersion::Genesis(fx.genesis.clone())];
        let domain_key = key(0x77);
        let member_domain = key(0x78);
        let snapshot = vec![
            RosterMember {
                identity: identity_of(&fx.admins[0]),
                role: "admin".to_string(),
                org_user_id: Some(identity_of(&domain_key)),
            },
            RosterMember {
                identity: identity_of(&fx.admins[1]),
                role: "admin".to_string(),
                org_user_id: None,
            },
            RosterMember {
                identity: identity_of(&fx.admins[2]),
                role: "admin".to_string(),
                org_user_id: None,
            },
            RosterMember {
                identity: identity_of(&key(0x10)),
                role: "member".to_string(),
                org_user_id: Some(identity_of(&member_domain)),
            },
        ];

        // 域私钥签名（signer=org_user_id）→ 通过（org-signature §2.1 口径）。
        let domain_set = sign_org_sig_set(
            &fx,
            &fx.org_id,
            &"ab".repeat(32),
            &snapshot,
            &[&domain_key],
        );
        assert_eq!(domain_set.signatures[0].signer, identity_of(&domain_key));
        assert!(
            ctx(&policies).verify_org_sig_set(&domain_set),
            "org_user_id 命中名册（双键兼容）"
        );

        // 旧 rootId 签名（同一份双写名册）→ 通过（未迁移端签出的包仍可验）。
        let legacy_set = sign_org_sig_set(
            &fx,
            &fx.org_id,
            &"ab".repeat(32),
            &snapshot,
            &[&fx.admins[1]],
        );
        assert!(
            ctx(&policies).verify_org_sig_set(&legacy_set),
            "rootId 旧签兼容"
        );

        // member 角色的 orgUserId 不算 admin 签名 → 阈值不足拒绝。
        let bad = sign_org_sig_set(
            &fx,
            &fx.org_id,
            &"ab".repeat(32),
            &snapshot,
            &[&member_domain],
        );
        assert_eq!(
            ctx(&policies).verify_detailed(&bad).unwrap_err(),
            SigSetReject::ThresholdNotMet
        );
    }

    #[test]
    fn non_admin_signer_fails_any_admin() {
        let fx = community_fixture();
        let policies = vec![PolicyVersion::Genesis(fx.genesis.clone())];
        let snapshot = full_admin_roster(&fx);
        let outsider = key(0x99);
        let sig_set = sign_org_sig_set(&fx, &fx.org_id, &"ab".repeat(32), &snapshot, &[&outsider]);
        assert_eq!(
            ctx(&policies).verify_detailed(&sig_set).unwrap_err(),
            SigSetReject::ThresholdNotMet
        );
    }

    #[test]
    fn tampered_subject_breaks_component_signatures() {
        let fx = community_fixture();
        let policies = vec![PolicyVersion::Genesis(fx.genesis.clone())];
        let snapshot = full_admin_roster(&fx);
        let mut sig_set = sign_org_sig_set(
            &fx,
            &fx.org_id,
            &"ab".repeat(32),
            &snapshot,
            &[&fx.admins[0]],
        );
        sig_set.subject = "cd".repeat(32);
        assert_eq!(
            ctx(&policies).verify_detailed(&sig_set).unwrap_err(),
            SigSetReject::ThresholdNotMet
        );
    }

    #[test]
    fn roster_commitment_tamper_rejected() {
        let fx = community_fixture();
        let policies = vec![PolicyVersion::Genesis(fx.genesis.clone())];
        let snapshot = full_admin_roster(&fx);
        let mut sig_set = sign_org_sig_set(
            &fx,
            &fx.org_id,
            &"ab".repeat(32),
            &snapshot,
            &[&fx.admins[0]],
        );
        sig_set.roster.member_set_hash = "0".repeat(64);
        assert_eq!(
            ctx(&policies).verify_detailed(&sig_set).unwrap_err(),
            SigSetReject::RosterMismatch
        );
    }

    #[test]
    fn anchor_root_mismatch_rejected() {
        let fx = community_fixture();
        let snapshot = full_admin_roster(&fx);
        let sig_set = sign_org_sig_set(
            &fx,
            &fx.org_id,
            &"ab".repeat(32),
            &snapshot,
            &[&fx.admins[0]],
        );
        let ctx = OrgSigSetVerifyContext {
            policies: &vec![PolicyVersion::Genesis(fx.genesis.clone())],
            anchor_matches: &|_| false,
            roster_lookup: &|_| None,
        };
        assert_eq!(
            ctx.verify_detailed(&sig_set).unwrap_err(),
            SigSetReject::AnchorMismatch
        );
    }

    #[test]
    fn m_of_n_requires_threshold() {
        let fx = community_fixture();
        // 修订策略 2/3（单测直接构造修订记录；线上修订签名链归治理钩子 C6）
        let revision = crate::org::genesis::PolicyRevisionRecord {
            policy_v: 1,
            org_id: fx.org_id.clone(),
            seq: 1,
            prev_policy_hash: fx.policy_hash.clone(),
            signing_policy: GenesisSigningPolicy::MOfN { m: 2, n: 3 },
            updated_at: NOW + 10_000,
            sig_set: sign_org_sig_set(
                &fx,
                &fx.org_id,
                &"ef".repeat(32),
                &full_admin_roster(&fx),
                &[&fx.admins[0]],
            ),
        };
        let revision_hash = crate::org::genesis::policy_revision_hash(&revision).unwrap();
        let policies = vec![
            PolicyVersion::Genesis(fx.genesis.clone()),
            PolicyVersion::Revision(revision),
        ];
        // 快照角色：三人均为 admin（n=3 ≤ 在册 admin 数，策略可执行）
        let snapshot = full_admin_roster(&fx);
        let mut one = sign_org_sig_set(
            &fx,
            &fx.org_id,
            &"ab".repeat(32),
            &snapshot,
            &[&fx.admins[0]],
        );
        one.policy_hash = revision_hash.clone();
        re_sign(&mut one, &[&fx.admins[0]]);
        let mut two = sign_org_sig_set(
            &fx,
            &fx.org_id,
            &"ab".repeat(32),
            &snapshot,
            &[&fx.admins[0], &fx.admins[1]],
        );
        two.policy_hash = revision_hash.clone();
        re_sign(&mut two, &[&fx.admins[0], &fx.admins[1]]);
        let ctx = ctx(&policies);
        assert_eq!(
            ctx.verify_detailed(&one).unwrap_err(),
            SigSetReject::ThresholdNotMet
        );
        assert!(ctx.verify_detailed(&two).is_ok());
    }

    #[test]
    fn legacy_org_id_verifies_with_degraded_annotation() {
        let fx = community_fixture();
        // legacy orgId：同一创世记录自愿发布，orgId 保持 16hex 随机形态。
        let legacy_org_id = format!("org_{}", "ab".repeat(8));
        let snapshot = full_admin_roster(&fx);
        let sig_set = sign_org_sig_set(
            &fx,
            &legacy_org_id,
            &"ab".repeat(32),
            &snapshot,
            &[&fx.admins[0]],
        );
        let policies = vec![PolicyVersion::Genesis(fx.genesis.clone())];
        let verdict = ctx(&policies).verify_detailed(&sig_set).unwrap();
        assert!(verdict.degraded);
        assert_eq!(verdict.degraded_reason, Some(DEGRADED_LEGACY_ORG_ID));
        // trait 接线：降级证明同样通过（信任裁决归消费方）
        assert!(ctx(&policies).verify_org_sig_set(&sig_set));
    }

    #[test]
    fn genesis_form_org_id_requires_self_certifying_closure() {
        let fx = community_fixture();
        // 创世哈希型 orgId 但 policyHash 指向无关策略 → 自认证闭环失败
        let snapshot = full_admin_roster(&fx);
        let mut sig_set = sign_org_sig_set(
            &fx,
            &fx.org_id,
            &"ab".repeat(32),
            &snapshot,
            &[&fx.admins[0]],
        );
        sig_set.policy_hash = "0".repeat(64);
        let policies = vec![PolicyVersion::Genesis(fx.genesis.clone())];
        assert_eq!(
            ctx(&policies).verify_detailed(&sig_set).unwrap_err(),
            SigSetReject::PolicyNotFound
        );
    }
}

//! 组织创世策略记录、新 orgId（`org_<64hex>` 自认证）、策略修订链、组织域
//! 身份派生、成员种类约束与 DAG 成环检查（wiki/protocol/community/org-genesis.md，
//! 总体方案 community-affairs §4）。
//!
//! 全部为纯逻辑：不读存储、不碰网络；存储键（`org:genesis:` / `org:policy:`）
//! 由调用方使用本模块的键构造函数落库（org:structure@v1 键域，写一次不可变 /
//! 追加语义见规格 §2.1/§5）。
//!
//! orgId 双形态（§2）：legacy `org_<16hex>` 随机（既有）与创世哈希型
//! `org_<64hex>` 并存，识别 = 长度判别（[`is_valid_org_id`]）。

use aes_gcm::KeyInit as _;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use ed25519_dalek::{Signer, SigningKey};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256, Sha512};

use super::org_address::org_address_from_public_key;
use super::types::{DomainType, MemberKind};
use super::{OrgError, Result};
use crate::credential::OrgSigSet;
use crate::evidence::normalize_object;

/// 创世策略记录存储键前缀（org-genesis §2.1：`org:genesis:{orgId}`，单记录，
/// 写一次不可变；legacy 组织自愿发布时同键承载，orgId 不变）。
pub const ORG_GENESIS_PREFIX: &str = "org:genesis:";

/// 创世策略记录存储键。
pub fn org_genesis_key(org_id: &str) -> String {
    format!("{ORG_GENESIS_PREFIX}{org_id}")
}

/// 策略修订记录存储键前缀（org-genesis §5：`org:policy:{orgId}:{seq:06}`，
/// 追加语义）。
pub const ORG_POLICY_PREFIX: &str = "org:policy:";

/// 策略修订记录存储键（`seq` 从 1 递增；创世为 seq 0，存 `org:genesis:`）。
pub fn org_policy_key(org_id: &str, seq: u64) -> String {
    format!("{ORG_POLICY_PREFIX}{org_id}:{seq:06}")
}

/// 组织签名策略（org-genesis §1 signingPolicy；求值规则见 org-signature §4）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum SigningPolicy {
    /// 任一现任管理员签（默认）。
    AnyAdmin,
    /// m/n 管理员多签（1 ≤ m ≤ n）。
    MOfN {
        /// 通过所需的不同管理员签名数。
        m: u32,
        /// 策略声明值（实现校验 m ≤ n ≤ 快照内 admin 数）。
        n: u32,
    },
}

/// 单人→集体决策过渡声明（org-genesis §1 transition；可为 null）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransitionDecl {
    /// 过渡机制（产品「创建者默认值」；默认 delayed-veto）。
    pub kind: String,
    /// 延迟生效时长（ms）。
    #[serde(rename = "delayMs")]
    pub delay_ms: i64,
    /// 否决阈值。
    #[serde(rename = "vetoThreshold")]
    pub veto_threshold: VetoThreshold,
    /// 成员数超过该值后策略修改改走集体决策。
    #[serde(rename = "whenMembersExceed")]
    pub when_members_exceed: u32,
}

/// 否决阈值声明（transition 内嵌）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VetoThreshold {
    /// 否决所需人数。
    pub count: u32,
}

/// 产品「创建者默认值」过渡声明（org-genesis §1 transition 默认值：成员数
/// 超过 1 后策略修改改走 delayed-veto，72 小时延迟、否决阈值 1 人）。
pub fn default_transition_decl() -> TransitionDecl {
    TransitionDecl {
        kind: "delayed-veto".to_string(),
        delay_ms: 259_200_000,
        veto_threshold: VetoThreshold { count: 1 },
        when_members_exceed: 1,
    }
}

/// 出生证明（org-genesis §6，创世策略记录可选字段）：决议创设组织时的公开
/// 合法性来源。普通（手动）创建为 null——键缺失（serde 丢键），不进入
/// canonical 载荷，既有向量与线形字节不变。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BornOf {
    /// 决议所属事务 id（64hex）。
    #[serde(rename = "affairId")]
    pub affair_id: String,
    /// 决议 id（opHash）。
    #[serde(rename = "resolutionId")]
    pub resolution_id: String,
}

/// 创世策略记录（org-genesis §1 线形；新创建组织一律携带出生）。
///
/// `sig` 恒为最后一个字段（签名载荷 = canonical 剔除 sig 的全部字段）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GenesisPolicyRecord {
    /// 恒 1。
    #[serde(rename = "genesisV")]
    pub genesis_v: u32,
    /// 组织名（trim + 连续空白归一，同 org-record §3.1）。
    pub name: String,
    /// 描述。
    pub description: String,
    /// 域类型：`leaf` | `community`；创建时确定、不可变更。
    #[serde(rename = "domainType")]
    pub domain_type: DomainType,
    /// 组织根 Ed25519 公钥（base64 原始 32 字节；org-address §15 同一密钥体系）。
    #[serde(rename = "rootPublicKey")]
    pub root_public_key: String,
    /// 自认证组织地址（互绑 C1：必须等于 §15 公式对 rootPublicKey 的派生值）。
    #[serde(rename = "orgAddress")]
    pub org_address: String,
    /// 组织签名策略。
    #[serde(rename = "signingPolicy")]
    pub signing_policy: SigningPolicy,
    /// 过渡声明（可为 null：创建者即集体）。
    pub transition: Option<TransitionDecl>,
    /// 出生证明（org-genesis §6；决议创设组织时携带，普通创建缺省丢键）。
    #[serde(rename = "bornOf", default, skip_serializing_if = "Option::is_none")]
    pub born_of: Option<BornOf>,
    /// 创建者个人 rootId（自动为首任 admin）。
    #[serde(rename = "createdBy")]
    pub created_by: String,
    /// 创建时刻（Unix 毫秒）。
    #[serde(rename = "createdAt")]
    pub created_at: i64,
    /// 组织根私钥对 `canonical(剔除 sig 的全部字段)` 的签名（base64 64B）。
    pub sig: String,
}

/// `canonical(剔除 sig 的全部字段)`（创世签名载荷，org-genesis §1）。
pub fn genesis_sign_payload(record: &GenesisPolicyRecord) -> Result<String> {
    let mut value = serde_json::to_value(record)?;
    if let Value::Object(map) = &mut value {
        map.shift_remove("sig");
    }
    Ok(normalize_object(&value))
}

/// `policyHash₀ = sha256hex(normalizeObject(创世剔除 sig))`（org-genesis §5）——
/// 即新形态 orgId 的哈希段。
pub fn genesis_policy_hash(record: &GenesisPolicyRecord) -> Result<String> {
    Ok(hex::encode(Sha256::digest(
        genesis_sign_payload(record)?.as_bytes(),
    )))
}

/// 新形态 orgId = `org_` + genesis_policy_hash（org-genesis §2）。
pub fn genesis_org_id(record: &GenesisPolicyRecord) -> Result<String> {
    Ok(format!("org_{}", genesis_policy_hash(record)?))
}

/// 创世记录签名验证：组织根公钥对签名载荷的 detached 验签。
pub fn verify_genesis_signature(record: &GenesisPolicyRecord) -> bool {
    let Ok(payload) = genesis_sign_payload(record) else {
        return false;
    };
    crate::identity::verify_ed25519_signature(&payload, &record.sig, &record.root_public_key)
}

/// 互绑（C1）复算校验（org-genesis §1 orgAddress 字段）：orgAddress 必须等于
/// §15 公式对 rootPublicKey 的派生值（`base32(sha256(pub)‖checksum)`）——创世
/// 记录与组织地址记录互相锚定，地址与根公钥不匹配的创世记录必须被拒。
/// rootPublicKey 非合法 base64 32 字节时同属不匹配。
pub fn verify_org_address_binding(record: &GenesisPolicyRecord) -> bool {
    let Ok(public_key_bytes) = B64.decode(record.root_public_key.as_bytes()) else {
        return false;
    };
    let Ok(public_key) = <[u8; 32]>::try_from(public_key_bytes.as_slice()) else {
        return false;
    };
    record.org_address == org_address_from_public_key(&public_key)
}

/// 组织根私钥签创世记录（创建路径用；纯函数，密钥由调用方持有）。
pub fn sign_genesis_record(record: &mut GenesisPolicyRecord, root_key: &SigningKey) {
    let payload = genesis_sign_payload(record).expect("genesis payload serialize");
    record.sig = B64.encode(root_key.sign(payload.as_bytes()).to_bytes());
}

/// 创世哈希型判别：`org_<64hex>`（org-genesis §2）。
pub fn is_genesis_form_org_id(org_id: &str) -> bool {
    org_id.len() == 4 + 64 && crate::org::types::is_valid_org_id(org_id)
}

/// 策略修订记录（org-genesis §5 线形；seq 0 = 创世策略记录）。
///
/// 修订 = 追加记录：`prevPolicyHash` 指向前版，签名集合按**修订前**策略求值。
/// `sigSet` 恒为最后一个字段（policyHash = sha256hex(normalizeObject(剔除 sigSet))）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PolicyRevisionRecord {
    /// 恒 1。
    #[serde(rename = "policyV")]
    pub policy_v: u32,
    /// 所属组织（双形态）。
    #[serde(rename = "orgId")]
    pub org_id: String,
    /// 修订序号（从 1 递增；创世为 0）。
    pub seq: u64,
    /// 前版策略文档哈希（首条修订 = policyHash₀）。
    #[serde(rename = "prevPolicyHash")]
    pub prev_policy_hash: String,
    /// 新版组织签名策略。
    #[serde(rename = "signingPolicy")]
    pub signing_policy: SigningPolicy,
    /// 修订时刻（Unix 毫秒）。
    #[serde(rename = "updatedAt")]
    pub updated_at: i64,
    /// 组织签名包（按 prevPolicyHash 对应策略签名）。
    #[serde(rename = "sigSet")]
    pub sig_set: OrgSigSet,
}

/// `canonical(剔除 sigSet 的全部字段)`（修订签名载荷 + policyHash 输入）。
pub fn policy_revision_payload(revision: &PolicyRevisionRecord) -> Result<String> {
    let mut value = serde_json::to_value(revision)?;
    if let Value::Object(map) = &mut value {
        map.shift_remove("sigSet");
    }
    Ok(normalize_object(&value))
}

/// `policyHash = sha256hex(normalizeObject(策略文档剔除 sigSet))`（org-genesis §5）。
pub fn policy_revision_hash(revision: &PolicyRevisionRecord) -> Result<String> {
    Ok(hex::encode(Sha256::digest(
        policy_revision_payload(revision)?.as_bytes(),
    )))
}

/// 策略版本（验证方按 policyHash 定位的链上节点）：创世（seq 0）或修订。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PolicyVersion {
    /// seq 0：创世策略记录。
    Genesis(GenesisPolicyRecord),
    /// seq ≥ 1：策略修订记录。
    Revision(PolicyRevisionRecord),
}

impl PolicyVersion {
    /// 策略文档哈希（创世 = policyHash₀；修订 = policyHash）。
    pub fn policy_hash(&self) -> Result<String> {
        match self {
            Self::Genesis(g) => genesis_policy_hash(g),
            Self::Revision(r) => policy_revision_hash(r),
        }
    }

    /// 组织签名策略。
    pub fn signing_policy(&self) -> &SigningPolicy {
        match self {
            Self::Genesis(g) => &g.signing_policy,
            Self::Revision(r) => &r.signing_policy,
        }
    }

    /// 修订序号（创世 = 0）。
    pub fn seq(&self) -> u64 {
        match self {
            Self::Genesis(_) => 0,
            Self::Revision(r) => r.seq,
        }
    }

    /// 前版策略哈希（创世 = None，链尾）。
    pub fn prev_policy_hash(&self) -> Option<&str> {
        match self {
            Self::Genesis(_) => None,
            Self::Revision(r) => Some(&r.prev_policy_hash),
        }
    }
}

/// 组织域身份（org-genesis §4）：组织根密钥对是裸 Ed25519（无 SLIP-0010
/// 链码），域派生另行定义：
/// `domainSeed = HMAC-SHA512(key = 组织根私钥 32B 原始字节,
///                            data = utf8("spark:org-domain:") ‖ utf8(domain))[0:32]`。
pub struct OrgDomainIdentity {
    /// 域身份签名私钥（按需派生，不独立存储）。
    pub signing_key: SigningKey,
}

impl OrgDomainIdentity {
    /// 由组织根私钥 + 域串派生（域串如 `community:{communityOrgId}` /
    /// `affair:{affairId}`；跨域不可关联）。
    pub fn derive(org_root_key: &SigningKey, domain: &str) -> Self {
        let mut mac =
            Hmac::<Sha512>::new_from_slice(org_root_key.as_bytes()).expect("hmac accepts 32B key");
        mac.update(b"spark:org-domain:");
        mac.update(domain.as_bytes());
        let seed: [u8; 32] = mac.finalize().into_bytes()[..32]
            .try_into()
            .expect("hmac-sha512 output >= 32B");
        Self {
            signing_key: SigningKey::from_bytes(&seed),
        }
    }

    /// 域身份公钥（base64 原始 32 字节）。
    pub fn public_key_b64(&self) -> String {
        B64.encode(self.signing_key.verifying_key().to_bytes())
    }

    /// 域身份 id = sha256hex(公钥原始字节)（`^[0-9a-f]{64}$`，成员条目 rootId 槽位）。
    pub fn identity(&self) -> String {
        hex::encode(Sha256::digest(self.signing_key.verifying_key().to_bytes()))
    }
}

/// 成员种类约束（org-genesis §3.2 内核硬规则）：`community` 域只接受组织成员；
/// `leaf` 域只接受个人成员。加入验证时强制，任何组织策略不得覆盖。
pub fn enforce_member_kind(domain_type: DomainType, kind: MemberKind) -> Result<()> {
    match (domain_type, kind) {
        (DomainType::Community, MemberKind::Org) => Ok(()),
        (DomainType::Leaf, MemberKind::Person) => Ok(()),
        (DomainType::Community, MemberKind::Person) => Err(OrgError::MemberKindNotAllowed(
            "Community domain only accepts organization members".to_string(),
        )),
        (DomainType::Leaf, MemberKind::Org) => Err(OrgError::MemberKindNotAllowed(
            "Leaf domain only accepts person members".to_string(),
        )),
    }
}

/// DAG 成环检查（org-genesis §3.3 禁止成环）：待加入组织已出现在目标域的
/// 可达祖先集中（含目标域本身）即成环。
///
/// 口径：纯逻辑 DFS——从 `target_org_id` 出发沿「上级域关系」向上遍历
/// （`parent_org_ids` 返回某组织已加入的上级域 id 列表；名册线形是
/// 域→成员，调用方由公开名册反查或自行维护反向索引）。若可达
/// `joiner_org_id`（含二者相等）则加入操作将成环，返回 `true`（调用方拒绝）。
/// 图无单父约束（一个组织可加入多个共同体，DFS  visited 去重即可）。
pub fn membership_would_cycle(
    joiner_org_id: &str,
    target_org_id: &str,
    parent_org_ids: &dyn Fn(&str) -> Vec<String>,
) -> bool {
    if joiner_org_id == target_org_id {
        return true;
    }
    let mut visited = std::collections::BTreeSet::new();
    let mut stack = vec![target_org_id.to_string()];
    while let Some(current) = stack.pop() {
        if !visited.insert(current.clone()) {
            continue;
        }
        for next in parent_org_ids(&current) {
            if next == joiner_org_id {
                return true;
            }
            if !visited.contains(&next) {
                stack.push(next);
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::org::types::is_valid_org_id;

    fn sample_genesis() -> (SigningKey, GenesisPolicyRecord) {
        let root = SigningKey::from_bytes(&[0x31; 32]);
        let mut record = GenesisPolicyRecord {
            genesis_v: 1,
            name: "阳光小区".to_string(),
            description: "阳光小区共同体域".to_string(),
            domain_type: DomainType::Community,
            root_public_key: B64.encode(root.verifying_key().to_bytes()),
            org_address: "a".repeat(55),
            signing_policy: SigningPolicy::AnyAdmin,
            transition: Some(TransitionDecl {
                kind: "delayed-veto".to_string(),
                delay_ms: 259_200_000,
                veto_threshold: VetoThreshold { count: 1 },
                when_members_exceed: 1,
            }),
            born_of: None,
            created_by: "ab".repeat(32),
            created_at: 1_720_000_000_000,
            sig: String::new(),
        };
        sign_genesis_record(&mut record, &root);
        (root, record)
    }

    #[test]
    fn genesis_id_self_certifying() {
        let (_root, record) = sample_genesis();
        assert!(verify_genesis_signature(&record));
        let org_id = genesis_org_id(&record).expect("org id");
        assert!(org_id.starts_with("org_"));
        assert_eq!(org_id.len(), 4 + 64);
        assert!(is_valid_org_id(&org_id));
        assert!(is_genesis_form_org_id(&org_id));
        // orgId 哈希段 == policyHash₀
        assert_eq!(&org_id[4..], genesis_policy_hash(&record).unwrap());
        // 篡改 name → orgId 变化且验签仍过（签的是原载荷，但哈希段不再匹配）
        let mut tampered = record.clone();
        tampered.name = "别的名字".to_string();
        assert_ne!(genesis_org_id(&tampered).unwrap(), org_id);
    }

    #[test]
    fn genesis_org_address_binding_recheck() {
        // 与向量 orgGenesis 组同 seed（0x31）：orgAddress 复算必须等于 §15 派生值
        let (root, mut record) = sample_genesis();
        record.org_address = org_address_from_public_key(&root.verifying_key().to_bytes());
        sign_genesis_record(&mut record, &root);
        assert!(verify_genesis_signature(&record));
        assert!(verify_org_address_binding(&record));

        // 地址与根公钥不匹配（换 key 的地址）→ 互绑断裂，必拒
        let other = SigningKey::from_bytes(&[0x99; 32]);
        let mut mismatched = record.clone();
        mismatched.org_address = org_address_from_public_key(&other.verifying_key().to_bytes());
        assert!(!verify_org_address_binding(&mismatched));

        // rootPublicKey 非合法 base64 32 字节 → 同属不匹配
        let mut bad_key = record.clone();
        bad_key.root_public_key = "!!not-base64!!".to_string();
        assert!(!verify_org_address_binding(&bad_key));
    }

    #[test]
    fn born_of_optional_roundtrip() {
        let (root, record) = sample_genesis();
        // 普通创建（bornOf 缺省）：序列化丢键，canonical 载荷不含 bornOf
        let json = serde_json::to_value(&record).unwrap();
        assert!(!json.as_object().unwrap().contains_key("bornOf"));
        assert!(!genesis_sign_payload(&record).unwrap().contains("bornOf"));
        // 缺省键反序列化回 None（既有无线形 bornOf 的记录原样解析）
        let parsed: GenesisPolicyRecord = serde_json::from_value(json).unwrap();
        assert_eq!(parsed, record);

        // 决议创设：bornOf 入线形、入载荷、入 orgId 承诺
        let mut birthed = record.clone();
        birthed.born_of = Some(BornOf {
            affair_id: "cd".repeat(32),
            resolution_id: "ef".repeat(32),
        });
        sign_genesis_record(&mut birthed, &root);
        let json = serde_json::to_value(&birthed).unwrap();
        assert_eq!(
            json["bornOf"],
            serde_json::json!({
                "affairId": "cd".repeat(32),
                "resolutionId": "ef".repeat(32),
            })
        );
        assert!(genesis_sign_payload(&birthed).unwrap().contains("bornOf"));
        assert!(verify_genesis_signature(&birthed));
        // bornOf 参与哈希承诺：同一创建参数带/不带出生证明产生不同 orgId
        assert_ne!(
            genesis_org_id(&birthed).unwrap(),
            genesis_org_id(&record).unwrap()
        );
        let parsed: GenesisPolicyRecord = serde_json::from_value(json).unwrap();
        assert_eq!(parsed, birthed);
    }

    #[test]
    fn dual_form_org_id_discrimination() {
        // legacy 形态：org_<16hex> 合法但非创世哈希型
        let legacy = format!("org_{}", "5e".repeat(8));
        assert!(is_valid_org_id(&legacy));
        assert!(!is_genesis_form_org_id(&legacy));
        // 创世哈希型：org_<64hex> 双判别均命中
        let (_root, record) = sample_genesis();
        let genesis_id = genesis_org_id(&record).unwrap();
        assert!(is_valid_org_id(&genesis_id));
        assert!(is_genesis_form_org_id(&genesis_id));
        // 两形态之外一律非法
        assert!(!is_valid_org_id("org_xyz"));
        assert!(!is_valid_org_id(&format!("org_{}", "5e".repeat(7))));
        assert!(!is_valid_org_id(&format!("org_{}", "5e".repeat(9))));
        assert!(!is_valid_org_id(&format!("org_{}", "5E".repeat(8))));
    }

    #[test]
    fn org_domain_identity_derives_distinct_per_domain() {
        let root = SigningKey::from_bytes(&[0x31; 32]);
        let community = OrgDomainIdentity::derive(&root, "community:org_abc");
        let affair = OrgDomainIdentity::derive(&root, "affair:def");
        assert_ne!(community.identity(), affair.identity());
        assert_eq!(community.identity().len(), 64);
    }

    #[test]
    fn member_kind_matrix() {
        assert!(enforce_member_kind(DomainType::Community, MemberKind::Org).is_ok());
        assert!(enforce_member_kind(DomainType::Leaf, MemberKind::Person).is_ok());
        assert!(enforce_member_kind(DomainType::Community, MemberKind::Person).is_err());
        assert!(enforce_member_kind(DomainType::Leaf, MemberKind::Org).is_err());
    }

    #[test]
    fn cycle_check_dfs() {
        // 上级域关系：C 已加入 B（C 的上级 = [B]）；B 已加入 A；A 无上级。
        // （名册线形为其反向：B 的名册列 C，A 的名册列 B。）
        let parents = |org: &str| -> Vec<String> {
            match org {
                "org_C" => vec!["org_B".to_string()],
                "org_B" => vec!["org_A".to_string()],
                _ => vec![],
            }
        };
        // 无环：D 加入 C（C 的祖先集 = {B, A}，不含 D）
        assert!(!membership_would_cycle("org_D", "org_C", &parents));
        // 自环：C 加入 C
        assert!(membership_would_cycle("org_C", "org_C", &parents));
        // 直接环：B 加入 C（C 已加入 B——B 在 C 的祖先集中）
        assert!(membership_would_cycle("org_B", "org_C", &parents));
        // 传递环：A 加入 C（C→B→A，A 在 C 的可达祖先集中）
        assert!(membership_would_cycle("org_A", "org_C", &parents));
        // 反向无环：C 加入 A（A 无上级，祖先集为空；C∈A 不产生环——
        // 名册反向为 A→B→C 的链，仍是 DAG）
        assert!(!membership_would_cycle("org_C", "org_A", &parents));
        // 多父：E 同时加入 B 与 A，B 加入 E 仍成环（图无单父约束）
        let parents_multi = |org: &str| -> Vec<String> {
            match org {
                "org_E" => vec!["org_B".to_string(), "org_A".to_string()],
                _ => parents(org),
            }
        };
        assert!(membership_would_cycle("org_B", "org_E", &parents_multi));
    }

    #[test]
    fn policy_revision_hash_chain() {
        let (_root, genesis) = sample_genesis();
        let policy_hash0 = genesis_policy_hash(&genesis).unwrap();
        let revision = PolicyRevisionRecord {
            policy_v: 1,
            org_id: genesis_org_id(&genesis).unwrap(),
            seq: 1,
            prev_policy_hash: policy_hash0.clone(),
            signing_policy: SigningPolicy::MOfN { m: 2, n: 3 },
            updated_at: genesis.created_at + 10_000,
            sig_set: serde_json::from_value(serde_json::json!({
                "sigSetV": 1, "orgId": "org_x", "subject": "ab".repeat(32),
                "policyHash": "cd".repeat(32),
                "roster": { "memberSetHash": "ef".repeat(32),
                    "anchor": { "orgId": "org_x", "anchorRoot": "12".repeat(32), "ts": 0 } },
                "signedAt": 0,
                "signatures": []
            }))
            .unwrap(),
        };
        assert_ne!(policy_revision_hash(&revision).unwrap(), policy_hash0);
        let payload = policy_revision_payload(&revision).unwrap();
        assert!(!payload.contains("sigSet"));
    }
}

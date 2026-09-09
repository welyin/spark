//! 凭证体系线形类型（wiki/protocol/community/credential.md §2–§5、read-gate.md §3）。
//!
//! 全部为协议线形的 serde 映射（camelCase），canonical/哈希/验签逻辑见各功能文件。
//! 约定：被签名剔除的字段（`sig` / `sigSet`）恒为结构体最后一个字段；
//! 缺省语义为「键缺失」的可选字段用 `skip_serializing_if` 省略（OrgSigSet 名册快照），
//! 缺省语义为 `null` 的字段（`linkRef` / `reason` / `prevHash`）保持 `Option` 直序列化。

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// 身份引用（issuer / 关联声明签发人）：identity 必须等于 sha256hex(公钥原始字节)。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IdentityRef {
    /// `^[0-9a-f]{64}$`。
    pub identity: String,
    /// base64（原始 32 字节公钥）。
    pub public_key: String,
}

/// 持有者种类（credential §2 holder.kind）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HolderKind {
    /// 个人。
    Person,
    /// 组织（在本域的域身份，典型形态）。
    Org,
}

/// 持有者引用。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HolderRef {
    /// 持有者种类。
    pub kind: HolderKind,
    /// `^[0-9a-f]{64}$`。
    pub identity: String,
    /// base64（原始 32 字节公钥）。
    pub public_key: String,
}

/// 资格凭证（credential §2）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Credential {
    /// 恒 1。
    pub cred_v: u32,
    /// 凭证类型（`^[A-Za-z0-9_-]+(:[A-Za-z0-9_-]+)*$`，≤64）。
    pub cred_type: String,
    /// 验证人（个人域身份）。
    pub issuer: IdentityRef,
    /// 持有者。
    pub holder: HolderRef,
    /// 对象域 orgId（双形态）。
    pub subject_domain: String,
    /// 结论字段（最小披露：禁止身份标识）。
    pub claims: Map<String, Value>,
    /// 核验方式标识（验证插件 id + 方法名）。
    pub method: String,
    /// 同人关联声明引用（linkId），缺省 null。
    pub link_ref: Option<String>,
    /// 签发者声明时刻（Unix 毫秒）。
    pub issued_at: i64,
    /// issuer 私钥对 `canonical(剔除 sig 的全部字段)` 的签名（base64 64B）。
    pub sig: String,
}

/// 注销条目（credential §3.1；每验证人一条独立 append-only 链）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RevocationEntry {
    /// 恒 1。
    pub rev_v: u32,
    /// 验证人 identity（链作用域）。
    pub issuer: String,
    /// 从 1 递增。
    pub seq: u64,
    /// 前一条目 entryHash，首条 null。
    pub prev_hash: Option<String>,
    /// 被注销的 credId。
    pub cred_id: String,
    /// 注销时刻（Unix 毫秒）。
    pub revoked_at: i64,
    /// 展示用原因（无协议语义）。
    pub reason: Option<String>,
    /// issuer 私钥对 `canonical(剔除 sig)` 的签名。
    pub sig: String,
}

/// 注销列表头承诺（credential §3.2「未注销」证明）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RevocationHead {
    /// 恒 1。
    pub rev_head_v: u32,
    /// 验证人 identity。
    pub issuer: String,
    /// 承诺覆盖到的最大 seq。
    pub head_seq: u64,
    /// seq == headSeq 条目的 entryHash。
    pub head_hash: String,
    /// 承诺时刻（Unix 毫秒）；新鲜度取舍归消费方。
    pub as_of: i64,
    /// issuer 私钥对 `canonical(剔除 sig)` 的签名。
    pub sig: String,
}

/// 验证人授权条目（credential §4 verifiers[]）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifierGrant {
    /// 验证人 identity。
    pub identity: String,
    /// base64（原始 32 字节公钥）。
    pub public_key: String,
    /// 授权凭证类型集。
    pub cred_types: Vec<String>,
    /// 授权方法模式集；尾部 `*` 为前缀通配（如 `plugin:hoa-verify:*`）。
    pub methods: Vec<String>,
}

/// 名册成员条目（org-signature §3：只含身份与角色两键）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RosterMember {
    /// 成员 identity。
    pub identity: String,
    /// `admin` | `member`。
    pub role: String,
    /// 成员 org_user_id（A16 双写过渡：`sha256hex(org-access 域公钥)`；`None` =
    /// 未发布 accessKey 的存量/旧版成员）。名册回查对 identity 与 orgUserId
    /// 双键兼容——双写期任何节点都能验 rootId / org_user_id 两种签名；
    /// memberSetHash 承诺随包内容自洽（携带即计入，旧包无此键不受影响）。
    #[serde(
        rename = "orgUserId",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub org_user_id: Option<String>,
}

/// 名册快照的存证锚引用（org-signature §2 roster.anchor）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RosterAnchor {
    /// 所属组织。
    pub org_id: String,
    /// 锚根（sync-evidence §6–§7）。
    pub anchor_root: String,
    /// 锚时刻（Unix 毫秒）。
    pub ts: i64,
}

/// 名册状态承诺（org-signature §2 roster）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RosterCommitment {
    /// 名册状态承诺哈希。
    pub member_set_hash: String,
    /// 存证锚引用。
    pub anchor: RosterAnchor,
    /// 可省：随包携带的名册快照全文；缺省时由验证方本地副本提供。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<Vec<RosterMember>>,
}

/// OrgSigSet 分量签名（org-signature §2 signatures[]）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComponentSignature {
    /// 签名者 identity（== sha256hex(publicKey)）。
    pub signer: String,
    /// base64（原始 32 字节公钥）。
    pub public_key: String,
    /// 分量签名（base64 64B）。
    pub sig: String,
}

/// 组织签名包（org-signature §2 线形）。
///
/// 本模块只定义线形与「subject 绑定」校验；五步验证链的密码学/策略求值实现
/// 归 C3（core/org），经 [`crate::credential::OrgSigSetVerifier`] 注入。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrgSigSet {
    /// 恒 1。
    pub sig_set_v: u32,
    /// 签名主体组织（双形态）。
    pub org_id: String,
    /// 被签对象哈希 = sha256hex(normalizeObject(被签对象))。
    pub subject: String,
    /// 签名时刻生效的策略文档哈希。
    pub policy_hash: String,
    /// 名册状态承诺。
    pub roster: RosterCommitment,
    /// 签名方声明时刻（Unix 毫秒）。
    pub signed_at: i64,
    /// 分量签名集合。
    pub signatures: Vec<ComponentSignature>,
}

/// 验证人信任声明（credential §4；存储键 `org:verifiers:{orgId}`，逐版 LWW）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrustDecl {
    /// 恒 1。
    pub trust_v: u32,
    /// 对象域组织（双形态）。
    pub org_id: String,
    /// 信任的验证人集。
    pub verifiers: Vec<VerifierGrant>,
    /// 生效时刻：之后的签发才按本版判定（既往不咎时间线）。
    pub effective_from: i64,
    /// 版本号（LWW 主键，大者胜）。
    pub seq: u64,
    /// 更新时刻（同 seq 时的 LWW 次序键：大者胜，仍并列则保留本地现状）。
    pub updated_at: i64,
    /// 组织签名包；subject 必须等于 `sha256hex(canonical(本记录剔除 sigSet))`。
    pub sig_set: OrgSigSet,
}

/// 同人关联声明成员（credential §5 members[]）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkMember {
    /// 持有者 identity。
    pub holder_identity: String,
    /// 关联凭证 credId。
    pub cred_id: String,
}

/// 同人关联声明（credential §5，opt-in）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SamePersonLink {
    /// 恒 1。
    pub link_v: u32,
    /// 恒 `same-person`。
    pub statement: String,
    /// 关联成员（≥2）。
    pub members: Vec<LinkMember>,
    /// 签发验证人。
    pub issuer: IdentityRef,
    /// 对象域 orgId。
    pub subject_domain: String,
    /// 签发时刻（Unix 毫秒）。
    pub issued_at: i64,
    /// issuer 私钥对 `canonical(剔除 sig)` 的签名。
    pub sig: String,
}

/// holderProof（read-gate §3）：证明持有 holder 私钥，载荷绑定本次请求防重放。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HolderProof {
    /// 所证明的 credId。
    pub cred_id: String,
    /// holder 私钥对 [`crate::credential::holder_proof_payload`] 的签名。
    pub sig: String,
}

/// orgq-req 的凭证呈现段（read-gate §3 readAuth）。
///
/// 本段随 orgq-req body 入 dm 信封既有签名面，自身不再独立签名。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadAuth {
    /// 恒 1。
    pub gate_v: u32,
    /// 呈现的凭证全文集。
    pub credentials: Vec<Credential>,
    /// 逐凭证的持有证明。
    pub holder_proofs: Vec<HolderProof>,
    /// 呈现时刻（Unix 毫秒）；±10 min 新鲜度门槛。
    pub presented_at: i64,
}

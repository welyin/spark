//! 免预录凭证入册的加入声明（A17，membership §4.5；字节级规格 org-join §8）。
//!
//! `org-join-request` = org-mail 明文载荷本体（既有邀请流通道复用，新载荷
//! 类型 append-only：旧端不解析即忽略）——申请人自签加入声明 + 附凭证，
//! 合入侧纯逻辑验证（[`adjudicate_join_request`]）后**直接入册**，零管理员
//! 在线（验证人的预先签名 + 组织准入策略声明替代管理员当场合签）。
//!
//! 双路径合一：预录条目存在 → 认领（授权来自预录本身，不消费 credential）；
//! 无预录 → 免预录（准入策略生效记录 + 类型/信任域匹配 + credential §6
//! 第 1–5 步验证链）。两路径经同一函数裁决，fail-closed。
//!
//! 纯逻辑层：存储装载（名册/策略/信任声明/注销快照）与入册落库归
//! [`crate::org::service`]，输入一律参数注入（now 注入，不读本地时钟）。

use serde::{Deserialize, Serialize};

use super::access_key::verify_access_key_binding;
use super::types::{OrganizationAccessKey, OrganizationNodeInfo, is_valid_org_id};
use crate::credential::{
    Credential, FRESHNESS_WINDOW_MS, IdentityRef, RevocationView, TrustDecl,
    verify_credential_chain,
};
use crate::evidence::normalize_object;
use crate::identity::verify_ed25519_signature;
use crate::policy::{AcceptPolicyRecord, accept_policy_admits};

/// org-mail 明文载荷类型标签（append-only 新类型）。
pub const ORG_JOIN_REQUEST_TYPE: &str = "org-join-request";
/// 加入声明版本（恒 1）。
pub const JOIN_REQUEST_V: u32 = 1;

/// 加入声明（org-join §8.1 线形；即 org-mail `org-join-request` 明文载荷）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JoinRequest {
    /// 恒 1。
    pub join_v: u32,
    /// 恒 `org-join-request`。
    #[serde(rename = "type")]
    pub type_: String,
    /// 目标组织（双形态）。
    pub org_id: String,
    /// 申请人根身份（identity == sha256hex(publicKey)，总约自包含绑定）。
    pub applicant: IdentityRef,
    /// 申请人自发布的 org-access 域身份（A16 线形）——入册条目直接携带，
    /// org_user_id 免二次发布。
    pub access_key: OrganizationAccessKey,
    /// 免预录路径必携；认领路径缺省 null（不消费）。
    pub credential: Option<Credential>,
    /// 申请人自报端点（可省；受理节点回传名册/后续寻址用）。
    #[serde(default)]
    pub node_info: Option<OrganizationNodeInfo>,
    /// 声明时刻（Unix 毫秒；±10 min 新鲜度门槛，总约）。
    pub declared_at: i64,
    /// 申请人根私钥对 `canonical(剔除 sig 的全部字段)` 的签名（base64 64B）。
    pub sig: String,
}

/// 加入声明签名载荷：`canonical(剔除 sig)`（community 总约统一口径）。
pub fn join_request_sign_payload(request: &JoinRequest) -> Result<String, JoinRejection> {
    let mut value = serde_json::to_value(request)
        .map_err(|_| JoinRejection::InvalidStructure("join request serialize"))?;
    if let Some(obj) = value.as_object_mut() {
        obj.remove("sig");
    }
    Ok(normalize_object(&value))
}

/// 结构校验（org-join §8.2 第 1 步）：字段常量/形状 + applicant 自包含绑定。
pub fn validate_join_request(request: &JoinRequest) -> Result<(), JoinRejection> {
    if request.join_v != JOIN_REQUEST_V {
        return Err(JoinRejection::InvalidStructure("joinV must be 1"));
    }
    if request.type_ != ORG_JOIN_REQUEST_TYPE {
        return Err(JoinRejection::InvalidStructure("type must be org-join-request"));
    }
    if !is_valid_org_id(&request.org_id) {
        return Err(JoinRejection::InvalidStructure("orgId dual-form"));
    }
    if !crate::credential::identity_matches_public_key(
        &request.applicant.identity,
        &request.applicant.public_key,
    ) {
        return Err(JoinRejection::IdentityMismatch);
    }
    Ok(())
}

/// 合入裁决拒绝原因（kind 逐字稳定：golden vectors 与跨层上报按此对齐；
/// 不派生 Clone/PartialEq——CredentialError 未派生，比对一律走 kind()）。
#[derive(Debug, thiserror::Error)]
pub enum JoinRejection {
    /// 字段常量/形状/目标组织不符。
    #[error("invalid join request structure: {0}")]
    InvalidStructure(&'static str),
    /// 身份绑定失败（applicant identity/publicKey、accessKey 验绑、凭证
    /// holder 与申请人不符）。
    #[error("join identity mismatch")]
    IdentityMismatch,
    /// 声明签名验签失败。
    #[error("join signature invalid")]
    InvalidSignature,
    /// declaredAt 超出 ±10 min 新鲜度窗口。
    #[error("join request stale")]
    StaleTimestamp,
    /// 无预录条目且未附凭证（免预录路径凭证必携）。
    #[error("credential required for unlisted join")]
    CredentialRequired,
    /// 组织无（生效的）准入策略声明——不接受免预录入册。
    #[error("accept policy missing or not effective")]
    AcceptPolicyMissing,
    /// 凭证类型/签发者信任域不在准入策略声明内。
    #[error("credential type not accepted")]
    CredTypeNotAccepted,
    /// 凭证验证链失败（结构/验签/签发者不受信任/已注销/注销快照缺失）。
    #[error("{0}")]
    Credential(#[from] crate::credential::CredentialError),
}

impl JoinRejection {
    /// 稳定错误名（vectors `expect` 与跨层上报口径）。
    pub fn kind(&self) -> &'static str {
        match self {
            Self::InvalidStructure(_) => "invalid-structure",
            Self::IdentityMismatch => "identity-mismatch",
            Self::InvalidSignature => "invalid-signature",
            Self::StaleTimestamp => "stale-timestamp",
            Self::CredentialRequired => "credential-required",
            Self::AcceptPolicyMissing => "accept-policy-missing",
            Self::CredTypeNotAccepted => "cred-type-not-accepted",
            Self::Credential(e) => e.kind(),
        }
    }
}

/// 入册路径（双路径合一裁决的输出标记）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JoinPath {
    /// 预录-认领：名册已有申请人条目（授权来自管理员预录）。
    Claim,
    /// 免预录：无预录条目，凭有效凭证 + 准入策略直接入册。
    Credential,
}

impl JoinPath {
    /// 线形/审计口径名。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Claim => "claim",
            Self::Credential => "credential",
        }
    }
}

/// 受理裁决（入册所需的最小事实）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JoinAdmission {
    /// 入册路径。
    pub path: JoinPath,
    /// 免预录路径的 credId（事务审计引用；认领路径为 None）。
    pub cred_id: Option<String>,
}

/// 合入侧双路径验证（org-join §8.2，fail-closed）：
/// 结构 → 目标组织匹配 → 声明签名 → 新鲜度 → accessKey 验绑 → 按名册状态
/// 分派（预录-认领 / 免预录凭证链）。
///
/// - `org_id`：受理组织（request.orgId 必须等于它，防跨组织转投）；
/// - `pre_registered`：名册是否已有申请人条目（调用方读装配视图名册）；
/// - `accept_policy`：**已生效**的准入策略现行记录（调用方按
///   [`crate::policy::effective_accept_policy`] 门控后传入）；
/// - `trust_decls`：凭证 subjectDomain 的全部已知信任声明版本（既往不咎
///   时间线；函数内按 subjectDomain 预筛）；
/// - `revocation`：签发者的注销证明（头承诺 + 全量条目）；缺数据即失败。
pub fn adjudicate_join_request(
    org_id: &str,
    request: &JoinRequest,
    pre_registered: bool,
    accept_policy: Option<&AcceptPolicyRecord>,
    trust_decls: &[&TrustDecl],
    revocation: Option<&RevocationView>,
    now_ms: i64,
) -> Result<JoinAdmission, JoinRejection> {
    validate_join_request(request)?;
    if request.org_id != org_id {
        return Err(JoinRejection::InvalidStructure("orgId mismatch"));
    }
    let payload = join_request_sign_payload(request)?;
    if !verify_ed25519_signature(&payload, &request.sig, &request.applicant.public_key) {
        return Err(JoinRejection::InvalidSignature);
    }
    if (request.declared_at - now_ms).abs() > FRESHNESS_WINDOW_MS {
        return Err(JoinRejection::StaleTimestamp);
    }
    // accessKey 验绑（A16 双步）：锚定申请人 + 绑定签名——入册条目直接携带
    // 验过的 accessKey，org_user_id 免二次发布。
    if !verify_access_key_binding(org_id, &request.applicant.identity, &request.access_key) {
        return Err(JoinRejection::IdentityMismatch);
    }

    // 路径分派：预录条目存在 → 认领（授权来自预录本身，不消费 credential）。
    if pre_registered {
        return Ok(JoinAdmission {
            path: JoinPath::Claim,
            cred_id: None,
        });
    }

    // 免预录路径：凭证必携且持有人 = 申请人（kind person）。
    let cred = request.credential.as_ref().ok_or(JoinRejection::CredentialRequired)?;
    if cred.holder.kind != crate::credential::HolderKind::Person
        || cred.holder.identity != request.applicant.identity
        || cred.holder.public_key != request.applicant.public_key
    {
        return Err(JoinRejection::IdentityMismatch);
    }
    // 类型匹配 + 签发者信任域匹配（准入策略声明内）。
    let policy = accept_policy.ok_or(JoinRejection::AcceptPolicyMissing)?;
    if !accept_policy_admits(policy, &cred.cred_type, &cred.subject_domain) {
        return Err(JoinRejection::CredTypeNotAccepted);
    }
    // credential §6 第 1–5 步验证链（结构/验签/签发者信任（既往不咎）/注销）。
    let domain_decls: Vec<&TrustDecl> = trust_decls
        .iter()
        .copied()
        .filter(|d| d.org_id == cred.subject_domain)
        .collect();
    let cred_id = verify_credential_chain(cred, &domain_decls, revocation, now_ms)?;
    Ok(JoinAdmission {
        path: JoinPath::Credential,
        cred_id: Some(cred_id),
    })
}

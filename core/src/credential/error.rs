//! 凭证模块错误（credential §6：任一失败即无效，fail-closed）。
//!
//! `kind()` 返回稳定的机器可读错误名——golden vectors 的 `expect` 字段与
//! 插件桥（跨命令边界）按此对齐，逐字稳定（代码规范 §2.3 用户可见文案纪律）。

/// 凭证验证/合入错误。
#[derive(Debug, thiserror::Error)]
pub enum CredentialError {
    /// 字段类型/形状/常量值不符（含 credType 正则、orgId 双形态、版本号）。
    #[error("invalid structure: {0}")]
    InvalidStructure(&'static str),
    /// identity != sha256hex(publicKey)。
    #[error("identity does not match public key")]
    IdentityMismatch,
    /// credId 复算值与引用值不符。
    #[error("credId mismatch")]
    CredIdMismatch,
    /// Ed25519 验签失败。
    #[error("invalid signature")]
    InvalidSignature,
    /// 声明时刻超出 ±10 min 新鲜度窗口。
    #[error("timestamp outside freshness window")]
    StaleTimestamp,
    /// issuer 不在 subjectDomain 于签发时刻的信任集内（或 credType/method 超范围）。
    #[error("issuer not trusted at issuance time")]
    IssuerNotTrusted,
    /// credId 出现在 issuer 注销列表中。
    #[error("credential revoked")]
    Revoked,
    /// 注销链断链（seq/prevHash/签名/作用域）。
    #[error("revocation chain broken: {0}")]
    RevocationChainBroken(&'static str),
    /// 头承诺与链不符或验签失败。
    #[error("revocation head invalid")]
    RevocationHeadInvalid,
    /// 注销数据缺失（fail-closed：无头承诺不可证「未注销」）。
    #[error("revocation data unavailable")]
    RevocationUnavailable,
    /// 同人关联声明校验失败。
    #[error("invalid same-person link: {0}")]
    InvalidLink(&'static str),
    /// trustDecl 的 sigSet 未通过组织签名验证（验签失败保留本地现状）。
    #[error("trustDecl sigSet rejected")]
    SigSetRejected,
    /// sigSet.subject 不等于 trustDecl 剔除 sigSet 的哈希（跨包搬签防护）。
    #[error("sigSet subject mismatch")]
    SigSetSubjectMismatch,
    /// 凭证类型不在 readPolicy.credTypes 内。
    #[error("credential type not allowed by read policy")]
    CredTypeNotAllowed,
    /// 凭证 subjectDomain 与 readPolicy.verifierDomain 不符。
    #[error("subject domain mismatch")]
    SubjectDomainMismatch,
    /// holderProof 验签失败或与本次请求绑定不符。
    #[error("holder proof invalid")]
    HolderProofInvalid,
    /// 缺少某呈现凭证的 holderProof（或存在无凭证对应的孤儿 proof）。
    #[error("holder proof missing")]
    HolderProofMissing,
    /// 同一 credId 在一次 readAuth 中重复呈现。
    #[error("duplicate credential presentation")]
    DuplicateCredential,
    /// 城门名册回查失败（A15）：持有者当时不是凭证 subjectDomain 的成员
    /// （退队即失效，零密钥轮换）；名册数据不可用同样 fail-closed 归本类。
    #[error("holder not a member of credential subject domain")]
    NotSubjectDomainMember,
    /// JSON 序列化/反序列化错误。
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

impl CredentialError {
    /// 稳定错误名（vectors `expect` 与跨层上报口径）。
    pub fn kind(&self) -> &'static str {
        match self {
            Self::InvalidStructure(_) => "invalid-structure",
            Self::IdentityMismatch => "identity-mismatch",
            Self::CredIdMismatch => "cred-id-mismatch",
            Self::InvalidSignature => "invalid-signature",
            Self::StaleTimestamp => "stale-timestamp",
            Self::IssuerNotTrusted => "issuer-not-trusted",
            Self::Revoked => "revoked",
            Self::RevocationChainBroken(_) => "revocation-chain-broken",
            Self::RevocationHeadInvalid => "revocation-head-invalid",
            Self::RevocationUnavailable => "revocation-unavailable",
            Self::InvalidLink(_) => "invalid-link",
            Self::SigSetRejected => "sig-set-rejected",
            Self::SigSetSubjectMismatch => "sig-set-subject-mismatch",
            Self::CredTypeNotAllowed => "cred-type-not-allowed",
            Self::SubjectDomainMismatch => "subject-domain-mismatch",
            Self::HolderProofInvalid => "holder-proof-invalid",
            Self::HolderProofMissing => "holder-proof-missing",
            Self::DuplicateCredential => "duplicate-credential",
            Self::NotSubjectDomainMember => "not-subject-domain-member",
            Self::Json(_) => "json-error",
        }
    }
}

/// 凭证模块 Result 别名。
pub type Result<T> = std::result::Result<T, CredentialError>;

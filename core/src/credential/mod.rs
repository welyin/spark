//! 凭证模块（community-affairs C2）：资格凭证、注销列表、验证人信任声明、
//! 同人关联声明、读授权 holderProof——纯逻辑层，不碰网络与运行时。
//!
//! 字节级权威规格：`wiki/protocol/community/credential.md`（凭证/注销/信任声明/
//! 关联声明/§6 验证链）与 `read-gate.md` §3–§4（holderProof 与 readAuth 段验证）；
//! canonical/签名/id 编码约定见 `wiki/protocol/community/README.md` 总约。
//! 验收向量：`code/spec/vectors/community.json`（credential.* / trustDecl /
//! samePersonLink / readGate.* 组），消费测试 `core/tests/community_credential_vectors.rs`。
//!
//! 边界（credential §1）：签发流程不在内核（验证插件是方法，验证人才是信任）；
//! OrgSigSet 五步验证链实现归 C3，本模块经 [`OrgSigSetVerifier`] 注入。

pub mod credential;
pub mod error;
pub mod link;
pub mod read_gate;
pub mod revocation;
pub mod trust;
pub mod types;

pub use credential::{
    CRED_HELD_PREFIX, FRESHNESS_WINDOW_MS, RevocationView, credential_id, credential_sign_payload,
    held_credential_key, validate_credential_issuance, validate_credential_structure,
    verify_credential_chain, verify_credential_signature, verify_credential_static,
};
pub use error::{CredentialError, Result};
pub use link::{
    LINK_STATEMENT, link_id, link_sign_payload, validate_link_issuance, verify_link_membership,
    verify_same_person_link,
};
pub use read_gate::{
    CRED_REV_PREFIX, CredentialReadPolicy, RevocationSnapshot, build_read_auth,
    holder_proof_payload, revocation_snapshot_key, select_presentable_credentials,
    verify_holder_proof, verify_read_auth,
};
pub use revocation::{
    revocation_entry_hash, revocation_head_payload, revocation_sign_payload, verify_not_revoked,
    verify_revocation_chain, verify_revocation_head,
};
pub use trust::{
    MergeVerdict, OrgSigSetVerifier, TRUST_DECL_PREFIX, issuer_trusted_at, merge_trust_decl,
    trust_decl_at, trust_decl_hash, trust_decl_key, validate_trust_decl_structure,
    verifier_granted,
};
pub use types::{
    ComponentSignature, Credential, HolderKind, HolderProof, HolderRef, IdentityRef, LinkMember,
    OrgSigSet, ReadAuth, RevocationEntry, RevocationHead, RosterAnchor, RosterCommitment,
    RosterMember, SamePersonLink, TrustDecl, VerifierGrant,
};

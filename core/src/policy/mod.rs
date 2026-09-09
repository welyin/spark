//! 策略引擎 B1（community-affairs C5）：最小声明式规则集——纯逻辑层，不碰
//! 网络与运行时；求值输入（策略文档、凭证呈现摘要、请求上下文）全部参数注入。
//!
//! 字节级权威规格：`wiki/protocol/community/policy.md`（文档线形 §2、受众 §3、
//! 求值 §4、静态分析 §5）；升级路径：schema 预留 `engine` 字段，当前唯一合法值
//! `"b1"`，非 b1 引擎文档 fail-closed 拒绝（B2 Cedar 换求值器、文档形态不变）。
//! 产品语义：`wiki/product/community-model.md` §九（名册三档、字段级控制、
//! 向上开放矩阵）；消费方：`wiki/protocol/community/read-gate.md` §4 第 5 步
//! （`policyRef` 求值）。
//! 验收向量：`code/spec/vectors/community.json`（`readGate.policyRef` 组），
//! 消费测试 `core/tests/community_policy_vectors.rs`。
//!
//! 边界（policy §1）：与 org-genesis §5 的组织签名策略文档（`signingPolicy`
//! 修订链，归 C3）是两套文档、两个哈希槽位，互不替代；本模块只做读授权/
//! 可见性策略。静态分析（analyze.rs）与求值（eval.rs）相互独立。
//! A15：名册开放声明（disclosure.rs，policy §8 `org:disclosure:` 记录 +
//! 公示延迟 + `eval_disclosure` 求值）同模块；orgq 读取点求值口径已切换
//! 为开放声明（membership §五.2），B1 `evaluate_read` 保留（引擎重排归 A30）。
//! 验收向量增 `policy.disclosure` 组（消费测试 `core/tests/policy_disclosure_vectors.rs`）。
//! A17：准入策略声明（accept.rs，policy §9 `org:accept:` 记录 + 公示延迟 +
//! 生效门控）同模块；验收向量增 `policy.acceptCredentials` / `joinRequest.merge`
//! 组（消费测试 `core/tests/accept_vectors.rs`）。

mod accept;
mod analyze;
mod disclosure;
mod doc;
mod error;
mod eval;

pub use accept::{
    ACCEPT_POLICY_PREFIX, ACCEPT_POLICY_PUB_PERIOD_MS, ACCEPT_POLICY_V, AcceptCredentialRule,
    AcceptPolicyRecord, accept_policy_admits, accept_policy_hash, accept_policy_key,
    accept_policy_widening, effective_accept_policy, validate_accept_policy,
};
pub use analyze::{AnalysisFinding, Severity, analyze};
pub use disclosure::{
    DISCLOSURE_PREFIX, DISCLOSURE_PUB_PERIOD_MS, DISCLOSURE_V, DisclosureRecord, DisclosureView,
    disclosure_hash, disclosure_key, disclosure_widening, eval_disclosure, validate_disclosure,
};
pub use doc::{
    Audience, ENGINE_B1, FieldRule, POLICY_DOC_V, PolicyDoc, RosterRules, RosterTier, UpwardEntry,
    policy_doc_hash, validate_policy_doc,
};
pub use error::{PolicyError, Result};
pub use eval::{
    DenyReason, PresentedCredential, ReadRequest, ReadVerdict, RequesterContext, eval_rules,
    evaluate_read, visible_fields,
};

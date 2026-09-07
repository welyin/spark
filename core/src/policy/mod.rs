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

mod analyze;
mod doc;
mod error;
mod eval;

pub use analyze::{AnalysisFinding, Severity, analyze};
pub use doc::{
    Audience, ENGINE_B1, FieldRule, POLICY_DOC_V, PolicyDoc, RosterRules, RosterTier, UpwardEntry,
    policy_doc_hash, validate_policy_doc,
};
pub use error::{PolicyError, Result};
pub use eval::{
    DenyReason, PresentedCredential, ReadRequest, ReadVerdict, RequesterContext, eval_rules,
    evaluate_read, visible_fields,
};

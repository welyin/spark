//! 策略模块错误（policy §4/§5：任一失败即拒绝，fail-closed）。
//!
//! `kind()` 返回稳定的机器可读错误名——golden vectors 的 `expect` 字段与
//! 跨层上报按此对齐，逐字稳定（代码规范 §2.3 用户可见文案纪律）。

/// 策略文档校验/求值错误。
#[derive(Debug, thiserror::Error)]
pub enum PolicyError {
    /// 字段类型/形状/常量值不符（policyV、orgId 双形态、字段名/集合名形状）。
    #[error("invalid structure: {0}")]
    InvalidStructure(&'static str),
    /// engine 非 "b1"：B1 求值器无法处理（B2 Cedar 升级路径的出口）。
    #[error("unsupported policy engine: {0}")]
    UnsupportedEngine(String),
    /// policyRef 复算值与策略文档哈希不符（文档被篡改或引用错位）。
    #[error("policyRef does not match policy document hash")]
    PolicyRefMismatch,
    /// JSON 序列化/反序列化错误。
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

impl PolicyError {
    /// 稳定错误名（vectors `expect` 与跨层上报口径）。
    pub fn kind(&self) -> &'static str {
        match self {
            Self::InvalidStructure(_) => "invalid-structure",
            Self::UnsupportedEngine(_) => "unsupported-engine",
            Self::PolicyRefMismatch => "policy-ref-mismatch",
            Self::Json(_) => "json-error",
        }
    }
}

/// 策略模块 Result 别名。
pub type Result<T> = std::result::Result<T, PolicyError>;

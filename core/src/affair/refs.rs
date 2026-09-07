//! 事务间引用（wiki/protocol/community/affair.md §10）。
//!
//! 统一一种机制：目标事务 ID + 关系类型。引用 append-only 不可撤销；
//! 自指禁令：`target == 本事务 affairId` 拒绝。自指校验需要已知本事务 id，
//! 创世记录在 affairId 复算后补查（genesis.rs），`ref` 操作在解析时携带。

use serde_json::Value;

use super::actor::is_valid_identity_id;

/// 引用关系枚举（§10 表）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefRel {
    /// 继承（换代/分叉继承）。
    Inherit,
    /// 申诉（对决议的异议 = 引用被争议事务的新事务）。
    Appeal,
    /// 父子分解（只做引用与展示，不做状态耦合）。
    Parent,
    /// 关联（导航）。
    Related,
}

impl RefRel {
    /// 线形字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Inherit => "inherit",
            Self::Appeal => "appeal",
            Self::Parent => "parent",
            Self::Related => "related",
        }
    }
}

/// 引用条目。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AffairRef {
    /// 目标事务 affairId（64 hex）。
    pub target: String,
    /// 关系类型。
    pub rel: RefRel,
}

/// 引用校验失败原因（reason 字符串稳定）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefReject {
    /// 字段缺失或类型错误。
    Malformed,
    /// rel 非 §10 枚举。
    UnknownRel,
    /// target 不是 64 hex。
    BadTarget,
    /// 自指（target == 本事务 affairId）。
    SelfReference,
}

impl RefReject {
    /// 稳定 reason 字符串。
    pub fn reason(self) -> &'static str {
        match self {
            Self::Malformed => "malformed-ref",
            Self::UnknownRel => "unknown-ref-rel",
            Self::BadTarget => "bad-ref-target",
            Self::SelfReference => "self-reference",
        }
    }
}

/// 解析 rel 字符串（未知枚举 fail-closed）。
pub fn parse_ref_rel(s: &str) -> Option<RefRel> {
    match s {
        "inherit" => Some(RefRel::Inherit),
        "appeal" => Some(RefRel::Appeal),
        "parent" => Some(RefRel::Parent),
        "related" => Some(RefRel::Related),
        _ => None,
    }
}

/// 解析并校验一条引用。`self_affair_id` 已知时执行自指禁令。
pub fn parse_ref(value: &Value, self_affair_id: Option<&str>) -> Result<AffairRef, RefReject> {
    let obj = value.as_object().ok_or(RefReject::Malformed)?;
    let target = obj
        .get("target")
        .and_then(Value::as_str)
        .ok_or(RefReject::Malformed)?;
    if !is_valid_identity_id(target) {
        return Err(RefReject::BadTarget);
    }
    if self_affair_id == Some(target) {
        return Err(RefReject::SelfReference);
    }
    let rel = obj
        .get("rel")
        .and_then(Value::as_str)
        .and_then(parse_ref_rel)
        .ok_or(RefReject::UnknownRel)?;
    Ok(AffairRef {
        target: target.to_string(),
        rel,
    })
}

/// 解析引用数组（创世 `refs` 用，可空数组；缺省 = 空）。
pub fn parse_ref_array(
    value: Option<&Value>,
    self_affair_id: Option<&str>,
) -> Result<Vec<AffairRef>, RefReject> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let items = value.as_array().ok_or(RefReject::Malformed)?;
    items
        .iter()
        .map(|item| parse_ref(item, self_affair_id))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rel_roundtrip_and_self_ban() {
        for s in ["inherit", "appeal", "parent", "related"] {
            assert_eq!(parse_ref_rel(s).unwrap().as_str(), s);
        }
        assert_eq!(parse_ref_rel("child"), None);

        let target = "cd".repeat(32);
        let ok = parse_ref(&json!({"target": target, "rel": "inherit"}), None).unwrap();
        assert_eq!(ok.rel, RefRel::Inherit);
        assert_eq!(
            parse_ref(&json!({"target": target, "rel": "inherit"}), Some(&target)),
            Err(RefReject::SelfReference)
        );
        assert_eq!(
            parse_ref(&json!({"target": target, "rel": "fork"}), None),
            Err(RefReject::UnknownRel)
        );
    }
}

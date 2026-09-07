//! 元数据公告线形（wiki/protocol/community/affair-metadata.md §3）：payload
//! 结构解析、校验与构造。公告是「当前生效元数据」的全量快照，`basisOpHash`
//! 锚定修订链（§4 可验证性）。纯逻辑：不碰网络与存储，时间由调用方注入。

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::affair::{MetaGeneration, is_valid_identity_id};

/// metaV 恒 1（§3）。
pub const META_V: u64 = 1;
/// 单条公告序列化 ≤ 4 KB（§5 体积卫生）。
pub const ANNOUNCE_MAX_BYTES: usize = 4 * 1024;

// 元数据上界与 affair §2.1 同口径（affair/meta.rs 解析期同款约束）。
const TITLE_MAX_UTF16: usize = 120;
const SUMMARY_MAX_UTF16: usize = 1024;
const TAG_MAX_UTF16: usize = 32;
const TAGS_MAX: usize = 16;
const BASIS_OP_HASH_MAX: usize = 64;
const CONTENT_REF_MAX: usize = 512;
const CONTENT_HINT_MAX: usize = 128;
const CONTENTS_MAX: usize = 8;

/// 内容面描述性指针（§3 contents：指针只做导航，做种归内容面）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentPtr {
    pub kind: String,
    #[serde(rename = "ref")]
    pub ref_: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

/// 元数据公告（§3 payload；serde 线形 camelCase，region/contents 可省）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MetaAnnounce {
    pub affair_id: String,
    pub title: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    pub meta_seq: u64,
    pub basis_op_hash: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contents: Vec<ContentPtr>,
    pub updated_at: i64,
}

/// 从 `tags` 提取规范化区域代码（§3 region：tags 中首个 `region:` 前缀项的
/// 冗余槽位；无则 None）。
pub fn extract_region(tags: &[String]) -> Option<String> {
    tags.iter()
        .find_map(|tag| tag.strip_prefix("region:").map(ToString::to_string))
}

/// 由最新生效代际构造公告（发布侧；updatedAt 为发布方毫秒，调用方注入）。
pub fn announce_from_generation(
    affair_id: &str,
    generation: &MetaGeneration,
    updated_at: i64,
) -> MetaAnnounce {
    MetaAnnounce {
        affair_id: affair_id.to_string(),
        title: generation.meta.title.clone(),
        summary: generation.meta.summary.clone(),
        tags: generation.meta.tags.clone(),
        region: extract_region(&generation.meta.tags),
        meta_seq: generation.meta_seq,
        basis_op_hash: generation.basis_op_hash.clone(),
        contents: Vec::new(),
        updated_at,
    }
}

fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// 解析并校验公告 payload（§3 字段约束 + §5 体积卫生）。任一不符返回
/// `bad-meta-announce`（C4 入站已做形状门控，此处为收录点的完整校验）。
pub fn parse_announce(payload: &Value) -> Result<MetaAnnounce, &'static str> {
    if payload.to_string().len() > ANNOUNCE_MAX_BYTES {
        return Err("bad-meta-announce");
    }
    let obj = payload.as_object().ok_or("bad-meta-announce")?;
    if obj.get("metaV").and_then(Value::as_u64) != Some(META_V) {
        return Err("bad-meta-announce");
    }
    let affair_id = obj
        .get("affairId")
        .and_then(Value::as_str)
        .ok_or("bad-meta-announce")?;
    if !is_valid_identity_id(affair_id) {
        return Err("bad-meta-announce");
    }
    let title = obj
        .get("title")
        .and_then(Value::as_str)
        .ok_or("bad-meta-announce")?;
    if !(1..=TITLE_MAX_UTF16).contains(&utf16_len(title.trim())) {
        return Err("bad-meta-announce");
    }
    let summary = obj
        .get("summary")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if utf16_len(summary) > SUMMARY_MAX_UTF16 {
        return Err("bad-meta-announce");
    }
    let mut tags = Vec::new();
    if let Some(raw) = obj.get("tags") {
        let arr = raw.as_array().ok_or("bad-meta-announce")?;
        if arr.len() > TAGS_MAX {
            return Err("bad-meta-announce");
        }
        for tag in arr {
            let tag = tag.as_str().ok_or("bad-meta-announce")?;
            if utf16_len(tag) > TAG_MAX_UTF16 {
                return Err("bad-meta-announce");
            }
            tags.push(tag.to_string());
        }
    }
    let region = match obj.get("region") {
        // region 是 tags 中首个 `region:` 前缀项的冗余槽位（§3）：字段缺席时
        // 从 tags 派生，否则带 region 过滤的查询对合法公告会漏命中。
        None | Some(Value::Null) => extract_region(&tags),
        Some(v) => Some(v.as_str().ok_or("bad-meta-announce")?.to_string()),
    };
    let meta_seq = obj
        .get("metaSeq")
        .and_then(Value::as_u64)
        .ok_or("bad-meta-announce")?;
    let basis_op_hash = obj
        .get("basisOpHash")
        .and_then(Value::as_str)
        .ok_or("bad-meta-announce")?;
    if basis_op_hash.is_empty() || basis_op_hash.len() > BASIS_OP_HASH_MAX {
        return Err("bad-meta-announce");
    }
    let mut contents = Vec::new();
    if let Some(raw) = obj.get("contents") {
        let arr = raw.as_array().ok_or("bad-meta-announce")?;
        if arr.len() > CONTENTS_MAX {
            return Err("bad-meta-announce");
        }
        for item in arr {
            let ptr: ContentPtr =
                serde_json::from_value(item.clone()).map_err(|_| "bad-meta-announce")?;
            if !matches!(ptr.kind.as_str(), "git" | "blob")
                || ptr.ref_.is_empty()
                || ptr.ref_.len() > CONTENT_REF_MAX
                || ptr.hint.as_deref().map(utf16_len).unwrap_or(0) > CONTENT_HINT_MAX
            {
                return Err("bad-meta-announce");
            }
            contents.push(ptr);
        }
    }
    let updated_at = obj
        .get("updatedAt")
        .and_then(Value::as_i64)
        .ok_or("bad-meta-announce")?;
    if updated_at <= 0 {
        return Err("bad-meta-announce");
    }
    Ok(MetaAnnounce {
        affair_id: affair_id.to_string(),
        title: title.to_string(),
        summary: summary.to_string(),
        tags,
        region,
        meta_seq,
        basis_op_hash: basis_op_hash.to_string(),
        contents,
        updated_at,
    })
}

/// 公告 payload 线形（固定键序：metaV → … → updatedAt；region/contents
/// 缺席即省略，与 §3 示例一致）。
pub fn announce_to_value(announce: &MetaAnnounce) -> Value {
    let mut map = Map::new();
    map.insert("metaV".to_string(), Value::Number(META_V.into()));
    map.insert(
        "affairId".to_string(),
        Value::String(announce.affair_id.clone()),
    );
    map.insert("title".to_string(), Value::String(announce.title.clone()));
    map.insert(
        "summary".to_string(),
        Value::String(announce.summary.clone()),
    );
    map.insert(
        "tags".to_string(),
        serde_json::to_value(&announce.tags).expect("tags serialize"),
    );
    if let Some(region) = &announce.region {
        map.insert("region".to_string(), Value::String(region.clone()));
    }
    map.insert(
        "metaSeq".to_string(),
        Value::Number(announce.meta_seq.into()),
    );
    map.insert(
        "basisOpHash".to_string(),
        Value::String(announce.basis_op_hash.clone()),
    );
    if !announce.contents.is_empty() {
        map.insert(
            "contents".to_string(),
            serde_json::to_value(&announce.contents).expect("contents serialize"),
        );
    }
    map.insert(
        "updatedAt".to_string(),
        Value::Number(announce.updated_at.into()),
    );
    Value::Object(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_payload() -> Value {
        serde_json::json!({
            "metaV": 1,
            "affairId": "a".repeat(64),
            "title": "第二届业委会选举",
            "summary": "候选人提名与表决",
            "tags": ["region:110105", "hoa"],
            "region": "110105",
            "metaSeq": 2,
            "basisOpHash": "b".repeat(64),
            "updatedAt": 1720000000000i64,
        })
    }

    #[test]
    fn parse_roundtrip_keeps_fields() {
        let announce = parse_announce(&sample_payload()).unwrap();
        assert_eq!(announce.meta_seq, 2);
        assert_eq!(announce.region.as_deref(), Some("110105"));
        assert_eq!(announce.tags, vec!["region:110105", "hoa"]);
        let value = announce_to_value(&announce);
        let again = parse_announce(&value).unwrap();
        assert_eq!(announce, again);
    }

    #[test]
    fn parse_rejects_bad_shape() {
        let mut bad_version = sample_payload();
        bad_version["metaV"] = 2.into();
        assert!(parse_announce(&bad_version).is_err());

        let mut bad_id = sample_payload();
        bad_id["affairId"] = "not-hex!".into();
        assert!(parse_announce(&bad_id).is_err());

        let mut empty_title = sample_payload();
        empty_title["title"] = "   ".into();
        assert!(parse_announce(&empty_title).is_err());

        let mut bad_seq = sample_payload();
        bad_seq["metaSeq"] = (-1).into();
        assert!(parse_announce(&bad_seq).is_err());

        let mut bad_content = sample_payload();
        bad_content["contents"] = serde_json::json!([{ "kind": "ftp", "ref": "x" }]);
        assert!(parse_announce(&bad_content).is_err());
    }

    #[test]
    fn extract_region_takes_first_region_tag() {
        assert_eq!(
            extract_region(&["hoa".to_string(), "region:110105".to_string()]),
            Some("110105".to_string())
        );
        assert_eq!(extract_region(&["hoa".to_string()]), None);
    }
}

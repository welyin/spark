//! 与公告指针线形的衔接点：公告/元数据 payload 中的 blob 引用 → CID。
//!
//! 现状的「公告指针线形」（`p2p::plugin_announce`）：公告是带签名 + PoW 的
//! 指针消息，payload 自描述业务内容。内容面引用沿用 feed/pdsync 的既定约定
//! ——payload 中以 `{"$blob": "<cid>", ...}` 对象标记 blob 引用
//! （`plugindata::blob::BLOB_REF_FIELD`，social-feed §7 同口径）。
//!
//! 完整链路（本阶段落地到 provider 检索为止）：
//!
//! ```text
//! 公告 payload ──extract_blob_cids──▶ CID 列表
//!      │                                │
//!      │                     持有方：save_blob + pin_root + provide_blob
//!      │                                │（Kad provider 声明，持有即做种）
//!      │                                ▼
//!      └──── 消费方：find_blob_providers ──▶ provider peerId 集合
//!                                           │
//!                                           ▼
//!                            按 CID 拉取本体（传输协议：后续里程碑）
//! ```
//!
//! 即：公告负责「存在性与指针」，Kad provider 负责「谁持有」，二者经 CID
//! 衔接；内容面不改动公告消息的构造与验签线形。

use super::cid::Cid;

/// 从公告/元数据 payload JSON 中递归提取合法 blob CID（`{"$blob": "<cid>"}`
/// 标记；非 CID 形状的 hash 字符串静默跳过——pdsync 面历史数据可能混入
/// 非内容面 hash，内容面只认领自己寻址体系内的引用）。
///
/// 去重、升序返回。
pub fn extract_blob_cids(value: &serde_json::Value) -> Vec<Cid> {
    let mut out = std::collections::BTreeSet::new();
    for hash in crate::plugindata::blob::blob_refs_in(value) {
        if let Ok(cid) = Cid::parse(&hash) {
            out.insert(cid);
        }
    }
    out.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_valid_cids_only() {
        let good = Cid::from_data(b"file-a");
        let good2 = Cid::from_data(b"file-b");
        let value = serde_json::json!({
            "kind": "topic-announce",
            "title": "示例议题",
            "files": [
                { "$blob": good.as_str(), "name": "a.zip", "size": 3 },
                { "$blob": good2.as_str() },
                // 非 CID 形状（pdsync 面历史 hash、短 hash 等）不认领
                { "$blob": "abc123" },
                { "$blob": "A".repeat(64) }
            ],
            "nested": { "repo": { "$blob": good.as_str() } }
        });
        let cids = extract_blob_cids(&value);
        assert_eq!(cids.len(), 2, "去重 + 只收合法 CID");
        assert!(cids.contains(&good));
        assert!(cids.contains(&good2));
    }

    #[test]
    fn empty_when_no_refs() {
        assert!(extract_blob_cids(&serde_json::json!({"a": 1})).is_empty());
        assert!(extract_blob_cids(&serde_json::Value::Null).is_empty());
    }
}

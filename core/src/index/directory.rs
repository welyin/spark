//! indexer 目录与角色覆盖配置（wiki/protocol/community/affair-metadata.md §7
//! 目录面）：indexer 节点自公告「名片」让轻客户端发现可用 indexer；角色
//! 配置从纯布尔扩展为「启用 + 按区域/主题的子集覆盖」。
//!
//! - 覆盖配置 `IndexCoverage`：regions/topics 双维度，空列表 = 该维度不限
//!   （两维皆空 = 全覆盖，与旧纯布尔启用等价）。生效路径：gossip 收录过滤
//!   （角色启用且覆盖非全量时只收录子集内公告）+ 查询应答门控（覆盖外查询
//!   回 `indexer-not-covered`）+ 自公告名片内容。
//! - 名片线形 `indexer-card`：`spark-affair-meta` 信封 `type='indexer-card'`
//!   （domain='affair'、id=peerId），payload 自含 libp2p 节点私钥签名
//!   （验签公钥从 peerId 内嵌提取——node-announce 同款自证口径，证明
//!   「该 peerId 持有者宣告了这份覆盖」）。
//! - 目录簿记：存储键 `affmeta:dir:{peerId}`，本地键不进同步；同 peerId 按
//!   updatedAt 大者胜，新鲜度以本地收录时刻（lastSeenAt）判 TTL。目录条目
//!   是**线索而非信任根**——查询结果的确定性/可复算性不变，选错 indexer
//!   的代价只是换一家重查。
//!
//! 纯逻辑 + 存储簿记：不碰网络与运行时，时间一律 `now_ms` 注入；发布/收录
//! 编排在 p2p 事件循环（gossip.rs/tick.rs），门面在 kernel/index_ops。

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::storage::{ScanOptions, StorageBackend};

use super::IndexError;

/// 名片信封类型（`spark-affair-meta` 主题内分流，与 affair-meta/org-card 并列）。
pub const INDEXER_CARD_TYPE: &str = "indexer-card";
/// 名片版本（indexerV 恒 1）。
pub const INDEXER_CARD_V: u64 = 1;
/// 单张名片序列化 ≤ 4 KB（与元数据公告同档体积卫生）。
pub const INDEXER_CARD_MAX_BYTES: usize = 4 * 1024;
/// 名片新鲜度 TTL：自公告周期（node-announce 5 min）×3 = 15 min，按本地
/// 收录时刻计（发布方时钟不可信，只用于同 peerId 的新旧裁决）。
pub const INDEXER_CARD_TTL_MS: i64 = 15 * 60_000;
/// 目录存储键前缀（本地键，不进同步）。
pub const DIR_PREFIX: &str = "affmeta:dir:";
/// 目录容量上限（超限按 lastSeenAt 最旧逐出）。
pub const DIR_MAX: usize = 1024;
/// 覆盖配置单维条目数上限（与公告 tags 上限同口径）。
pub const COVERAGE_MAX_ITEMS: usize = 16;
/// 覆盖配置单条目长度上限（UTF-16；与公告 tag 上限同口径）。
pub const COVERAGE_ITEM_MAX_UTF16: usize = 32;
/// peerId 长度上限（base58 身份 multihash 实测 ~52，留余量）。
const PEER_ID_MAX: usize = 128;

// ------------------------------------------------------------------
// 覆盖配置
// ------------------------------------------------------------------

/// 子集覆盖配置：regions/topics 双维度，空列表 = 该维度不限。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexCoverage {
    /// 覆盖的区域代码集合（与公告 `region` 同口径，如 "110105"）。
    #[serde(default)]
    pub regions: Vec<String>,
    /// 覆盖的主题（标签）集合（与公告 `tags` 同口径）。
    #[serde(default)]
    pub topics: Vec<String>,
}

impl IndexCoverage {
    /// 全覆盖（两维皆不限）。
    pub fn is_full(&self) -> bool {
        self.regions.is_empty() && self.topics.is_empty()
    }

    /// 查询覆盖判定（应答门控）：覆盖维度受限时，查询必须带对应过滤且落在
    /// 覆盖内——受限维度缺席过滤条件的查询无法保证结果完整性，判不覆盖
    /// （客户端据此换目录内其他 indexer 重查）。regions 受限：query.region
    /// 须 ∈ regions；topics 受限：query.tags 须非空且全部 ∈ topics。
    pub fn covers_query(&self, region: Option<&str>, tags: &[String]) -> bool {
        if !self.regions.is_empty()
            && !region.is_some_and(|r| self.regions.iter().any(|c| c == r))
        {
            return false;
        }
        if !self.topics.is_empty()
            && (tags.is_empty() || !tags.iter().all(|t| self.topics.contains(t)))
        {
            return false;
        }
        true
    }

    /// 公告收录判定（收录过滤）：公告落在覆盖子集内才收录。regions 受限：
    /// 公告 region 须 ∈ regions；topics 受限：公告 tags 与 topics 有交集。
    pub fn covers_announce(&self, region: Option<&str>, tags: &[String]) -> bool {
        if !self.regions.is_empty()
            && !region.is_some_and(|r| self.regions.iter().any(|c| c == r))
        {
            return false;
        }
        if !self.topics.is_empty() && !tags.iter().any(|t| self.topics.contains(t)) {
            return false;
        }
        true
    }
}

/// 覆盖配置线形校验：各维 ≤16 条、每条 1–32 UTF-16。
pub fn coverage_valid(coverage: &IndexCoverage) -> bool {
    let list_ok = |items: &[String]| {
        items.len() <= COVERAGE_MAX_ITEMS
            && items
                .iter()
                .all(|s| (1..=COVERAGE_ITEM_MAX_UTF16).contains(&s.encode_utf16().count()))
    };
    list_ok(&coverage.regions) && list_ok(&coverage.topics)
}

/// indexer 角色配置（会话级内存态共享格的线形）：启用位 + 子集覆盖。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IndexRoleConfig {
    pub enabled: bool,
    pub coverage: IndexCoverage,
}

// ------------------------------------------------------------------
// 名片线形（indexer-card payload）
// ------------------------------------------------------------------

/// indexer 名片（目录公告 payload）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexerCard {
    pub peer_id: String,
    pub coverage: IndexCoverage,
    pub updated_at: i64,
    /// libp2p 节点私钥对 [`build_card_signing_payload`] 的签名（base64）。
    pub signature: String,
}

/// 待签名载荷（固定键序紧凑 JSON：indexerV → peerId → regions → topics →
/// updatedAt；regions/topics 恒在，空即空数组——签名线形不随可选字段漂移）。
pub fn build_card_signing_payload(peer_id: &str, coverage: &IndexCoverage, updated_at: i64) -> String {
    let mut map = Map::new();
    map.insert(
        "indexerV".to_string(),
        Value::Number(INDEXER_CARD_V.into()),
    );
    map.insert("peerId".to_string(), Value::String(peer_id.to_string()));
    map.insert(
        "regions".to_string(),
        serde_json::to_value(&coverage.regions).expect("regions serialize"),
    );
    map.insert(
        "topics".to_string(),
        serde_json::to_value(&coverage.topics).expect("topics serialize"),
    );
    map.insert(
        "updatedAt".to_string(),
        Value::Number(updated_at.into()),
    );
    serde_json::to_string(&Value::Object(map)).expect("card payload is always serializable")
}

/// 组装名片（发布侧：签名字节由调用方以节点私钥产出）。
pub fn build_card(
    peer_id: &str,
    coverage: &IndexCoverage,
    updated_at: i64,
    signature: &[u8],
) -> IndexerCard {
    use base64::Engine as _;
    IndexerCard {
        peer_id: peer_id.to_string(),
        coverage: coverage.clone(),
        updated_at,
        signature: base64::engine::general_purpose::STANDARD.encode(signature),
    }
}

/// 名片 payload 线形（固定键序：indexerV → peerId → regions → topics →
/// updatedAt → signature）。
pub fn card_to_value(card: &IndexerCard) -> Value {
    let mut map = Map::new();
    map.insert(
        "indexerV".to_string(),
        Value::Number(INDEXER_CARD_V.into()),
    );
    map.insert("peerId".to_string(), Value::String(card.peer_id.clone()));
    map.insert(
        "regions".to_string(),
        serde_json::to_value(&card.coverage.regions).expect("regions serialize"),
    );
    map.insert(
        "topics".to_string(),
        serde_json::to_value(&card.coverage.topics).expect("topics serialize"),
    );
    map.insert(
        "updatedAt".to_string(),
        Value::Number(card.updated_at.into()),
    );
    map.insert(
        "signature".to_string(),
        Value::String(card.signature.clone()),
    );
    Value::Object(map)
}

/// 解析并校验名片 payload（结构 → 覆盖线形 → 验签）。验签公钥从 peerId
/// 内嵌提取（identity multihash；node-announce 同款口径）。任一不符返回
/// `bad-indexer-card`。
pub fn parse_card(payload: &Value) -> Result<IndexerCard, IndexError> {
    use base64::Engine as _;
    const REASON: &str = "bad-indexer-card";
    let err = || IndexError::BadAnnounce(REASON);
    if payload.to_string().len() > INDEXER_CARD_MAX_BYTES {
        return Err(err());
    }
    let obj = payload.as_object().ok_or_else(err)?;
    if obj.get("indexerV").and_then(Value::as_u64) != Some(INDEXER_CARD_V) {
        return Err(err());
    }
    let peer_id = obj
        .get("peerId")
        .and_then(Value::as_str)
        .ok_or_else(err)?;
    if peer_id.is_empty() || peer_id.len() > PEER_ID_MAX {
        return Err(err());
    }
    // peerId 必须内嵌可提取的 ed25519 公钥（身份 multihash 形态），
    // 否则验签无从谈起
    let raw_public = crate::identity::peer_id::ed25519_public_key_from_peer_id(peer_id)
        .ok_or_else(err)?;
    let parse_list = |key: &str| -> Result<Vec<String>, IndexError> {
        match obj.get(key) {
            None | Some(Value::Null) => Ok(Vec::new()),
            Some(v) => {
                let arr = v.as_array().ok_or_else(err)?;
                arr.iter()
                    .map(|item| item.as_str().map(ToString::to_string).ok_or_else(err))
                    .collect()
            }
        }
    };
    let coverage = IndexCoverage {
        regions: parse_list("regions")?,
        topics: parse_list("topics")?,
    };
    if !coverage_valid(&coverage) {
        return Err(err());
    }
    let updated_at = obj
        .get("updatedAt")
        .and_then(Value::as_i64)
        .ok_or_else(err)?;
    if updated_at <= 0 {
        return Err(err());
    }
    let signature = obj
        .get("signature")
        .and_then(Value::as_str)
        .ok_or_else(err)?;
    let sig_bytes = base64::engine::general_purpose::STANDARD
        .decode(signature)
        .map_err(|_| err())?;
    if sig_bytes.len() != 64 {
        return Err(err());
    }
    let mut sig_arr = [0u8; 64];
    sig_arr.copy_from_slice(&sig_bytes);
    let signing_payload = build_card_signing_payload(peer_id, &coverage, updated_at);
    let verifying_key = ed25519_dalek::VerifyingKey::from_bytes(&raw_public).map_err(|_| err())?;
    use ed25519_dalek::Verifier as _;
    verifying_key
        .verify(
            signing_payload.as_bytes(),
            &ed25519_dalek::Signature::from_bytes(&sig_arr),
        )
        .map_err(|_| err())?;
    Ok(IndexerCard {
        peer_id: peer_id.to_string(),
        coverage,
        updated_at,
        signature: signature.to_string(),
    })
}

// ------------------------------------------------------------------
// 目录簿记（affmeta:dir:{peerId}）
// ------------------------------------------------------------------

/// 目录条目（收录时刻为本地时钟；updatedAt 为发布方时钟，只做新旧裁决）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexerDirEntry {
    pub peer_id: String,
    #[serde(default)]
    pub coverage: IndexCoverage,
    pub updated_at: i64,
    pub last_seen_at: i64,
}

/// upsert 结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DirUpsert {
    Inserted,
    Replaced,
    /// 同 updatedAt 重复到达（仅刷新 lastSeenAt）。
    Duplicate,
    /// 更旧的 updatedAt，丢弃。
    Stale,
}

fn dir_key(peer_id: &str) -> String {
    format!("{DIR_PREFIX}{peer_id}")
}

/// 收录名片入目录：同 peerId 按 updatedAt 大者胜，lastSeenAt 记本地收录
/// 时刻；随后惰性清过期 + 容量逐出（lastSeenAt 最旧先出）。
pub fn upsert_card<S: StorageBackend>(
    storage: &mut S,
    card: &IndexerCard,
    now_ms: i64,
) -> Result<DirUpsert, IndexError> {
    let key = dir_key(&card.peer_id);
    let existing: Option<IndexerDirEntry> = storage
        .get(&key)?
        .and_then(|raw| serde_json::from_str(&raw).ok());
    let outcome = match &existing {
        Some(e) if card.updated_at < e.updated_at => DirUpsert::Stale,
        Some(e) if card.updated_at == e.updated_at => {
            let mut entry = e.clone();
            entry.last_seen_at = now_ms;
            storage.put(&key, &serde_json::to_string(&entry).expect("dir entry serializable"))?;
            DirUpsert::Duplicate
        }
        _ => {
            let entry = IndexerDirEntry {
                peer_id: card.peer_id.clone(),
                coverage: card.coverage.clone(),
                updated_at: card.updated_at,
                last_seen_at: now_ms,
            };
            storage.put(&key, &serde_json::to_string(&entry).expect("dir entry serializable"))?;
            match existing {
                Some(_) => DirUpsert::Replaced,
                None => DirUpsert::Inserted,
            }
        }
    };
    if !matches!(outcome, DirUpsert::Stale) {
        evict_to_cap(storage, now_ms)?;
    }
    Ok(outcome)
}

/// 列出新鲜目录条目（惰性清除过期后返回，按 peerId 升序——确定性消费顺序）。
pub fn list_indexers<S: StorageBackend>(
    storage: &mut S,
    now_ms: i64,
) -> Result<Vec<IndexerDirEntry>, IndexError> {
    let mut entries = Vec::new();
    for (key, raw) in storage.scan(&ScanOptions::prefix(DIR_PREFIX))? {
        let Ok(entry) = serde_json::from_str::<IndexerDirEntry>(&raw) else {
            continue;
        };
        if now_ms.saturating_sub(entry.last_seen_at) > INDEXER_CARD_TTL_MS {
            let _ = storage.delete(&key);
            continue;
        }
        entries.push(entry);
    }
    entries.sort_by(|a, b| a.peer_id.cmp(&b.peer_id));
    Ok(entries)
}

/// 从目录条目按查询过滤条件选一家 indexer（覆盖匹配 + peerId 升序取首：
/// 同目录状态任何节点选同一家，确定性口径）。无覆盖匹配返回 None（调用方
/// 可退化为不带过滤重试或报「无可用 indexer」）。
pub fn pick_indexer(
    entries: &[IndexerDirEntry],
    region: Option<&str>,
    tags: &[String],
) -> Option<String> {
    let mut candidates: Vec<&IndexerDirEntry> = entries
        .iter()
        .filter(|e| e.coverage.covers_query(region, tags))
        .collect();
    candidates.sort_by(|a, b| a.peer_id.cmp(&b.peer_id));
    candidates.first().map(|e| e.peer_id.clone())
}

/// 容量逐出：先清过期，仍超限按 lastSeenAt 最旧逐出到上限。
fn evict_to_cap<S: StorageBackend>(storage: &mut S, now_ms: i64) -> Result<(), IndexError> {
    let mut entries = Vec::new();
    for (key, raw) in storage.scan(&ScanOptions::prefix(DIR_PREFIX))? {
        match serde_json::from_str::<IndexerDirEntry>(&raw) {
            Ok(entry) if now_ms.saturating_sub(entry.last_seen_at) <= INDEXER_CARD_TTL_MS => {
                entries.push((key, entry))
            }
            // 过期或损坏一并清出
            _ => {
                let _ = storage.delete(&key);
            }
        }
    }
    if entries.len() > DIR_MAX {
        entries.sort_by(|a, b| a.1.last_seen_at.cmp(&b.1.last_seen_at).then(a.0.cmp(&b.0)));
        for (key, _) in entries.iter().take(entries.len() - DIR_MAX) {
            let _ = storage.delete(key);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::MemoryStorage;

    /// 测试用身份：libp2p ed25519 密钥对 + peerId（与线上一致的提取/验签路径）。
    fn test_peer(seed: u8) -> (libp2p::identity::Keypair, String) {
        let mut secret = [seed; 32];
        secret[0] = seed;
        let kp = libp2p::identity::Keypair::ed25519_from_bytes(secret).unwrap();
        let peer_id = libp2p::PeerId::from_public_key(&kp.public()).to_base58();
        (kp, peer_id)
    }

    fn signed_card(kp: &libp2p::identity::Keypair, peer_id: &str, coverage: &IndexCoverage, updated_at: i64) -> IndexerCard {
        let payload = build_card_signing_payload(peer_id, coverage, updated_at);
        let sig = kp.sign(payload.as_bytes()).unwrap();
        build_card(peer_id, coverage, updated_at, &sig)
    }

    #[test]
    fn coverage_gate_matrix() {
        let full = IndexCoverage::default();
        assert!(full.is_full());
        assert!(full.covers_query(None, &[]));
        assert!(full.covers_announce(None, &[]));

        let regional = IndexCoverage {
            regions: vec!["110105".to_string()],
            topics: Vec::new(),
        };
        // 查询门控：受限维度必须带过滤且落覆盖内
        assert!(regional.covers_query(Some("110105"), &[]));
        assert!(!regional.covers_query(Some("110101"), &[]));
        assert!(!regional.covers_query(None, &[]), "缺 region 过滤无法保证完整性");
        // 收录过滤
        assert!(regional.covers_announce(Some("110105"), &["hoa".to_string()]));
        assert!(!regional.covers_announce(Some("110101"), &[]));
        assert!(!regional.covers_announce(None, &[]));

        let topical = IndexCoverage {
            regions: Vec::new(),
            topics: vec!["hoa".to_string(), "vote".to_string()],
        };
        assert!(topical.covers_query(None, &["hoa".to_string()]));
        assert!(!topical.covers_query(None, &["hoa".to_string(), "other".to_string()]));
        assert!(!topical.covers_query(None, &[]));
        assert!(topical.covers_announce(None, &["x".to_string(), "vote".to_string()]));
        assert!(!topical.covers_announce(None, &["x".to_string()]));

        // 双维受限：两维都须满足
        let both = IndexCoverage {
            regions: vec!["110105".to_string()],
            topics: vec!["hoa".to_string()],
        };
        assert!(both.covers_query(Some("110105"), &["hoa".to_string()]));
        assert!(!both.covers_query(Some("110105"), &[]));
        assert!(!both.covers_query(None, &["hoa".to_string()]));

        // 线形校验
        assert!(coverage_valid(&both));
        assert!(!coverage_valid(&IndexCoverage {
            regions: vec!["x".repeat(33)],
            topics: Vec::new(),
        }));
        assert!(!coverage_valid(&IndexCoverage {
            regions: (0..17).map(|i| i.to_string()).collect(),
            topics: Vec::new(),
        }));
        assert!(!coverage_valid(&IndexCoverage {
            regions: vec![String::new()],
            topics: Vec::new(),
        }));
    }

    #[test]
    fn card_roundtrip_and_tamper_rejected() {
        let (kp, peer_id) = test_peer(7);
        let coverage = IndexCoverage {
            regions: vec!["110105".to_string()],
            topics: vec!["hoa".to_string()],
        };
        let card = signed_card(&kp, &peer_id, &coverage, 1720000000000);
        let value = card_to_value(&card);
        let parsed = parse_card(&value).unwrap();
        assert_eq!(parsed, card);

        // 篡改覆盖 / updatedAt / peerId → 验签失败
        let mut tampered = value.clone();
        tampered["topics"] = serde_json::json!(["other"]);
        assert!(parse_card(&tampered).is_err());
        let mut tampered = value.clone();
        tampered["updatedAt"] = serde_json::json!(1720000000001i64);
        assert!(parse_card(&tampered).is_err());
        let (_, other_peer) = test_peer(8);
        let mut tampered = value.clone();
        tampered["peerId"] = serde_json::json!(other_peer);
        assert!(parse_card(&tampered).is_err());

        // 结构拒绝：版本不符 / peerId 非身份形态 / 覆盖越限
        let mut bad = value.clone();
        bad["indexerV"] = serde_json::json!(2);
        assert!(parse_card(&bad).is_err());
        let mut bad = value.clone();
        bad["peerId"] = serde_json::json!("not-a-peer-id");
        assert!(parse_card(&bad).is_err());
    }

    #[test]
    fn directory_arbitration_freshness_and_pick() {
        let mut s = MemoryStorage::new();
        let (kp_a, peer_a) = test_peer(1);
        let (kp_b, peer_b) = test_peer(2);
        let regional = IndexCoverage {
            regions: vec!["110105".to_string()],
            topics: Vec::new(),
        };

        // 收录：全覆盖 peer_b + 区域覆盖 peer_a
        let card_b = signed_card(&kp_b, &peer_b, &IndexCoverage::default(), 1000);
        let card_a = signed_card(&kp_a, &peer_a, &regional, 2000);
        assert_eq!(upsert_card(&mut s, &card_b, 10_000).unwrap(), DirUpsert::Inserted);
        assert_eq!(upsert_card(&mut s, &card_a, 11_000).unwrap(), DirUpsert::Inserted);

        // 裁决：旧 updatedAt 丢弃，同 updatedAt 只刷新 lastSeenAt，新的替换
        let stale_a = signed_card(&kp_a, &peer_a, &IndexCoverage::default(), 1500);
        assert_eq!(upsert_card(&mut s, &stale_a, 12_000).unwrap(), DirUpsert::Stale);
        assert_eq!(upsert_card(&mut s, &card_a, 13_000).unwrap(), DirUpsert::Duplicate);
        let newer_a = signed_card(&kp_a, &peer_a, &regional, 3000);
        assert_eq!(upsert_card(&mut s, &newer_a, 14_000).unwrap(), DirUpsert::Replaced);

        let entries = list_indexers(&mut s, 20_000).unwrap();
        assert_eq!(entries.len(), 2);
        assert!(entries[0].peer_id <= entries[1].peer_id, "按 peerId 升序");
        assert_eq!(entries.iter().find(|e| e.peer_id == peer_a).unwrap().updated_at, 3000);

        // 选取：带区域过滤的查询两家都覆盖（全覆盖者任意过滤均可答），
        // 按 peerId 升序取首——同目录状态任何节点选同一家（确定性）
        let pick1 = pick_indexer(&entries, Some("110105"), &[]);
        let expect_min = peer_a.clone().min(peer_b.clone());
        assert_eq!(pick1.as_deref(), Some(expect_min.as_str()));
        // 无过滤查询：只有全覆盖 indexer 可完整应答
        let pick2 = pick_indexer(&entries, None, &[]);
        assert_eq!(pick2.as_deref(), Some(peer_b.as_str()));
        // 覆盖外区域过滤：全覆盖者仍可答；目录为空则无人可答
        assert_eq!(pick_indexer(&entries, Some("110101"), &[]), Some(peer_b.clone()));
        assert_eq!(pick_indexer(&[], None, &[]), None);

        // TTL：过期条目惰性清除
        let stale_entries = list_indexers(&mut s, 20_000 + INDEXER_CARD_TTL_MS + 1).unwrap();
        assert!(stale_entries.is_empty(), "过期名片应被清除");
    }
}

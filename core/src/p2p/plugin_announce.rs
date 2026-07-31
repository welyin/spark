//! plugin-announce：插件市场广播索引（wiki/protocol/plugin-dist.md §8，阶段 C 波次 2a）。
//!
//! - 声明消息自含签名与 PoW（不走 §3 信封）：签名绑定发布者 rootId（标识身份，
//!   不证明仓库写权限；信任锚仍是 plugin-dist §4.1 的仓库声明文件）；
//! - 规范载荷 = 固定键序紧凑 JSON（不含 pow/signature，§8.3）；
//! - PoW = hashcash：`sha256(规范载荷 || decimal(nonce))` 前 N bit 为 0（§8.4）；
//! - 接收校验链（§8.6，廉价优先）：结构 → 逐 peer 限流 → TTL/新鲜度 → PoW → 签名；
//! - relay 资历制由节点层实现（本模块提供判定所需常量与校验结果）；
//! - 本地索引：sled `mkt:ann:<id>`，单 id 只留 timestamp 最新一条，限量 + LRU（§8.7）。

use std::collections::{HashMap, VecDeque};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use super::constants::{
    PLUGIN_ANNOUNCE_ICON_MAX_CHARS, PLUGIN_ANNOUNCE_MAX_BYTES, PLUGIN_ANNOUNCE_MAX_FUTURE_MS,
    PLUGIN_ANNOUNCE_RATE_LIMIT_PER_HOUR, PLUGIN_ANNOUNCE_RATE_LIMIT_TRACKED_PEERS,
    PLUGIN_ANNOUNCE_TTL_MS, PLUGIN_ANNOUNCE_URL_MAX_CHARS, PLUGIN_ANNOUNCE_VERSION_MAX_CHARS,
    PLUGIN_MARKET_INDEX_COUNT_KEY, PLUGIN_MARKET_INDEX_MAX, PLUGIN_MARKET_INDEX_PREFIX,
};
use crate::storage::{ScanOptions, StorageBackend};

/// 消息类型固定值（§8.2）。
pub const PLUGIN_ANNOUNCE_TYPE: &str = "spark-plugin-announce";

// ------------------------------------------------------------------
// 声明消息（§8.2）
// ------------------------------------------------------------------

/// PoW 字段（§8.2：bits 为声明难度，nonce 为工作量计数）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnnouncePow {
    pub bits: u32,
    pub nonce: u64,
}

/// plugin-announce 声明消息（§8.2；serde 线形 camelCase）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginAnnounce {
    #[serde(rename = "type")]
    pub msg_type: String,
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub icon: String,
    pub summary: String,
    pub category: String,
    pub version: String,
    #[serde(default)]
    pub release_url: String,
    pub timestamp: i64,
    pub ttl: i64,
    pub publisher: String,
    pub pub_key: String,
    pub pow: AnnouncePow,
    pub signature: String,
}

/// 发布侧输入（开发者模式命令参数；timestamp/ttl/pow/signature 由内核补齐）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginAnnounceInput {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub icon: String,
    pub summary: String,
    pub category: String,
    pub version: String,
    #[serde(default)]
    pub release_url: String,
}

/// 接收侧拒绝原因（校验链按序，§8.6；任一失败静默丢弃并扣传播源 peer 分）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PluginAnnounceReject {
    /// JSON 解析/字段与长度/id 语法/pow.bits 下限/消息总大小。
    Structure,
    /// 逐 peer 限流（10 条/小时滑动窗口）。
    RateLimited,
    /// 已过期或 timestamp 超出 ±窗口。
    Stale,
    /// PoW 前导零不足。
    Pow,
    /// pubKey↔publisher 绑定失败或验签失败。
    Signature,
}

// ------------------------------------------------------------------
// id 形状校验（§1.1；core 独立实现一份，relay 侧廉价结构校验用——
// 完整语法 + 仓库可达性核查在懒惰核查层，见 plugin-dist §8.8）
// ------------------------------------------------------------------

/// 段字符集 `[a-z0-9._-]`（消息中的 id 必须已是规范化线形：小写、无 scheme）。
fn segment_valid(segment: &str, max_len: usize) -> bool {
    !segment.is_empty()
        && segment.len() <= max_len
        && segment != "."
        && segment != ".."
        && segment
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'))
}

/// id 结构校验（§1.1）：host 白名单 + 3–11 段 + 段字符集/长度 + 总长 ≤ 256。
/// 输入必须已规范化（大写/scheme/尾斜杠一律拒，§8.2）。
pub fn announce_id_valid(id: &str) -> bool {
    if id.is_empty() || id.len() > 256 {
        return false;
    }
    let segments: Vec<&str> = id.split('/').collect();
    if segments.len() < 3 || segments.len() > 3 + 8 {
        return false;
    }
    if !matches!(segments[0], "github.com" | "gitlab.com" | "gitee.com") {
        return false;
    }
    if !segment_valid(segments[1], 100) || !segment_valid(segments[2], 100) {
        return false;
    }
    // repo 段带 `.git` 尾缀即未规范化（§1.2 规范化会剥掉一次）：广播侧直接拒
    if segments[2].ends_with(".git") {
        return false;
    }
    segments[3..].iter().all(|s| segment_valid(s, 64))
}

/// semver 三段（x.y.z，可带 `-预发布后缀` 与 `+build` 元数据；§2.1 口径的宽松形状校验）。
fn version_shape_valid(version: &str) -> bool {
    if version.is_empty() || version.chars().count() > PLUGIN_ANNOUNCE_VERSION_MAX_CHARS {
        return false;
    }
    let core = version.split(['-', '+']).next().unwrap_or("");
    let parts: Vec<&str> = core.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
}

/// 字段与长度校验（§8.2；发布侧与接收侧共用同一份）。
fn fields_valid(a: &PluginAnnounce) -> bool {
    let name_len = a.name.chars().count();
    let summary_len = a.summary.chars().count();
    if !(1..=64).contains(&name_len) || !(1..=256).contains(&summary_len) {
        return false;
    }
    if !matches!(a.category.as_str(), "foundation" | "business") {
        return false;
    }
    if !version_shape_valid(&a.version) {
        return false;
    }
    // icon：空 | https URL ≤ 512 | data: base64 ≤ 28672 字符（20 KB 二进制）
    let icon_ok = a.icon.is_empty()
        || (a.icon.starts_with("https://") && a.icon.len() <= PLUGIN_ANNOUNCE_URL_MAX_CHARS)
        || (a.icon.starts_with("data:") && a.icon.len() <= PLUGIN_ANNOUNCE_ICON_MAX_CHARS);
    if !icon_ok {
        return false;
    }
    // releaseUrl：空 | https ≤ 512
    if !a.release_url.is_empty()
        && !(a.release_url.starts_with("https://")
            && a.release_url.len() <= PLUGIN_ANNOUNCE_URL_MAX_CHARS)
    {
        return false;
    }
    announce_id_valid(&a.id)
}

// ------------------------------------------------------------------
// 规范序列化 / 签名 / PoW（§8.3/§8.4）
// ------------------------------------------------------------------

/// 规范载荷（§8.3：固定键序紧凑 JSON，不含 pow 与 signature）。
pub fn build_announce_payload(a: &PluginAnnounce) -> String {
    let mut map = Map::new();
    map.insert("type".to_string(), Value::String(a.msg_type.clone()));
    map.insert("id".to_string(), Value::String(a.id.clone()));
    map.insert("name".to_string(), Value::String(a.name.clone()));
    map.insert("icon".to_string(), Value::String(a.icon.clone()));
    map.insert("summary".to_string(), Value::String(a.summary.clone()));
    map.insert("category".to_string(), Value::String(a.category.clone()));
    map.insert("version".to_string(), Value::String(a.version.clone()));
    map.insert(
        "releaseUrl".to_string(),
        Value::String(a.release_url.clone()),
    );
    map.insert("timestamp".to_string(), Value::Number(a.timestamp.into()));
    map.insert("ttl".to_string(), Value::Number(a.ttl.into()));
    map.insert("publisher".to_string(), Value::String(a.publisher.clone()));
    map.insert("pubKey".to_string(), Value::String(a.pub_key.clone()));
    serde_json::to_string(&Value::Object(map)).expect("announce payload is always serializable")
}

/// 完整消息的紧凑 JSON（发布字节；固定键序与 §8.2 示例一致）。
pub fn plugin_announce_to_json(a: &PluginAnnounce) -> String {
    let mut map = Map::new();
    map.insert("type".to_string(), Value::String(a.msg_type.clone()));
    map.insert("id".to_string(), Value::String(a.id.clone()));
    map.insert("name".to_string(), Value::String(a.name.clone()));
    map.insert("icon".to_string(), Value::String(a.icon.clone()));
    map.insert("summary".to_string(), Value::String(a.summary.clone()));
    map.insert("category".to_string(), Value::String(a.category.clone()));
    map.insert("version".to_string(), Value::String(a.version.clone()));
    map.insert(
        "releaseUrl".to_string(),
        Value::String(a.release_url.clone()),
    );
    map.insert("timestamp".to_string(), Value::Number(a.timestamp.into()));
    map.insert("ttl".to_string(), Value::Number(a.ttl.into()));
    map.insert("publisher".to_string(), Value::String(a.publisher.clone()));
    map.insert("pubKey".to_string(), Value::String(a.pub_key.clone()));
    map.insert(
        "pow".to_string(),
        serde_json::json!({ "bits": a.pow.bits, "nonce": a.pow.nonce }),
    );
    map.insert("signature".to_string(), Value::String(a.signature.clone()));
    serde_json::to_string(&Value::Object(map)).expect("announce is always serializable")
}

/// 摘要前导零 bit 数（§8.4）。
pub fn leading_zero_bits(digest: &[u8]) -> u32 {
    let mut zeros = 0u32;
    for byte in digest {
        if *byte == 0 {
            zeros += 8;
        } else {
            zeros += byte.leading_zeros();
            break;
        }
    }
    zeros
}

/// PoW 摘要（§8.4）：`sha256(规范载荷 || decimal(nonce))`。
fn pow_digest(payload: &str, nonce: u64) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(payload.as_bytes());
    hasher.update(nonce.to_string().as_bytes());
    hasher.finalize().into()
}

/// 计算 PoW：从 0 递增 nonce 直至前导零 ≥ bits（发布侧；CPU 密集，
/// 调用方须放到阻塞线程/worker，避免卡 UI）。
pub fn mine_announce_nonce(payload: &str, bits: u32) -> u64 {
    let mut nonce = 0u64;
    loop {
        if leading_zero_bits(&pow_digest(payload, nonce)) >= bits {
            return nonce;
        }
        nonce = nonce.wrapping_add(1);
    }
}

/// 校验 PoW（§8.4）：实际前导零 ≥ max(声明 bits, min_bits)。
pub fn verify_announce_pow(payload: &str, pow: &AnnouncePow, min_bits: u32) -> bool {
    if pow.bits < min_bits {
        return false;
    }
    leading_zero_bits(&pow_digest(payload, pow.nonce)) >= pow.bits
}

/// 发布侧：字段校验 → 组装消息（timestamp/ttl 内核补齐）→ 签名。
/// 返回消息与规范载荷（调用方随后 mine_announce_nonce 并回填 pow）。
pub fn build_signed_announce(
    input: &PluginAnnounceInput,
    signing_key: &ed25519_dalek::SigningKey,
    timestamp: i64,
) -> std::result::Result<(PluginAnnounce, String), String> {
    use ed25519_dalek::Signer as _;

    let field_invalid = |field: &str, reason: &str| {
        format!("Plugin announce field invalid: {field}: {reason}")
    };
    if !announce_id_valid(&input.id) {
        return Err(format!("Plugin announce id invalid: {}", input.id));
    }
    let verifying_key = signing_key.verifying_key();
    let publisher = hex::encode(Sha256::digest(verifying_key.to_bytes()));
    let mut announce = PluginAnnounce {
        msg_type: PLUGIN_ANNOUNCE_TYPE.to_string(),
        id: input.id.clone(),
        name: input.name.clone(),
        icon: input.icon.clone(),
        summary: input.summary.clone(),
        category: input.category.clone(),
        version: input.version.clone(),
        release_url: input.release_url.clone(),
        timestamp,
        ttl: PLUGIN_ANNOUNCE_TTL_MS,
        publisher,
        pub_key: B64.encode(verifying_key.to_bytes()),
        pow: AnnouncePow { bits: 0, nonce: 0 },
        signature: String::new(),
    };
    if !fields_valid(&announce) {
        return Err(field_invalid("fields", "length or shape out of range"));
    }
    let payload = build_announce_payload(&announce);
    let signature = signing_key.sign(payload.as_bytes());
    announce.signature = B64.encode(signature.to_bytes());
    Ok((announce, payload))
}

// ------------------------------------------------------------------
// 接收侧校验器（§8.5/§8.6：结构 → 限流 → TTL → PoW → 签名）
// ------------------------------------------------------------------

/// 逐 peer 滑动窗口限流（10 条/小时；容量上限 1024 个 peer，满时回收过期
/// 条目、仍满则整体清空——对齐 dm §19 限流器口径）。
struct SlidingWindowLimiter {
    window_ms: i64,
    max_per_window: usize,
    hits: HashMap<String, VecDeque<i64>>,
}

impl SlidingWindowLimiter {
    fn new(window_ms: i64, max_per_window: usize) -> Self {
        Self {
            window_ms,
            max_per_window,
            hits: HashMap::new(),
        }
    }

    /// 命中限流返回 true；未命中则记录本次。
    fn is_rate_limited(&mut self, peer: &str, now_ms: i64) -> bool {
        if !self.hits.contains_key(peer) && self.hits.len() >= PLUGIN_ANNOUNCE_RATE_LIMIT_TRACKED_PEERS
        {
            let window = self.window_ms;
            self.hits.retain(|_, q| {
                q.back().is_some_and(|last| now_ms - *last < window)
            });
            if self.hits.len() >= PLUGIN_ANNOUNCE_RATE_LIMIT_TRACKED_PEERS {
                self.hits.clear();
            }
        }
        let window = self.window_ms;
        let queue = self.hits.entry(peer.to_string()).or_default();
        while queue.front().is_some_and(|t| now_ms - *t >= window) {
            queue.pop_front();
        }
        if queue.len() >= self.max_per_window {
            return true;
        }
        queue.push_back(now_ms);
        false
    }
}

/// 接收侧校验链 + 限流状态（§8.6）。
pub struct PluginAnnounceValidator {
    min_pow_bits: u32,
    limiter: SlidingWindowLimiter,
}

impl PluginAnnounceValidator {
    pub fn new(min_pow_bits: u32) -> Self {
        Self {
            min_pow_bits,
            limiter: SlidingWindowLimiter::new(3_600_000, PLUGIN_ANNOUNCE_RATE_LIMIT_PER_HOUR),
        }
    }

    /// 校验入站声明；`source_peer` 为传播源 peerId（限流键）。
    /// 通过即返回消息（调用方随后入索引并按资历制决定转发）。
    pub fn validate(
        &mut self,
        text: &str,
        source_peer: &str,
        now_ms: i64,
    ) -> std::result::Result<PluginAnnounce, PluginAnnounceReject> {
        let announce =
            parse_structure(text, self.min_pow_bits).ok_or(PluginAnnounceReject::Structure)?;

        if self.limiter.is_rate_limited(source_peer, now_ms) {
            return Err(PluginAnnounceReject::RateLimited);
        }
        // TTL/新鲜度（§8.5）：已过期即拒；远未来超 ±10 min 窗口即拒
        if now_ms.saturating_sub(announce.timestamp) > announce.ttl
            || announce.timestamp.saturating_sub(now_ms) > PLUGIN_ANNOUNCE_MAX_FUTURE_MS
        {
            return Err(PluginAnnounceReject::Stale);
        }
        // PoW（§8.4，一次 sha256，廉价）
        let payload = build_announce_payload(&announce);
        if !verify_announce_pow(&payload, &announce.pow, self.min_pow_bits) {
            return Err(PluginAnnounceReject::Pow);
        }
        // 签名（§8.3）：pubKey↔publisher 绑定 + ed25519 验签
        let pub_key_bytes = B64
            .decode(&announce.pub_key)
            .map_err(|_| PluginAnnounceReject::Signature)?;
        if pub_key_bytes.len() != 32 {
            return Err(PluginAnnounceReject::Signature);
        }
        if hex::encode(Sha256::digest(&pub_key_bytes)) != announce.publisher {
            return Err(PluginAnnounceReject::Signature);
        }
        let sig_bytes = B64
            .decode(&announce.signature)
            .map_err(|_| PluginAnnounceReject::Signature)?;
        if sig_bytes.len() != 64 {
            return Err(PluginAnnounceReject::Signature);
        }
        let mut key_arr = [0u8; 32];
        key_arr.copy_from_slice(&pub_key_bytes);
        let mut sig_arr = [0u8; 64];
        sig_arr.copy_from_slice(&sig_bytes);
        let verifying_key = ed25519_dalek::VerifyingKey::from_bytes(&key_arr)
            .map_err(|_| PluginAnnounceReject::Signature)?;
        use ed25519_dalek::Verifier;
        verifying_key
            .verify(
                payload.as_bytes(),
                &ed25519_dalek::Signature::from_bytes(&sig_arr),
            )
            .map_err(|_| PluginAnnounceReject::Signature)?;

        Ok(announce)
    }
}

/// 结构校验（§8.2/§8.6-1）：总大小 → JSON → type → 字段与长度 → id 语法 →
/// ttl 固定值 → pow.bits ≥ 本节点难度下限。
fn parse_structure(text: &str, min_pow_bits: u32) -> Option<PluginAnnounce> {
    if text.len() > PLUGIN_ANNOUNCE_MAX_BYTES {
        return None;
    }
    let announce: PluginAnnounce = serde_json::from_str(text).ok()?;
    if announce.msg_type != PLUGIN_ANNOUNCE_TYPE {
        return None;
    }
    if announce.ttl != PLUGIN_ANNOUNCE_TTL_MS {
        return None;
    }
    if announce.timestamp <= 0 {
        return None;
    }
    if announce.pow.bits < min_pow_bits {
        return None;
    }
    // nonce 上限 < 2^63（§8.2：JSON number 互操作口径，超出按结构非法）
    if announce.pow.nonce > i64::MAX as u64 {
        return None;
    }
    // publisher：64 位小写 hex（rootId 形状）
    if announce.publisher.len() != 64
        || !announce
            .publisher
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    {
        return None;
    }
    if !fields_valid(&announce) {
        return None;
    }
    Some(announce)
}

// ------------------------------------------------------------------
// 本地索引（§8.7：sled `mkt:ann:<id>`、单 id 最新、限量 + LRU）
// ------------------------------------------------------------------

/// 懒惰核查状态（§8.7/§8.8）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AnnounceVerified {
    /// 待核查（新入索引/被更新声明替换）。
    #[default]
    Pending,
    /// 核查通过（§4.1 仓库锚定验证），只有此状态进入市场视图。
    Verified,
    /// 核查失败（verifyError 记原因：unreachable / id-mismatch / 其他）。
    Failed,
}

/// 懒惰核查校正后的展示字段（§8.8：以仓库声明文件为准回写索引；
/// announce 自报值仅在 corrected 缺席时作占位）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CorrectedAnnounceFields {
    pub name: String,
    #[serde(default)]
    pub icon: String,
    pub summary: String,
    pub version: String,
}

/// 索引条目（§8.7 值线形）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginAnnounceIndexEntry {
    pub announce: PluginAnnounce,
    pub first_seen_at: i64,
    pub updated_at: i64,
    #[serde(default)]
    pub verified: AnnounceVerified,
    #[serde(default)]
    pub verify_error: String,
    #[serde(default)]
    pub verified_at: i64,
    /// 核查通过时回写的校正展示字段（§8.8）；同 id 新声明到达时重置为 None
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corrected: Option<CorrectedAnnounceFields>,
}

/// upsert 结果（ stale = 同 id 已有更新 timestamp，未入索引）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnnounceUpsert {
    Inserted,
    Replaced,
    /// 同 timestamp 重复到达（仅刷新 updatedAt）。
    Duplicate,
    Stale,
}

/// 本地索引存储（模式对齐 OverlayPeerStore：借 &mut storage，即用即弃）。
pub struct PluginAnnounceStore<'a> {
    storage: &'a mut dyn StorageBackend,
}

impl<'a> PluginAnnounceStore<'a> {
    pub fn new(storage: &'a mut dyn StorageBackend) -> Self {
        Self { storage }
    }

    fn key(id: &str) -> String {
        format!("{PLUGIN_MARKET_INDEX_PREFIX}{id}")
    }

    /// 读取单条索引（不存在或值损坏返回 None）。
    pub fn get(&mut self, id: &str) -> super::Result<Option<PluginAnnounceIndexEntry>> {
        let Some(raw) = self.storage.get(&Self::key(id))? else {
            return Ok(None);
        };
        Ok(serde_json::from_str(&raw).ok())
    }

    fn save(&mut self, entry: &PluginAnnounceIndexEntry) -> super::Result<()> {
        self.storage.put(
            &Self::key(&entry.announce.id),
            &serde_json::to_string(entry)?,
        )?;
        Ok(())
    }

    /// 入索引（§8.7）：单 id 只留 timestamp 最新一条；替换重置 verified 为
    /// pending；随后按需 LRU 逐出（updatedAt 最旧）并惰性清过期。
    pub fn upsert(
        &mut self,
        announce: &PluginAnnounce,
        now_ms: i64,
    ) -> super::Result<AnnounceUpsert> {
        let existing = self.get(&announce.id)?;
        let outcome = match &existing {
            Some(e) if announce.timestamp < e.announce.timestamp => AnnounceUpsert::Stale,
            Some(e) if announce.timestamp == e.announce.timestamp => {
                let mut entry = e.clone();
                entry.updated_at = now_ms;
                self.save(&entry)?;
                AnnounceUpsert::Duplicate
            }
            Some(e) => {
                self.save(&PluginAnnounceIndexEntry {
                    announce: announce.clone(),
                    first_seen_at: e.first_seen_at,
                    updated_at: now_ms,
                    verified: AnnounceVerified::Pending,
                    verify_error: String::new(),
                    verified_at: 0,
                    // 新声明到达：旧核查结论与校正字段一并作废
                    corrected: None,
                })?;
                AnnounceUpsert::Replaced
            }
            None => {
                self.save(&PluginAnnounceIndexEntry {
                    announce: announce.clone(),
                    first_seen_at: now_ms,
                    updated_at: now_ms,
                    verified: AnnounceVerified::Pending,
                    verify_error: String::new(),
                    verified_at: 0,
                    corrected: None,
                })?;
                AnnounceUpsert::Inserted
            }
        };
        if !matches!(outcome, AnnounceUpsert::Stale) {
            self.evict_if_needed(now_ms, matches!(outcome, AnnounceUpsert::Inserted))?;
        }
        Ok(outcome)
    }

    /// 懒惰核查落终态（§8.8）：verified + 原因 + 时间；条目不存在返回 false。
    /// `expected_timestamp` 绑定核查时读到的 announce.timestamp：核查期间同 id
    /// 新声明到达（替换条目）则本次结论作废丢弃，防旧结论覆盖新声明。
    /// 核查通过时回写校正展示字段（corrected）；失败清空 corrected。
    pub fn mark_verified(
        &mut self,
        id: &str,
        verified: AnnounceVerified,
        error: &str,
        now_ms: i64,
        expected_timestamp: i64,
        corrected: Option<CorrectedAnnounceFields>,
    ) -> super::Result<bool> {
        let Some(mut entry) = self.get(id)? else {
            return Ok(false);
        };
        if entry.announce.timestamp != expected_timestamp {
            return Ok(false);
        }
        entry.verified = verified;
        entry.verify_error = error.to_string();
        entry.verified_at = now_ms;
        entry.corrected = if verified == AnnounceVerified::Verified {
            corrected
        } else {
            None
        };
        self.save(&entry)?;
        Ok(true)
    }

    /// 列出全部条目（惰性清除过期条目后返回，按 updatedAt 降序）。
    pub fn list(&mut self, now_ms: i64) -> super::Result<Vec<PluginAnnounceIndexEntry>> {
        let mut entries = self.scan_entries(Some(now_ms))?;
        entries.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(entries)
    }

    /// 扫描全部条目；`purge_expired` 非 None 时顺带删除已过期条目。
    fn scan_entries(
        &mut self,
        purge_expired: Option<i64>,
    ) -> super::Result<Vec<PluginAnnounceIndexEntry>> {
        let rows = self
            .storage
            .scan(&ScanOptions::prefix(PLUGIN_MARKET_INDEX_PREFIX))?;
        let mut entries = Vec::new();
        for (key, raw) in rows {
            let Ok(entry) = serde_json::from_str::<PluginAnnounceIndexEntry>(&raw) else {
                continue;
            };
            if let Some(now_ms) = purge_expired
                && now_ms.saturating_sub(entry.announce.timestamp) > entry.announce.ttl
            {
                let _ = self.storage.delete(&key);
                continue;
            }
            entries.push(entry);
        }
        Ok(entries)
    }

    /// 容量控制（§8.7）：读计数键，新插入（`count_new`）才递增——Replace/Duplicate
    /// 不新增条目不递增，避免近似计数虚高触发无谓全量扫描；超限全量扫描按
    /// updatedAt 最旧逐出到上限，并以扫描结果重写计数（顺带修正漂移）。
    fn evict_if_needed(&mut self, now_ms: i64, count_new: bool) -> super::Result<()> {
        let count: u64 = self
            .storage
            .get(PLUGIN_MARKET_INDEX_COUNT_KEY)?
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(0);
        let count = count + u64::from(count_new);
        if count <= PLUGIN_MARKET_INDEX_MAX as u64 {
            self.storage
                .put(PLUGIN_MARKET_INDEX_COUNT_KEY, &count.to_string())?;
            return Ok(());
        }
        // 惰性清过期 + 全量扫描
        let mut entries = self.scan_entries(Some(now_ms))?;
        if entries.len() > PLUGIN_MARKET_INDEX_MAX {
            entries.sort_by(|a, b| a.updated_at.cmp(&b.updated_at));
            let evict = entries.len() - PLUGIN_MARKET_INDEX_MAX;
            for entry in entries.iter().take(evict) {
                let _ = self.storage.delete(&Self::key(&entry.announce.id));
            }
            entries.drain(..evict);
        }
        self.storage
            .put(PLUGIN_MARKET_INDEX_COUNT_KEY, &entries.len().to_string())?;
        Ok(())
    }
}

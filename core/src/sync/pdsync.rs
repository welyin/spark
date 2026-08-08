//! pdsync 个人域同步协议（P3）：三信封反熵。
//!
//! 复用 org-pull 的反熵模式，信封走 dm 直连（`from == to == 自己 rootId`）。
//! 三个 kind：
//! - `pdsync-hello`：摘要交换（category → 合并折叠 vv）。
//! - `pdsync-need`：diff 请求（category + 本地 knownVv）。
//! - `pdsync-data`：数据传输（逐条 key/value/meta，接收方逐条 `apply_personal_remote`）。
//!
//! 本模块是纯逻辑（存储泛型），不触碰 p2p/签名——信封装配与投递由
//! host / keepalive 侧完成（对齐 contact/service/sync.rs 的职责边界）。
//!
//! category 覆盖 P1–P5 已迁入的类别：联系人四域、设备清单、个人资料
//! （`profile:self`）、会话元数据（`msg:conv:personal:`）与组织数据
//! （`org:meta` / `ct:org` / `org:inv`）；消息（`msg:item` / `msg:app`）
//! 走 §6.2 窗口快照协议，不参与折叠/增量。

use std::collections::BTreeMap;

use serde_json::{Map, Value, json};

use crate::storage::{ScanOptions, StorageBackend};
use crate::sync::meta::{
    DocMeta, VersionVector, compare_version_vectors, merge_version_vectors, CompareResult,
};
use crate::sync::personal::get_personal_meta;

// ── category 注册表 ────────────────────────────────────────────────

/// 一个 pdsync category：命名 + 匹配的存储 key 前缀。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Category {
    /// category 名（hello/need/data 中使用的 `category` 字段值）。
    pub name: &'static str,
    /// 匹配的存储 key 前缀（可多个，任一命中即属该 category）。
    pub prefixes: &'static [&'static str],
}

/// pdsync category 注册表。
///
/// 顺序即 hello 中 `categories` 对象字段的迭代顺序（JSON 对象，顺序无关，
/// 此处仅便于人读）。
/// - P1 已启用：联系人四域 + 设备清单；
/// - P2 加入：个人资料 `profile:self` + 个人空间会话元数据 `msg:conv:personal:`；
/// - 消息（`msg:item`/`msg:app`）P4 加入（走窗口协议，不参与折叠）；
/// - 组织数据 P5 加入：`org:meta` / `ct:org` / `org:inv`（见 §6.3）。
///
/// `ct:org` 前缀覆盖组织空间全部 `ct:org:{orgId}:*` 键（成员 extra / req:out /
/// tags / tree）；tags/tree 集合型数据按整域单记录同步。
pub const CATEGORIES: &[Category] = &[
    Category { name: "ct:friend", prefixes: &["ct:friend:"] },
    Category { name: "ct:req", prefixes: &["ct:req:in:", "ct:req:out:"] },
    Category { name: "ct:tag", prefixes: &["ct:tag:"] },
    Category { name: "ct:group", prefixes: &["ct:group:"] },
    Category { name: "ct:blocked", prefixes: &["ct:blocked:"] },
    Category { name: "device", prefixes: &["device:"] },
    Category { name: "profile:self", prefixes: &["profile:self"] },
    Category { name: "msg:conv", prefixes: &["msg:conv:personal:"] },
    Category { name: "org:meta", prefixes: &["org:meta:"] },
    Category { name: "ct:org", prefixes: &["ct:org:"] },
    Category { name: "org:inv", prefixes: &["org:inv:in:", "org:inv:out:"] },
];

/// 按前缀从注册表解析 category（不存在 → `None`，如组织/消息前缀）。
pub fn category_for_key(key: &str) -> Option<&'static Category> {
    CATEGORIES
        .iter()
        .find(|c| c.prefixes.iter().any(|p| key.starts_with(p)))
}

/// category 名 → 注册项（need/data 按名回查）。
pub fn category_by_name(name: &str) -> Option<&'static Category> {
    CATEGORIES.iter().find(|c| c.name == name)
}

/// 自 FriendRecord 的存储键（`ct:friend:{rootId}`）。
///
/// 双设备同账号 → 同 rootId → 同键，但记录体的 `peer` 字段是设备相对值
/// （各存对方设备的寻址），互灌会让一端设备的自记录指向自己（自投/自拨
/// 失败）。pdsync 的折叠 vv、增量采集（含墓碑）与落库对该键对称排除——
/// 排除键两侧相同，折叠 vv 保持一致、无伪 diff；本机删自记录的墓碑也不
/// 推给对端（对端的自记录是它的设备相对数据）。对齐旧 contact-sync 通道
/// 的不变式（见 contact/service/sync.rs 模块注释）。
pub fn self_friend_key(root_id: &str) -> String {
    format!("{}{root_id}", crate::contact::FRIEND_PREFIX)
}

// ── hello 摘要折叠 ─────────────────────────────────────────────────

/// 收集某 category 的合并折叠 vv：扫描该 category 全部 key 前缀下所有
/// `pmeta:{key}`，逐条 `merge_version_vectors` 取 max。
///
/// `exclude_key`：对称排除的记录键（如 [`self_friend_key`] 的自记录——
/// 设备相对数据不参与折叠；双设备同账号排除键相同，两侧折叠仍一致）。
///
/// O(记录数) 扫描；个人域自设备记录量级小，可接受。
pub fn collect_category_vv<S: StorageBackend>(
    storage: &S,
    category: &Category,
    exclude_key: Option<&str>,
) -> crate::sync::SyncResult<VersionVector> {
    let mut folded = VersionVector::new();
    for prefix in category.prefixes {
        let meta_prefix = crate::sync::personal::personal_meta_key(*prefix);
        for (meta_key, raw) in storage.scan(&ScanOptions::prefix(&meta_prefix))? {
            // pmeta:{key} → key = strip pmeta 前缀
            let Some(record_key) = meta_key.strip_prefix(crate::sync::personal::PMETA_PREFIX)
            else {
                continue;
            };
            // 防串：meta key 剥离 pmeta 后必须仍命中 category 前缀（scan
            // 按 meta 前缀查，天然只会命中同 category 记录的 pmeta）。
            if !category.prefixes.iter().any(|p| record_key.starts_with(p)) {
                continue;
            }
            // 排除键（自记录）：设备相对数据不参与折叠/增量（含墓碑——
            // 本机删自记录不应删掉对端的）
            if exclude_key == Some(record_key) {
                continue;
            }
            if let Ok(meta) = serde_json::from_str::<DocMeta>(&raw) {
                folded = merge_version_vectors(Some(&folded), Some(&meta.vv));
            }
        }
    }
    Ok(folded)
}

/// 构建全部 category 的折叠 vv 摘要（hello 的 `categories` 字段）。
pub fn collect_all_categories<S: StorageBackend>(
    storage: &S,
    exclude_key: Option<&str>,
) -> crate::sync::SyncResult<Map<String, Value>> {
    let mut map = Map::new();
    for category in CATEGORIES {
        let vv = collect_category_vv(storage, category, exclude_key)?;
        map.insert(
            category.name.to_string(),
            serde_json::to_value(&vv).unwrap_or(Value::Object(Map::new())),
        );
    }
    Ok(map)
}

// ── diff 裁决 ──────────────────────────────────────────────────────

/// 一个 category 的 diff 结论。
#[derive(Clone, Debug)]
pub enum DiffOutcome {
    /// 本地落后：发 `pdsync-need`（携带本地 knownVv，对端据此补增量）。
    LocalBehind {
        /// 本地折叠 vv（作为 need 的 knownVv）。
        local_vv: VersionVector,
    },
    /// 本地领先：主动发 `pdsync-data` 推增量（即发即忘）。
    LocalAhead,
    /// 并发：折叠 vv 各有本地/对端领先的分量（如两台设备各自写了不同的
    /// 记录）。**双向交换**——既发 `pdsync-need` 请求对端缺的，也主动推
    /// 本机缺的（data 逐条按向量幂等去重，双发收敛）。
    Concurrent,
    /// 相等：不动（无 ping-pong）。
    Equal,
}

/// 对比本地折叠 vv 与对端 hello 摘要中的折叠 vv。
///
/// 折叠 vv 是 category 内所有记录的最大合并界，两台设备写入不同记录时
/// 折叠 vv 会并发（各有领先分量）——此时必须双向交换才能收敛，而非
/// 视为相等。任何一方纯落后/纯领先都只单向动作。
pub fn diff_category(local_vv: &VersionVector, remote_vv: &VersionVector) -> DiffOutcome {
    match compare_version_vectors(Some(local_vv), Some(remote_vv)) {
        CompareResult::Remote => DiffOutcome::LocalBehind {
            local_vv: local_vv.clone(),
        },
        CompareResult::Local => DiffOutcome::LocalAhead,
        CompareResult::Concurrent => DiffOutcome::Concurrent,
        CompareResult::Equal => DiffOutcome::Equal,
    }
}

// ── need → data 增量采集 ───────────────────────────────────────────

/// 收到的 `pdsync-data` 中的单条记录。
#[derive(Clone, Debug)]
pub struct PdsyncRecord {
    pub key: String,
    pub value: Value,
    pub meta: DocMeta,
}

/// 按 `knownVv` 采集 category 增量（need 的处理）：扫描该 category 全部
/// 记录，凡本地 pmeta 相对 knownVv 不是 `Local`/`Equal`（即 `Remote`/
/// `Concurrent`，或 knownVv 里没有该 nodeId）→ 纳入返回。
///
/// 天然筛出"对端缺的"，不必逐条比较两侧记录集合。
///
/// 墓碑（`tombstone: true`）以 `{key, value: null, meta}` 纳入推送——本体已删，
/// 墓碑只存在于 pmeta 扫描里；接收方据 `meta.tombstone` 执行删除（§5.3/§10）。
///
/// `exclude_key`：对称排除的记录键（[`self_friend_key`] 的自记录）——本体与
/// 墓碑两段扫描都跳过（本机删自记录不应删掉对端的）。
pub fn collect_incremental<S: StorageBackend>(
    storage: &S,
    category: &Category,
    known_vv: &VersionVector,
    exclude_key: Option<&str>,
) -> crate::sync::SyncResult<Vec<PdsyncRecord>> {
    let mut records = Vec::new();
    for prefix in category.prefixes {
        for (key, raw_value) in storage.scan(&ScanOptions::prefix(*prefix))? {
            // 排除键（自记录）：设备相对数据不推给对端
            if exclude_key == Some(key.as_str()) {
                continue;
            }
            let meta = match get_personal_meta(storage, &key)? {
                Some(m) => m,
                None => continue,
            };
            // 墓碑无本体（本体已删），不会出现在这条 scan 里；防御性跳过，
            // 墓碑增量由下方 pmeta 扫描覆盖
            if crate::sync::personal::is_tombstone(&meta) {
                continue;
            }
            // 增量判定：本地记录 vv 相对 knownVv——
            // - Local（本记录比 knownVv 新）/ Concurrent（knownVv 未覆盖本记录）
            //   → 对端缺本记录，纳入推送；
            // - Remote（knownVv 已覆盖本记录，对端有更新版）/ Equal → 跳过。
            match compare_version_vectors(Some(&meta.vv), Some(known_vv)) {
                CompareResult::Remote | CompareResult::Equal => continue,
                _ => {
                    // 损坏记录跳过并告警——不以 `null` 冒充空值推给对端
                    // （会把对端好数据覆盖成 null）；待本地修复后下轮同步补推
                    let value = match serde_json::from_str(&raw_value) {
                        Ok(v) => v,
                        Err(error) => {
                            eprintln!("[pdsync] skip corrupted record {key}: {error}");
                            continue;
                        }
                    };
                    records.push(PdsyncRecord { key, value, meta });
                }
            }
        }
        // 墓碑增量：本体已删的记录不在上面的记录 scan 里，扫 `pmeta:{prefix}`
        // 把对端缺失的墓碑一并推出（value 恒 null，meta 携带 tombstone=true）
        let meta_prefix = crate::sync::personal::personal_meta_key(*prefix);
        for (meta_key, raw_meta) in storage.scan(&ScanOptions::prefix(&meta_prefix))? {
            let Some(record_key) = meta_key.strip_prefix(crate::sync::personal::PMETA_PREFIX)
            else {
                continue;
            };
            if !category.prefixes.iter().any(|p| record_key.starts_with(p)) {
                continue;
            }
            // 排除键（自记录）：设备相对数据不参与折叠/增量（含墓碑——
            // 本机删自记录不应删掉对端的）
            if exclude_key == Some(record_key) {
                continue;
            }
            let Ok(meta) = serde_json::from_str::<DocMeta>(&raw_meta) else {
                continue;
            };
            if !crate::sync::personal::is_tombstone(&meta) {
                continue;
            }
            match compare_version_vectors(Some(&meta.vv), Some(known_vv)) {
                CompareResult::Remote | CompareResult::Equal => continue,
                _ => records.push(PdsyncRecord {
                    key: record_key.to_string(),
                    value: Value::Null,
                    meta,
                }),
            }
        }
    }
    Ok(records)
}

// ── 信封 body 构造 ─────────────────────────────────────────────────

/// 构造 `pdsync-hello` body：`{categories, msgWindow, attachmentPolicy}`。
///
/// `exclude_key`：折叠摘要对称排除的记录键（[`self_friend_key`] 的自记录）。
pub fn build_hello<S: StorageBackend>(
    storage: &S,
    msg_window_max_age_ms: i64,
    msg_window_max_per_conv: usize,
    attachment_policy: &str,
    exclude_key: Option<&str>,
) -> crate::sync::SyncResult<Value> {
    Ok(json!({
        "categories": collect_all_categories(storage, exclude_key)?,
        "msgWindow": {
            "maxAgeMs": msg_window_max_age_ms,
            "maxPerConv": msg_window_max_per_conv,
        },
        "attachmentPolicy": attachment_policy,
    }))
}

/// 解析 hello 的 categories 摘要 → category 名 → 折叠 vv。
pub fn parse_hello_categories(body: &Value) -> BTreeMap<String, VersionVector> {
    let mut map = BTreeMap::new();
    if let Some(categories) = body.get("categories").and_then(Value::as_object) {
        for (name, vv_val) in categories {
            if let Ok(vv) = serde_json::from_value(vv_val.clone()) {
                map.insert(name.clone(), vv);
            }
        }
    }
    map
}

/// 构造 `pdsync-need` body：`{category, knownVv}`。
pub fn build_need(category: &str, known_vv: &VersionVector) -> Value {
    json!({
        "category": category,
        "knownVv": known_vv,
    })
}

/// 解析 need body → (category, knownVv)。
pub fn parse_need(body: &Value) -> Option<(String, VersionVector)> {
    let category = body.get("category")?.as_str()?;
    let known_vv = serde_json::from_value(body.get("knownVv")?.clone()).ok()?;
    Some((category.to_string(), known_vv))
}

/// 构造单批 `pdsync-data` body：`{category, records, batchSeq, batchTotal}`。
///
/// `records` 每项 `{key, value, meta}`；meta 序列化沿用 [`DocMeta`]。
pub fn build_data_batch(
    category: &str,
    records: &[PdsyncRecord],
    batch_seq: usize,
    batch_total: usize,
) -> Value {
    let items: Vec<Value> = records
        .iter()
        .map(|r| {
            json!({
                "key": r.key,
                "value": r.value,
                "meta": serde_json::to_value(&r.meta).unwrap_or(Value::Null),
            })
        })
        .collect();
    json!({
        "category": category,
        "records": items,
        "batchSeq": batch_seq,
        "batchTotal": batch_total,
    })
}

/// 解析 data body → (category, records)。
pub fn parse_data(body: &Value) -> Option<(String, Vec<PdsyncRecord>)> {
    let category = body.get("category")?.as_str()?;
    let items = body.get("records")?.as_array()?;
    let mut records = Vec::with_capacity(items.len());
    for item in items {
        let key = item.get("key")?.as_str()?.to_string();
        let value = item.get("value").cloned().unwrap_or(Value::Null);
        let meta: DocMeta = serde_json::from_value(item.get("meta")?.clone()).ok()?;
        records.push(PdsyncRecord { key, value, meta });
    }
    Some((category.to_string(), records))
}

// ── 消息窗口（P4）──────────────────────────────────────────────────

/// 消息窗口参数（对端 hello 声明，用于发送方裁剪）。
#[derive(Clone, Copy, Debug)]
pub struct MessageWindow {
    /// 每会话最大条数。
    pub max_per_conv: usize,
    /// 窗口时间下界（毫秒，相对当前时间）。
    pub max_age_ms: i64,
}

impl MessageWindow {
    /// 默认窗口（对齐文档 §6：500 条 / 30 天）。
    pub fn default_() -> Self {
        Self { max_per_conv: 500, max_age_ms: 30 * 24 * 3600 * 1000 }
    }

    /// 从 hello 的 `msgWindow` 解析；缺失或无效回退默认。
    pub fn from_hello(body: &Value) -> Self {
        let mut w = Self::default_();
        if let Some(mw) = body.get("msgWindow") {
            if let Some(n) = mw.get("maxPerConv").and_then(Value::as_u64) {
                w.max_per_conv = n as usize;
            }
            if let Some(n) = mw.get("maxAgeMs").and_then(Value::as_i64) {
                // 对端声明不可信：钳制到 [0, i64::MAX]——负值 / i64::MIN
                // 会在 cutoff 减法触发溢出 panic（本函数在 io_lock 内调用）
                w.max_age_ms = n.clamp(0, i64::MAX);
            }
        }
        w
    }

    /// "全部"窗口：条数与时间都极大（设备声明收齐完整历史）。
    pub fn all() -> Self {
        Self { max_per_conv: usize::MAX, max_age_ms: i64::MAX }
    }
}

/// 从消息存储键解析 convId。
///
/// 键格式 `msg:item:{space}:{convId}:{createdAt:013}:{msgId}`，convId 是
/// 前缀后的第一段。`space` 固定为 `personal`（pdsync 仅个人域）。
/// 应用消息键 `msg:app:personal:{pluginId}:{createdAt:013}:{msgId}` 的
/// convId 为 `app:{pluginId}`（§20.1）。
fn message_conv_id(key: &str) -> Option<String> {
    if let Some(rest) = key.strip_prefix("msg:app:personal:") {
        // pluginId 字符集不含 `:`（is_valid_plugin_id），第一段即 pluginId
        let plugin_id = rest.split(':').next()?;
        if plugin_id.is_empty() {
            return None;
        }
        return Some(format!("{}{plugin_id}", crate::message::APP_CONV_PREFIX));
    }
    let rest = key.strip_prefix("msg:item:personal:")?;
    // convId 之后是 `:{13位零填充时间戳}:`——用该分隔符切分。convId 自身
    // 可含 `:`（如 `app:{pluginId}`），故取最后一个 `:[0-9]{13}:` 为切分点。
    let bytes = rest.as_bytes();
    let mut i = bytes.len();
    while i > 0 {
        let Some(rel) = rest[..i].rfind(':') else {
            break;
        };
        // rel 是时间戳前的 `:`，convId = rest[..rel]（不含该冒号）
        let cand = rel + 1;
        let tail = &rest[cand..];
        if tail.len() > 14 && tail.as_bytes()[13] == b':' {
            let digits = &tail[..13];
            if digits.bytes().all(|b| b.is_ascii_digit()) {
                return Some(rest[..rel].to_string());
            }
        }
        i = rel;
    }
    None
}

/// 采集消息窗口：枚举个人域会话（`msg:conv:personal:`），每会话按
/// `reverse + limit` 倒序扫描消息前缀（普通会话 `msg:item:personal:`，
/// 应用会话 `app:{pluginId}` 扫 `msg:app:personal:`），取窗口内
/// （时间下界 + 条数上限）的最新消息，逐条装成 [`PdsyncRecord`]
/// （**无 pmeta**——消息不走折叠/增量，见 §6.2）。
///
/// 倒序限量扫描即"每会话最近 N 条"（§6），无需全量载入再裁剪。
pub fn collect_message_window<S: StorageBackend>(
    storage: &S,
    window: &MessageWindow,
) -> crate::sync::SyncResult<Vec<PdsyncRecord>> {
    // max_age_ms 双保险钳制（from_hello 已钳，直接构造的窗口也安全）；
    // saturating_sub 防 now < max_age_ms 时下溢
    let lower_bound = crate::p2p::node::system_now_ms()
        .saturating_sub(window.max_age_ms.clamp(0, i64::MAX));
    let mut out = Vec::new();
    for (conv_key, _) in storage.scan(&ScanOptions::prefix("msg:conv:personal:"))? {
        let Some(conv_id) = conv_key.strip_prefix("msg:conv:personal:") else {
            continue;
        };
        let msg_prefix = if let Some(plugin_id) =
            conv_id.strip_prefix(crate::message::APP_CONV_PREFIX)
        {
            crate::message::app_message_prefix("personal", plugin_id)
        } else {
            crate::message::message_prefix("personal", conv_id)
        };
        // 倒序取最新 max_per_conv 条（键序即时间序），返回降序（新→旧）
        let rows = storage.scan(&ScanOptions {
            prefix: msg_prefix,
            limit: Some(window.max_per_conv),
            reverse: true,
            ..Default::default()
        })?;
        // 按降序迭代：逐条收下，遇到窗口外旧消息即停（降序中其后更老）；
        // 收下的翻回升序再入 out，保持输出确定性
        let mut conv_records: Vec<PdsyncRecord> = Vec::new();
        for (key, raw) in rows {
            // createdAt 取自记录体（msg:item / msg:app 均为 camelCase 同名
            // 字段）；损坏记录跳过——不推 null 给对端
            let Ok(value) = serde_json::from_str::<Value>(&raw) else {
                continue;
            };
            let Some(created_at) = value.get("createdAt").and_then(Value::as_i64) else {
                continue;
            };
            if created_at < lower_bound {
                break;
            }
            conv_records.push(PdsyncRecord { key, value, meta: DocMeta::default() });
        }
        out.extend(conv_records.into_iter().rev());
    }
    Ok(out)
}

/// 落盘一条消息（pdsync 入站）：写 `msg:item` + `msg:byid` 索引（同一
/// batch 提交）；应用消息 `msg:app` 只写本体（本地 `append_app_message`
/// 亦不建 byid 索引）。
///
/// append-only 幂等：以 `msgId`（键）天然去重，重复推送覆盖无害。
/// **不写消息 pmeta**（见 §6.2），也不刷新 conv.updated_at / 不要求会话
/// 存在——窗口同步只做合并。
///
/// `recalled` 只增不减（§6.2）：本地已撤回时，对端窗口里的撤回前旧快照
/// 不得复活内容——保留 `recalled: true`，其余字段取对端版本。
pub fn apply_message_record<S: StorageBackend>(
    storage: &mut S,
    key: &str,
    value: &str,
) -> crate::sync::SyncResult<()> {
    let Some(conv_id) = message_conv_id(key) else {
        return Ok(());
    };
    // 应用消息（§20）：无 recalled 概念、无 byid 索引，幂等覆盖
    if key.starts_with("msg:app:") {
        if serde_json::from_str::<crate::message::AppMessageRecord>(value).is_err() {
            return Ok(()); // 解析失败静默跳过（窗口快照跨版本容错）
        }
        storage.put(key, value)?;
        return Ok(());
    }
    let Ok(msg) = serde_json::from_str::<crate::message::MessageRecord>(value) else {
        return Ok(()); // 解析失败静默跳过（窗口快照跨版本容错）
    };
    let idx = crate::message::types::message_id_index_key("personal", &conv_id, &msg.id);
    let mut raw = value.to_string();
    if !msg.recalled
        && let Some(existing) = storage.get(key)?
        && let Ok(old) = serde_json::from_str::<crate::message::MessageRecord>(&existing)
        && old.recalled
    {
        raw = serde_json::to_string(&crate::message::MessageRecord {
            recalled: true,
            ..msg
        })?;
    }
    storage.batch(vec![
        crate::storage::BatchOperation::put(key, raw),
        crate::storage::BatchOperation::put(&idx, key),
    ])?;
    Ok(())
}

// ── 批量切分 ───────────────────────────────────────────────────────

/// 按单批字节上限切分记录列表（近似：以每条序列化长度为累加单位）。
pub fn split_batches(
    records: Vec<PdsyncRecord>,
    max_batch_bytes: usize,
) -> Vec<Vec<PdsyncRecord>> {
    let mut batches: Vec<Vec<PdsyncRecord>> = Vec::new();
    let mut current: Vec<PdsyncRecord> = Vec::new();
    let mut current_bytes = 0usize;
    for record in records {
        let bytes = serde_json::to_string(&record.value)
            .map(|s| s.len() + record.key.len() + 64)
            .unwrap_or(record.key.len() + 128);
        if !current.is_empty() && current_bytes + bytes > max_batch_bytes {
            batches.push(std::mem::take(&mut current));
            current_bytes = 0;
        }
        current.push(record);
        current_bytes += bytes;
    }
    if !current.is_empty() {
        batches.push(current);
    }
    batches
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::MemoryStorage;
    use crate::sync::personal::{
        apply_personal_remote, delete_personal, is_tombstone, put_personal,
    };

    const NODE_A: &str = "node-a";
    const NODE_B: &str = "node-b";
    const FRIEND_PREFIX: &str = "ct:friend:";

    fn category_friend() -> &'static Category {
        CATEGORIES
            .iter()
            .find(|c| c.name == "ct:friend")
            .unwrap()
    }

    #[test]
    fn collect_folds_max_across_records() {
        let mut s = MemoryStorage::new();
        // A 写两条朋友，B 在其中一条上再改
        put_personal(&mut s, NODE_A, &format!("{FRIEND_PREFIX}a"), "1", 1000).unwrap();
        put_personal(&mut s, NODE_A, &format!("{FRIEND_PREFIX}b"), "2", 1000).unwrap();
        let meta_b = put_personal(&mut s, NODE_B, &format!("{FRIEND_PREFIX}b"), "3", 2000).unwrap();
        assert_eq!(meta_b.vv.get(NODE_B), Some(&1));

        let folded = collect_category_vv(&s, category_friend(), None).unwrap();
        // a: A:1；b: A:1+B:1 → 折叠 A:1, B:1
        assert_eq!(folded.get(NODE_A), Some(&1));
        assert_eq!(folded.get(NODE_B), Some(&1));
    }

    #[test]
    fn diff_detects_local_behind_ahead_equal() {
        // 本地 {A:1}，对端 {A:2} → 落后
        let local: VersionVector = [(NODE_A.to_string(), 1)].into_iter().collect();
        let remote: VersionVector = [(NODE_A.to_string(), 2)].into_iter().collect();
        assert!(matches!(
            diff_category(&local, &remote),
            DiffOutcome::LocalBehind { .. }
        ));

        // 本地 {A:2}，对端 {A:1} → 领先
        let remote_older: VersionVector = [(NODE_A.to_string(), 1)].into_iter().collect();
        assert!(matches!(
            diff_category(&remote, &remote_older),
            DiffOutcome::LocalAhead
        ));

        // 相等 → Equal
        assert!(matches!(
            diff_category(&remote, &remote),
            DiffOutcome::Equal
        ));
    }

    #[test]
    fn collect_incremental_filters_by_known_vv() {
        let mut s = MemoryStorage::new();
        // A 写两条，B 在其一上再改
        // 记录本体需为合法 JSON（损坏记录会被跳过不推，防 null 覆盖对端）
        put_personal(&mut s, NODE_A, &format!("{FRIEND_PREFIX}a"), r#""A1""#, 1000).unwrap();
        put_personal(&mut s, NODE_A, &format!("{FRIEND_PREFIX}b"), r#""B1""#, 1000).unwrap();
        put_personal(&mut s, NODE_B, &format!("{FRIEND_PREFIX}b"), r#""B2""#, 2000).unwrap();

        // knownVv = {A:1, B:1}（对端已齐全）→ 无增量
        let known_full: VersionVector =
            [(NODE_A.to_string(), 1), (NODE_B.to_string(), 1)].into_iter().collect();
        let inc = collect_incremental(&s, category_friend(), &known_full, None).unwrap();
        assert!(inc.is_empty());

        // knownVv = {A:1}（对端缺 B 对 b 的更新）→ 只推 b（B:2 所在）
        let known_a: VersionVector = [(NODE_A.to_string(), 1)].into_iter().collect();
        let inc = collect_incremental(&s, category_friend(), &known_a, None).unwrap();
        assert_eq!(inc.len(), 1);
        assert_eq!(inc[0].key, format!("{FRIEND_PREFIX}b"));
        // b 的 meta vv 含 B 分量（相对 known_a 是 Remote）
        assert_eq!(inc[0].meta.vv.get(NODE_B), Some(&1));

        // knownVv 空 → 全部增量（两条都要）
        let inc = collect_incremental(&s, category_friend(), &VersionVector::new(), None).unwrap();
        assert_eq!(inc.len(), 2);
    }

    #[test]
    fn apply_data_is_idempotent() {
        let mut a = MemoryStorage::new();
        let mut b = MemoryStorage::new();

        put_personal(&mut a, NODE_A, &format!("{FRIEND_PREFIX}a"), "A1", 1000).unwrap();
        let meta = get_personal_meta(&a, &format!("{FRIEND_PREFIX}a")).unwrap().unwrap();

        // A 发 data 给 B：B 采纳
        let rec = PdsyncRecord {
            key: format!("{FRIEND_PREFIX}a"),
            value: json!("A1"),
            meta: meta.clone(),
        };
        let r = apply_personal_remote(&mut b, &rec.key, &rec.value.to_string(), &rec.meta).unwrap();
        assert_eq!(r, crate::sync::personal::ApplyResult::Applied);

        // 重放同一 data：Equal，幂等
        let r = apply_personal_remote(&mut b, &rec.key, &rec.value.to_string(), &rec.meta).unwrap();
        assert_eq!(r, crate::sync::personal::ApplyResult::Equal);
    }

    #[test]
    fn build_parse_roundtrip_hello() {
        let mut s = MemoryStorage::new();
        put_personal(&mut s, NODE_A, &format!("{FRIEND_PREFIX}a"), "1", 1000).unwrap();

        let hello = build_hello(&s, 2_592_000_000, 500, "eager", None).unwrap();
        let cats = parse_hello_categories(&hello);
        let friend_vv = cats.get("ct:friend").unwrap();
        assert_eq!(friend_vv.get(NODE_A), Some(&1));
        // 其余 category 存在（空 vv）
        for c in CATEGORIES {
            assert!(cats.contains_key(c.name), "category {} 缺失", c.name);
        }
    }

    #[test]
    fn build_parse_roundtrip_need_and_data() {
        let mut s = MemoryStorage::new();
        put_personal(&mut s, NODE_A, &format!("{FRIEND_PREFIX}a"), r#""v""#, 1000)
            .unwrap();
        let known: VersionVector = VersionVector::new();
        let inc = collect_incremental(&s, category_friend(), &known, None).unwrap();

        let need = build_need("ct:friend", &known);
        let (cat, parsed_vv) = parse_need(&need).unwrap();
        assert_eq!(cat, "ct:friend");
        assert!(parsed_vv.is_empty());

        let data = build_data_batch("ct:friend", &inc, 0, 1);
        let (cat2, records) = parse_data(&data).unwrap();
        assert_eq!(cat2, "ct:friend");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].key, format!("{FRIEND_PREFIX}a"));
    }

    #[test]
    fn split_batches_respects_limit() {
        let mut s = MemoryStorage::new();
        for i in 0..10 {
            let key = format!("{FRIEND_PREFIX}{i}");
            let val = format!("\"user-{i}-{}\"", "x".repeat(100));
            put_personal(&mut s, NODE_A, &key, &val, 1000).unwrap();
        }
        let inc = collect_incremental(&s, category_friend(), &VersionVector::new(), None).unwrap();
        let batches = split_batches(inc, 300);
        assert!(batches.len() > 1, "应切分为多批，实际 {}", batches.len());
        // 所有记录都覆盖到
        let total: usize = batches.iter().map(|b| b.len()).sum();
        assert_eq!(total, 10);
    }

    /// 端到端：两台设备（node-a / node-b）各自写入互不知情的数据，
    /// 通过 hello→need→data 三信封收敛，最终双方记录一致。
    ///
    /// 模拟协议：
    /// 1. A 发 hello（折叠 vv）给 B；
    /// 2. B 比对 → B 落后于 A 的类别发 need 回 A；
    /// 3. A 收到 need → 回 data；
    /// 4. B 应用 data → 双方一致。
    ///
    /// `exclude`：对称排除键（[`self_friend_key`] 自记录排除的端到端验证用，
    /// 其余测试传 `None`）。
    fn exchange(
        sender: &MemoryStorage,
        receiver: &mut MemoryStorage,
        hello: &Value,
        exclude: Option<&str>,
    ) -> Vec<Value> {
        // receiver 处理 hello：产生 need/data 出站 body
        let remote_cats = parse_hello_categories(hello);
        let mut out = Vec::new();
        for category in CATEGORIES {
            let local_vv = collect_category_vv(receiver, category, exclude).unwrap_or_default();
            let remote_vv = remote_cats.get(category.name).cloned().unwrap_or_default();
            match diff_category(&local_vv, &remote_vv) {
                DiffOutcome::LocalBehind { local_vv } => {
                    out.push(build_need(category.name, &local_vv));
                }
                DiffOutcome::LocalAhead => {
                    if let Ok(records) =
                        collect_incremental(receiver, category, &remote_vv, exclude)
                    {
                        let batches = split_batches(records, 4096);
                        let total = batches.len();
                        for (i, b) in batches.into_iter().enumerate() {
                            out.push(build_data_batch(category.name, &b, i, total));
                        }
                    }
                }
                DiffOutcome::Concurrent => {
                    // 双向：推本机缺的 + 请求对端缺的
                    if let Ok(records) =
                        collect_incremental(receiver, category, &remote_vv, exclude)
                    {
                        let batches = split_batches(records, 4096);
                        let total = batches.len();
                        for (i, b) in batches.into_iter().enumerate() {
                            out.push(build_data_batch(category.name, &b, i, total));
                        }
                    }
                    out.push(build_need(category.name, &local_vv));
                }
                DiffOutcome::Equal => {}
            }
        }
        // 处理 need：sender 侧采集并回 data（在真实链路由 sender 处理）
        let mut responses = Vec::new();
        for body in out {
            if let Some((cat_name, known_vv)) = parse_need(&body) {
                let cat = category_by_name(&cat_name).unwrap();
                let records = collect_incremental(sender, cat, &known_vv, exclude).unwrap();
                let batches = split_batches(records, 4096);
                let total = batches.len();
                for (i, b) in batches.into_iter().enumerate() {
                    responses.push(build_data_batch(&cat_name, &b, i, total));
                }
            }
        }
        responses
    }

    #[test]
    fn two_device_exchange_converges() {
        let mut a = MemoryStorage::new();
        let mut b = MemoryStorage::new();

        // A 写 3 条朋友，B 写 1 条不同的朋友
        for i in 0..3 {
            put_personal(
                &mut a,
                NODE_A,
                &format!("{FRIEND_PREFIX}a{i}"),
                &format!(r#""friend-a{i}""#),
                1000,
            )
            .unwrap();
        }
        put_personal(
            &mut b,
            NODE_B,
            &format!("{FRIEND_PREFIX}b0"),
            r#""friend-b0""#,
            2000,
        )
        .unwrap();

        // A → B：A 领先（B 缺 A 的 3 条），B 发 need，A 回 data，B 应用
        let hello_a = build_hello(&a, 2_592_000_000, 500, "eager", None).unwrap();
        let responses = exchange(&a, &mut b, &hello_a, None);
        for data in &responses {
            let (_, records) = parse_data(data).unwrap();
            for r in records {
                let _ = apply_personal_remote(&mut b, &r.key, &r.value.to_string(), &r.meta).unwrap();
            }
        }
        // B 现在应有 a0,a1,a2（来自 A）+ b0（自己）
        assert_eq!(collect_category_vv(&b, category_friend(), None).unwrap().get(NODE_A), Some(&1));
        assert_eq!(collect_category_vv(&b, category_friend(), None).unwrap().get(NODE_B), Some(&1));

        // 反向 B → A：A 缺 b0，B 领先，A 发 need，B 回 data，A 应用
        let hello_b = build_hello(&b, 2_592_000_000, 500, "eager", None).unwrap();
        let responses_b = exchange(&b, &mut a, &hello_b, None);
        for data in &responses_b {
            let (_, records) = parse_data(data).unwrap();
            for r in records {
                let _ = apply_personal_remote(&mut a, &r.key, &r.value.to_string(), &r.meta).unwrap();
            }
        }
        // A 也有 b0 了
        assert_eq!(collect_category_vv(&a, category_friend(), None).unwrap().get(NODE_B), Some(&1));

        // 收敛后再互发 hello → 均 Equal，无新响应
        let hello_a2 = build_hello(&a, 2_592_000_000, 500, "eager", None).unwrap();
        let responses_a2 = exchange(&a, &mut b, &hello_a2, None);
        assert!(responses_a2.is_empty(), "收敛后不应有 need/data");
        let hello_b2 = build_hello(&b, 2_592_000_000, 500, "eager", None).unwrap();
        let responses_b2 = exchange(&b, &mut a, &hello_b2, None);
        assert!(responses_b2.is_empty(), "收敛后不应有 need/data");
    }

    /// P2：个人资料 `profile:self` + 会话元数据 `msg:conv:personal:` 作为独立
    /// category 折叠，且双向交换收敛（A 改资料 + 置顶会话，B 改昵称 + 草稿）。
    #[test]
    fn two_device_exchange_converges_profile_and_conv() {
        let mut a = MemoryStorage::new();
        let mut b = MemoryStorage::new();

        let profile_key = "profile:self";
        let conv_key = "msg:conv:personal:c1";
        // A：昵称 + 会话置顶
        put_personal(
            &mut a,
            NODE_A,
            profile_key,
            r#"{"nickname":"甲","avatar":"","gender":"","region":"","signature":""}"#,
            1000,
        )
        .unwrap();
        put_personal(
            &mut a,
            NODE_A,
            conv_key,
            r#"{"id":"c1","kind":"Direct","title":"peer","peerRootId":"peer","peer":null,"unreadCount":0,"pinnedAt":1000,"muted":false,"draft":"","updatedAt":0,"metaUpdatedAt":1000}"#,
            1000,
        )
        .unwrap();
        // B：昵称 + 会话草稿
        put_personal(
            &mut b,
            NODE_B,
            profile_key,
            r#"{"nickname":"乙","avatar":"","gender":"","region":"","signature":""}"#,
            2000,
        )
        .unwrap();
        put_personal(
            &mut b,
            NODE_B,
            conv_key,
            r#"{"id":"c1","kind":"Direct","title":"peer","peerRootId":"peer","peer":null,"unreadCount":0,"pinnedAt":0,"muted":false,"draft":"草稿","updatedAt":0,"metaUpdatedAt":2000}"#,
            2000,
        )
        .unwrap();

        // A → B 交换：B 落后于 A 的 profile（A 先写），并发于 conv（各自改不同
        // 字段）。双向交换后双方各取所需。
        let hello_a = build_hello(&a, 2_592_000_000, 500, "eager", None).unwrap();
        let responses = exchange(&a, &mut b, &hello_a, None);
        for data in responses {
            let (_, records) = parse_data(&data).unwrap();
            for r in records {
                let _ = apply_personal_remote(&mut b, &r.key, &r.value.to_string(), &r.meta).unwrap();
            }
        }
        // B → A 反向
        let hello_b = build_hello(&b, 2_592_000_000, 500, "eager", None).unwrap();
        let responses_b = exchange(&b, &mut a, &hello_b, None);
        for data in responses_b {
            let (_, records) = parse_data(&data).unwrap();
            for r in records {
                let _ = apply_personal_remote(&mut a, &r.key, &r.value.to_string(), &r.meta).unwrap();
            }
        }

        // 单条 profile:self / msg:conv 是"单记录 category"——并发写冲突时
        // 只有 LWW 胜者保留（这里 B 后写 ts 更大 → B 胜），双方收敛到同一
        // 胜者，vv 只含胜者分量（B）。这是 LWW 的确定性收敛，非数据丢失。
        for s in [&a, &b] {
            // profile：B 胜（B 昵称 "乙"）
            let prof_vv = collect_category_vv(s, category_named("profile:self"), None).unwrap();
            assert_eq!(prof_vv.get(NODE_A), None, "A 的 profile 编辑被 LWW 丢弃");
            assert_eq!(prof_vv.get(NODE_B), Some(&1));
            // conv：B 胜（B 草稿，ts 更大）
            let conv_vv = collect_category_vv(s, category_named("msg:conv"), None).unwrap();
            assert_eq!(conv_vv.get(NODE_A), None, "A 的 conv 编辑被 LWW 丢弃");
            assert_eq!(conv_vv.get(NODE_B), Some(&1));
        }
        // 双方实际数据一致：profile 昵称 = 乙，conv 草稿 = "草稿"
        let profile_raw = get_personal_meta(&a, profile_key).unwrap().unwrap();
        assert_eq!(profile_raw.vv.get(NODE_B), Some(&1));
        // 收敛后再互发 hello → 无新响应
        let hello_a2 = build_hello(&a, 2_592_000_000, 500, "eager", None).unwrap();
        assert!(exchange(&a, &mut b, &hello_a2, None).is_empty(), "profile/conv 收敛后无增量");
    }

    // ── P4 消息窗口 ─────────────────────────────────────────────────

    /// 构造一条消息记录 JSON（字段对齐 `MessageRecord` 的 camelCase；
    /// `type` 用小写，因 `MessageType` 以 lowercase 序列化）。
    fn message_json(id: &str, created_at: i64, recalled: bool) -> String {
        format!(
            r#"{{"id":"{id}","senderId":"u1","senderName":"u1","type":"text","content":"{id}","createdAt":{created_at},"status":null,"recalled":{recalled},"read":false}}"#
        )
    }

    #[test]
    fn message_conv_id_parses_simple_and_nested() {
        let k1 = crate::message::types::message_key("personal", "peer1", 1234567890123, "m1");
        assert_eq!(message_conv_id(&k1).unwrap(), "peer1");
        // convId 含 `:`（应用会话 app:pluginId）
        let k2 = crate::message::types::message_key("personal", "app:plug", 1234567890123, "m1");
        assert_eq!(message_conv_id(&k2).unwrap(), "app:plug");
    }

    #[test]
    fn collect_message_window_clips_by_count_and_age() {
        let mut s = MemoryStorage::new();
        let now = crate::p2p::node::system_now_ms();
        // 采集按会话枚举：conv 记录是消息归属的入口
        s.put("msg:conv:personal:c1", "{}").unwrap();
        // 5 条消息，时间从旧到新
        for i in 0..5 {
            let key = crate::message::types::message_key(
                "personal",
                "c1",
                now - 1000 * (5 - i as i64),
                &format!("m{i}"),
            );
            let val = message_json(&format!("m{i}"), now - 1000 * (5 - i as i64), false);
            s.put(&key, &val).unwrap();
        }
        // 窗口：每 conv 3 条，时间全收 → 取最新 3 条（m2,m3,m4）
        let w = MessageWindow { max_per_conv: 3, max_age_ms: i64::MAX };
        let recs = collect_message_window(&s, &w).unwrap();
        assert_eq!(recs.len(), 3);
        // 最新 3 条是 m2,m3,m4
        for r in &recs {
            let last_seg = r.key.rsplit(':').next().unwrap();
            assert!(
                matches!(last_seg, "m2" | "m3" | "m4"),
                "窗口应只含最新 3 条，实为 {last_seg}"
            );
        }
    }

    /// 时间下界裁剪：同一会话部分消息在窗口外（更老）、部分在窗口内——
    /// 窗口外旧消息裁掉、窗口内新消息必须全部保留（回归：迭代方向与
    /// break 语义错配会把窗口内新消息一并丢弃）。
    #[test]
    fn collect_message_window_age_cutoff_keeps_in_window_messages() {
        let mut s = MemoryStorage::new();
        let now = crate::p2p::node::system_now_ms();
        s.put("msg:conv:personal:c1", "{}").unwrap();
        let hour = 3_600_000i64;
        // m0=-3h, m1=-2h 在 1h 窗口外；m2=-30m, m3=-10m, m4=now 在窗口内
        let offsets = [-3 * hour, -2 * hour, -hour / 2, -hour / 6, 0];
        for (i, off) in offsets.iter().enumerate() {
            let key =
                crate::message::types::message_key("personal", "c1", now + off, &format!("m{i}"));
            s.put(&key, &message_json(&format!("m{i}"), now + off, false))
                .unwrap();
        }
        let w = MessageWindow { max_per_conv: 100, max_age_ms: hour };
        let recs = collect_message_window(&s, &w).unwrap();
        let ids: Vec<&str> = recs
            .iter()
            .map(|r| r.key.rsplit(':').next().unwrap())
            .collect();
        assert_eq!(ids, ["m2", "m3", "m4"], "窗口外裁掉、窗口内全保留");
    }

    #[test]
    fn apply_message_record_writes_item_and_byid() {
        let mut s = MemoryStorage::new();
        let now = 1_000_000_000;
        let key = crate::message::types::message_key("personal", "c1", now, "m1");
        let val = message_json("m1", now, false);
        apply_message_record(&mut s, &key, &val).unwrap();
        // 消息本体 + byid 索引
        assert!(s.get(&key).unwrap().is_some());
        let idx = crate::message::types::message_id_index_key("personal", "c1", "m1");
        assert_eq!(s.get(&idx).unwrap().unwrap(), key);
    }

    #[test]
    fn apply_message_record_overwrites_on_recall() {
        let mut s = MemoryStorage::new();
        let now = 1_000_000_000;
        let key = crate::message::types::message_key("personal", "c1", now, "m1");
        // 先落普通消息
        apply_message_record(&mut s, &key, &message_json("m1", now, false)).unwrap();
        let before: crate::message::MessageRecord =
            serde_json::from_str(&s.get(&key).unwrap().unwrap()).unwrap();
        assert!(!before.recalled);
        // 撤回传播：覆盖为 recalled=true
        apply_message_record(&mut s, &key, &message_json("m1", now, true)).unwrap();
        let after: crate::message::MessageRecord =
            serde_json::from_str(&s.get(&key).unwrap().unwrap()).unwrap();
        assert!(after.recalled);
    }

    /// recalled 只增不减（§6.2）：本地已撤回，对端窗口里的撤回前旧快照
    /// （recalled=false）不得复活内容。
    #[test]
    fn apply_message_record_recall_is_sticky() {
        let mut s = MemoryStorage::new();
        let now = 1_000_000_000;
        let key = crate::message::types::message_key("personal", "c1", now, "m1");
        // 先落撤回后的记录
        apply_message_record(&mut s, &key, &message_json("m1", now, true)).unwrap();
        // 撤回前旧快照后到达（窗口推送乱序）→ recalled 保持 true
        apply_message_record(&mut s, &key, &message_json("m1", now, false)).unwrap();
        let after: crate::message::MessageRecord =
            serde_json::from_str(&s.get(&key).unwrap().unwrap()).unwrap();
        assert!(after.recalled, "recalled 只增不减，旧快照不得回退");
        // byid 索引仍指向该消息
        let idx = crate::message::types::message_id_index_key("personal", "c1", "m1");
        assert_eq!(s.get(&idx).unwrap().unwrap(), key);
    }

    // ── msg:app 窗口同步（§6.2：与 msg:item 同窗口）─────────────────

    /// 构造一条应用消息记录 JSON（字段对齐 `AppMessageRecord` 的 camelCase）。
    fn app_message_json(id: &str, plugin_id: &str, created_at: i64) -> String {
        format!(
            r#"{{"id":"{id}","pluginId":"{plugin_id}","summary":"s","payload":{{"summary":"s"}},"createdAt":{created_at},"status":"local"}}"#
        )
    }

    #[test]
    fn message_conv_id_parses_app_key() {
        let k = crate::message::types::app_message_key("personal", "plug", 1234567890123, "m1");
        assert_eq!(message_conv_id(&k).unwrap(), "app:plug");
    }

    /// msg:app 采集 + 接收 roundtrip：应用消息随窗口推出并在对端落盘，
    /// 不建 byid 索引（与本地 `append_app_message` 一致）。
    #[test]
    fn app_message_window_collect_apply_roundtrip() {
        let mut a = MemoryStorage::new();
        let mut b = MemoryStorage::new();
        let now = crate::p2p::node::system_now_ms();
        // A：应用会话 + 一条应用消息
        a.put("msg:conv:personal:app:plug", "{}").unwrap();
        let key = crate::message::types::app_message_key("personal", "plug", now, "am1");
        a.put(&key, &app_message_json("am1", "plug", now)).unwrap();

        let w = MessageWindow { max_per_conv: 100, max_age_ms: i64::MAX };
        let recs = collect_message_window(&a, &w).unwrap();
        assert_eq!(recs.len(), 1, "msg:app 应随窗口采集");
        assert_eq!(recs[0].key, key);

        for r in &recs {
            apply_message_record(&mut b, &r.key, &r.value.to_string()).unwrap();
        }
        let landed: crate::message::AppMessageRecord =
            serde_json::from_str(&b.get(&key).unwrap().unwrap()).unwrap();
        assert_eq!(landed.id, "am1");
        assert_eq!(landed.plugin_id, "plug");
        // 无 byid 索引（本地应用消息路径同样不建）
        let idx = crate::message::types::message_id_index_key("personal", "app:plug", "am1");
        assert!(b.get(&idx).unwrap().is_none());
    }

    /// msg:item 与 msg:app 混合窗口：同 conv 名前缀互不串扰。
    #[test]
    fn collect_message_window_covers_item_and_app() {
        let mut s = MemoryStorage::new();
        let now = crate::p2p::node::system_now_ms();
        s.put("msg:conv:personal:c1", "{}").unwrap();
        s.put("msg:conv:personal:app:plug", "{}").unwrap();
        let ik = crate::message::types::message_key("personal", "c1", now, "i1");
        s.put(&ik, &message_json("i1", now, false)).unwrap();
        let ak = crate::message::types::app_message_key("personal", "plug", now, "a1");
        s.put(&ak, &app_message_json("a1", "plug", now)).unwrap();

        let w = MessageWindow { max_per_conv: 100, max_age_ms: i64::MAX };
        let recs = collect_message_window(&s, &w).unwrap();
        let keys: Vec<&str> = recs.iter().map(|r| r.key.as_str()).collect();
        assert!(keys.contains(&ik.as_str()), "缺 msg:item 记录");
        assert!(keys.contains(&ak.as_str()), "缺 msg:app 记录");
    }

    /// 对端 hello 声明极端 maxAgeMs（负值 / i64::MIN）：钳制不 panic。
    #[test]
    fn message_window_extreme_max_age_clamped() {
        for raw in [-1i64, i64::MIN, 0, i64::MAX] {
            let body = json!({"msgWindow": {"maxAgeMs": raw, "maxPerConv": 10}});
            let w = MessageWindow::from_hello(&body);
            assert!(w.max_age_ms >= 0, "maxAgeMs={raw} 应钳到非负");
            // 采集路径（含 cutoff 减法）不得溢出 panic
            let mut s = MemoryStorage::new();
            s.put("msg:conv:personal:c1", "{}").unwrap();
            let now = crate::p2p::node::system_now_ms();
            let key = crate::message::types::message_key("personal", "c1", now, "m1");
            s.put(&key, &message_json("m1", now, false)).unwrap();
            let _ = collect_message_window(&s, &w).unwrap();
        }
        let body = json!({"msgWindow": {"maxAgeMs": i64::MIN}});
        assert_eq!(MessageWindow::from_hello(&body).max_age_ms, 0);
    }

    /// 损坏的本地记录（pmeta 完好、本体非 JSON）：跳过不推——不得以
    /// `null` 冒充空值覆盖对端好数据。
    #[test]
    fn collect_incremental_skips_corrupted_value() {
        let mut s = MemoryStorage::new();
        let good = format!("{FRIEND_PREFIX}good");
        let bad = format!("{FRIEND_PREFIX}bad");
        put_personal(&mut s, NODE_A, &good, r#""ok""#, 1000).unwrap();
        put_personal(&mut s, NODE_A, &bad, r#""will-corrupt""#, 1000).unwrap();
        // 直接写坏本体（pmeta 仍完好）
        s.put(&bad, "not-json{{{").unwrap();
        let inc = collect_incremental(&s, category_friend(), &VersionVector::new(), None).unwrap();
        assert_eq!(inc.len(), 1, "损坏记录应跳过");
        assert_eq!(inc[0].key, good);
    }

    /// 墓碑推送：删除的记录以 `{key, value: null, meta(tombstone=true)}`
    /// 进入增量；knownVv 已覆盖的墓碑不重推。
    #[test]
    fn collect_incremental_includes_tombstones() {
        let mut s = MemoryStorage::new();
        let live = format!("{FRIEND_PREFIX}live");
        let dead = format!("{FRIEND_PREFIX}dead");
        put_personal(&mut s, NODE_A, &live, r#""1""#, 1000).unwrap();
        put_personal(&mut s, NODE_A, &dead, r#""2""#, 1000).unwrap();
        delete_personal(&mut s, NODE_A, &dead, 2000).unwrap();

        let inc = collect_incremental(&s, category_friend(), &VersionVector::new(), None).unwrap();
        assert_eq!(inc.len(), 2);
        let tomb = inc.iter().find(|r| r.key == dead).expect("墓碑应在增量中");
        assert_eq!(tomb.value, Value::Null);
        assert_eq!(tomb.meta.tombstone, Some(true));
        let live_rec = inc.iter().find(|r| r.key == live).unwrap();
        assert_eq!(live_rec.value, json!("1"));

        // knownVv 已覆盖墓碑（A:2）→ 无增量
        let known: VersionVector = [(NODE_A.to_string(), 2)].into_iter().collect();
        let inc2 = collect_incremental(&s, category_friend(), &known, None).unwrap();
        assert!(inc2.is_empty(), "已知墓碑不应重推");
    }

    /// 端到端墓碑传播：A 删除 → hello/need/data 交换 → B 的记录被删 +
    /// 墓碑 pmeta 落地，且收敛后无增量。
    #[test]
    fn tombstone_delete_propagates_end_to_end() {
        let mut a = MemoryStorage::new();
        let mut b = MemoryStorage::new();
        let key = format!("{FRIEND_PREFIX}doomed");
        put_personal(&mut a, NODE_A, &key, r#""v1""#, 1000).unwrap();

        // 第一轮：A → B，B 获得记录
        let hello_a = build_hello(&a, 2_592_000_000, 500, "eager", None).unwrap();
        for data in exchange(&a, &mut b, &hello_a, None) {
            let (_, records) = parse_data(&data).unwrap();
            for r in records {
                let _ =
                    apply_personal_remote(&mut b, &r.key, &r.value.to_string(), &r.meta).unwrap();
            }
        }
        assert!(b.get(&key).unwrap().is_some(), "B 应先获得记录");

        // A 删除记录（写墓碑）
        delete_personal(&mut a, NODE_A, &key, 2000).unwrap();

        // 第二轮：墓碑随增量推给 B → B 删本体 + 落墓碑 pmeta
        let hello_a2 = build_hello(&a, 2_592_000_000, 500, "eager", None).unwrap();
        let responses = exchange(&a, &mut b, &hello_a2, None);
        assert!(!responses.is_empty(), "B 落后应触发 need→data");
        for data in responses {
            let (_, records) = parse_data(&data).unwrap();
            for r in records {
                let _ =
                    apply_personal_remote(&mut b, &r.key, &r.value.to_string(), &r.meta).unwrap();
            }
        }
        assert!(b.get(&key).unwrap().is_none(), "B 的记录应被墓碑删除");
        let pmeta = get_personal_meta(&b, &key).unwrap().unwrap();
        assert!(is_tombstone(&pmeta), "B 应持久化墓碑 pmeta");
        assert_eq!(pmeta.vv.get(NODE_A), Some(&2));

        // 收敛：双方折叠 vv 一致，再交换无增量
        let hello_b = build_hello(&b, 2_592_000_000, 500, "eager", None).unwrap();
        assert!(exchange(&b, &mut a, &hello_b, None).is_empty(), "墓碑收敛后无增量");
        let hello_a3 = build_hello(&a, 2_592_000_000, 500, "eager", None).unwrap();
        assert!(exchange(&a, &mut b, &hello_a3, None).is_empty(), "墓碑收敛后无增量");
    }

    #[test]
    fn two_device_message_window_converges() {
        let mut a = MemoryStorage::new();
        let mut b = MemoryStorage::new();
        let now = crate::p2p::node::system_now_ms();
        // 采集按会话枚举：双方都有 c1 的 conv 记录
        a.put("msg:conv:personal:c1", "{}").unwrap();
        b.put("msg:conv:personal:c1", "{}").unwrap();
        // A 有 2 条消息
        for i in 0..2 {
            let key = crate::message::types::message_key(
                "personal",
                "c1",
                now - 1000,
                &format!("a{i}"),
            );
            let val = message_json(&format!("a{i}"), now - 1000, false);
            a.put(&key, &val).unwrap();
        }
        // B 有 1 条不同消息
        let bk = crate::message::types::message_key("personal", "c1", now, "b0");
        b.put(&bk, &message_json("b0", now, false)).unwrap();

        // A → B：按 B 的窗口采集 A 的消息，apply 到 B
        let window = MessageWindow { max_per_conv: 100, max_age_ms: i64::MAX };
        let a_recs = collect_message_window(&a, &window).unwrap();
        for r in &a_recs {
            apply_message_record(&mut b, &r.key, &r.value.to_string()).unwrap();
        }
        // B → A：反向
        let b_recs = collect_message_window(&b, &window).unwrap();
        for r in &b_recs {
            apply_message_record(&mut a, &r.key, &r.value.to_string()).unwrap();
        }
        // 双方都有全部 3 条
        for (s, name) in [(&a, "a"), (&b, "b")] {
            for id in ["a0", "a1", "b0"] {
                let key = crate::message::types::message_key("personal", "c1", {
                    if id == "b0" { now } else { now - 1000 }
                }, id);
                assert!(
                    s.get(&key).unwrap().is_some(),
                    "{name} 缺少消息 {id}"
                );
            }
        }
    }

    // ── P5 组织数据 ─────────────────────────────────────────────────

    /// P5：组织记录型数据（`org:meta` / `ct:org` 成员 extra / `org:inv`）作为
    /// 独立 category 折叠，且双设备双向交换收敛。
    #[test]
    fn two_device_org_data_exchange_converges() {
        let mut a = MemoryStorage::new();
        let mut b = MemoryStorage::new();

        let org_meta_key = "org:meta:org1";
        let member_key = "ct:org:org1:extra:peer1";
        let invite_key = "org:inv:out:org1:peer1";

        // A：组织记录 + 成员资料 + 邀请
        put_personal(
            &mut a,
            NODE_A,
            org_meta_key,
            r#"{"orgId":"org1","name":"组织甲"}"#,
            1000,
        )
        .unwrap();
        put_personal(
            &mut a,
            NODE_A,
            member_key,
            r#"{"rootId":"peer1","remark":"A的备注"}"#,
            1000,
        )
        .unwrap();
        put_personal(
            &mut a,
            NODE_A,
            invite_key,
            r#"{"id":"inv1","status":"pending"}"#,
            1000,
        )
        .unwrap();
        // B：组织记录（同名，不同字段）
        put_personal(
            &mut b,
            NODE_B,
            org_meta_key,
            r#"{"orgId":"org1","name":"组织乙"}"#,
            2000,
        )
        .unwrap();

        // A → B 交换
        let hello_a = build_hello(&a, 2_592_000_000, 500, "eager", None).unwrap();
        let responses = exchange(&a, &mut b, &hello_a, None);
        for data in responses {
            let (_, records) = parse_data(&data).unwrap();
            for r in records {
                let _ = apply_personal_remote(&mut b, &r.key, &r.value.to_string(), &r.meta).unwrap();
            }
        }
        // B → A 反向
        let hello_b = build_hello(&b, 2_592_000_000, 500, "eager", None).unwrap();
        let responses_b = exchange(&b, &mut a, &hello_b, None);
        for data in responses_b {
            let (_, records) = parse_data(&data).unwrap();
            for r in records {
                let _ = apply_personal_remote(&mut a, &r.key, &r.value.to_string(), &r.meta).unwrap();
            }
        }

        // B 端应获得 A 的成员资料与邀请（org:meta 单记录并发冲突 → LWW B 胜）
        assert!(b.get(member_key).unwrap().is_some(), "B 缺成员资料");
        assert!(b.get(invite_key).unwrap().is_some(), "B 缺邀请记录");
        // A 端获得 B 的组织记录（org:meta 单记录并发，B ts 大 → B 胜）
        assert!(a.get(org_meta_key).unwrap().is_some());

        // org:meta / ct:org / org:inv 三 category 折叠收敛
        for s in [&a, &b] {
            let org_meta_vv = collect_category_vv(s, category_named("org:meta"), None).unwrap();
            assert_eq!(org_meta_vv.get(NODE_B), Some(&1), "org:meta LWW B 胜");
            let ct_org_vv = collect_category_vv(s, category_named("ct:org"), None).unwrap();
            assert_eq!(ct_org_vv.get(NODE_A), Some(&1), "ct:org 含 A 分量");
            let org_inv_vv = collect_category_vv(s, category_named("org:inv"), None).unwrap();
            assert_eq!(org_inv_vv.get(NODE_A), Some(&1), "org:inv 含 A 分量");
        }
    }

    /// P5：组织标签/分组树集合型数据作为单记录（整域）经 pdsync 同步。
    #[test]
    fn org_tags_tree_sync_as_single_record() {
        let mut a = MemoryStorage::new();
        let mut b = MemoryStorage::new();

        let tags_key = "ct:org:org1:tags";
        let tree_key = "ct:org:org1:tree";
        // A 写组织标签数组 + 分组树
        put_personal(&mut a, NODE_A, tags_key, r#"[{"id":"t1","name":"核心"}]"#, 1000).unwrap();
        put_personal(
            &mut a,
            NODE_A,
            tree_key,
            r#"[{"id":"g1","name":"研发","children":[]}]"#,
            1000,
        )
        .unwrap();

        // A → B
        let hello_a = build_hello(&a, 2_592_000_000, 500, "eager", None).unwrap();
        let responses = exchange(&a, &mut b, &hello_a, None);
        for data in responses {
            let (_, records) = parse_data(&data).unwrap();
            for r in records {
                let _ = apply_personal_remote(&mut b, &r.key, &r.value.to_string(), &r.meta).unwrap();
            }
        }
        // B 获得整域 tags/tree 记录
        assert!(b.get(tags_key).unwrap().is_some(), "B 缺组织标签");
        assert!(b.get(tree_key).unwrap().is_some(), "B 缺组织分组树");
        // 收敛后无增量
        let hello_a2 = build_hello(&a, 2_592_000_000, 500, "eager", None).unwrap();
        assert!(exchange(&a, &mut b, &hello_a2, None).is_empty(), "ct:org 收敛后无增量");
    }

    // ── 自 FriendRecord 排除（`ct:friend:{rootId}`，设备相对 peer 不可互灌）──

    /// 双设备同账号各自持有自记录（同键、peer 各指向对方设备）：带排除键的
    /// hello→need→data 交换后，两端自记录保持各自原值（不被互灌/LWW 收敛），
    /// 普通朋友记录照常同步，收敛后折叠 vv 无伪 diff。
    ///
    /// 回归：无排除时两设备自记录并发互推，LWW 收敛成一份 → 一端 peer
    /// 指向自己（自聊投递拨本机失败）。
    #[test]
    fn self_friend_record_excluded_from_sync() {
        let mut a = MemoryStorage::new();
        let mut b = MemoryStorage::new();
        let self_key = self_friend_key("root-self");
        let friend_key = format!("{FRIEND_PREFIX}other");
        // A：先写普通朋友（A:1）再写自记录（A:2）——排除后折叠应为 {A:1}，
        // 与不排除的 {A:2} 可区分
        put_personal(&mut a, NODE_A, &friend_key, r#""friend""#, 1000).unwrap();
        put_personal(
            &mut a,
            NODE_A,
            &self_key,
            r#"{"rootId":"root-self","peer":{"peerId":"peer-a","addresses":[]}}"#,
            1000,
        )
        .unwrap();
        // B：只有自记录（B:1，ts 更大——无排除时 LWW 会毒化 A）
        put_personal(
            &mut b,
            NODE_B,
            &self_key,
            r#"{"rootId":"root-self","peer":{"peerId":"peer-b","addresses":[]}}"#,
            2000,
        )
        .unwrap();

        // 折叠：排除键使自记录分量不进摘要
        let folded_a = collect_category_vv(&a, category_friend(), Some(&self_key)).unwrap();
        assert_eq!(folded_a.get(NODE_A), Some(&1), "排除后只折普通朋友记录");
        assert_eq!(folded_a.get(NODE_B), None);
        let folded_b = collect_category_vv(&b, category_friend(), Some(&self_key)).unwrap();
        assert!(folded_b.get(NODE_B).is_none(), "B 排除自记录后 ct:friend 为空");
        // 对照：不排除时自记录进折叠（B 侧 {B:1}）→ 与 A 并发互推
        let folded_b_raw = collect_category_vv(&b, category_friend(), None).unwrap();
        assert_eq!(folded_b_raw.get(NODE_B), Some(&1));

        // A → B、B → A 各一轮 hello→need→data（两侧同一排除键）
        let hello_a = build_hello(&a, 2_592_000_000, 500, "eager", Some(&self_key)).unwrap();
        for data in exchange(&a, &mut b, &hello_a, Some(&self_key)) {
            let (_, records) = parse_data(&data).unwrap();
            for r in records {
                let _ = apply_personal_remote(&mut b, &r.key, &r.value.to_string(), &r.meta).unwrap();
            }
        }
        let hello_b = build_hello(&b, 2_592_000_000, 500, "eager", Some(&self_key)).unwrap();
        for data in exchange(&b, &mut a, &hello_b, Some(&self_key)) {
            let (_, records) = parse_data(&data).unwrap();
            for r in records {
                let _ = apply_personal_remote(&mut a, &r.key, &r.value.to_string(), &r.meta).unwrap();
            }
        }

        // 两端自记录保持各自原值（peer 各指向对方设备，未被互灌）
        let a_self = a.get(&self_key).unwrap().expect("A 自记录仍在");
        assert!(a_self.contains("peer-a"), "A 自记录 peer 保持指向 A 设备: {a_self}");
        let b_self = b.get(&self_key).unwrap().expect("B 自记录仍在");
        assert!(b_self.contains("peer-b"), "B 自记录 peer 保持指向 B 设备: {b_self}");
        // 普通朋友记录照常同步到 B
        assert!(b.get(&friend_key).unwrap().is_some(), "普通朋友记录应同步");

        // 收敛：折叠 vv 一致，再交换无 need/data（无伪 diff）
        let hello_a2 = build_hello(&a, 2_592_000_000, 500, "eager", Some(&self_key)).unwrap();
        assert!(exchange(&a, &mut b, &hello_a2, Some(&self_key)).is_empty(), "A→B 收敛无增量");
        let hello_b2 = build_hello(&b, 2_592_000_000, 500, "eager", Some(&self_key)).unwrap();
        assert!(exchange(&b, &mut a, &hello_b2, Some(&self_key)).is_empty(), "B→A 收敛无增量");
    }

    /// 自记录墓碑排除：本机删自记录产生的墓碑不进增量、不进折叠——
    /// 不得把对端的自记录（它的设备相对数据）删掉。
    #[test]
    fn self_friend_tombstone_excluded_from_incremental() {
        let mut a = MemoryStorage::new();
        let self_key = self_friend_key("root-self");
        put_personal(&mut a, NODE_A, &self_key, r#""v""#, 1000).unwrap();
        delete_personal(&mut a, NODE_A, &self_key, 2000).unwrap();

        let inc = collect_incremental(
            &a,
            category_friend(),
            &VersionVector::new(),
            Some(&self_key),
        )
        .unwrap();
        assert!(inc.is_empty(), "自记录墓碑不参与增量推送");
        let folded = collect_category_vv(&a, category_friend(), Some(&self_key)).unwrap();
        assert!(folded.get(NODE_A).is_none(), "自记录墓碑不进折叠");
        // 对照：不排除时墓碑确实会在增量里（防测试本身失效）
        let inc_raw =
            collect_incremental(&a, category_friend(), &VersionVector::new(), None).unwrap();
        assert_eq!(inc_raw.len(), 1);
        assert_eq!(inc_raw[0].meta.tombstone, Some(true));
    }

    fn category_named(name: &str) -> &'static Category {
        CATEGORIES.iter().find(|c| c.name == name).unwrap()
    }
}

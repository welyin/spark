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
    CompareResult, DocMeta, VersionVector, compare_version_vectors, merge_version_vectors,
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
    Category {
        name: "ct:friend",
        prefixes: &["ct:friend:"],
    },
    Category {
        name: "ct:req",
        prefixes: &["ct:req:in:", "ct:req:out:"],
    },
    Category {
        name: "ct:tag",
        prefixes: &["ct:tag:"],
    },
    Category {
        name: "ct:group",
        prefixes: &["ct:group:"],
    },
    Category {
        name: "ct:blocked",
        prefixes: &["ct:blocked:"],
    },
    Category {
        name: "device",
        prefixes: &["device:"],
    },
    Category {
        name: "profile:self",
        prefixes: &["profile:self"],
    },
    Category {
        name: "msg:conv",
        prefixes: &["msg:conv:personal:"],
    },
    Category {
        name: "org:meta",
        prefixes: &["org:meta:"],
    },
    Category {
        name: "ct:org",
        prefixes: &["ct:org:"],
    },
    Category {
        name: "org:inv",
        prefixes: &["org:inv:in:", "org:inv:out:"],
    },
    // P6 插件声明式 API（personal scope）：声明记录先行（对端合入数据前必已
    // 知策略），数据记录随统一反熵。`ldoc:`（local scope）有意不在表内——
    // 永不离开本机。
    // 注意：`pdecl:` 含 local scope 集合的声明——声明可见性经 2026-08-31
    // 裁决为**有意设计**（同账号设备互可见装了哪些插件不是秘密，且
    // 「声明先行」要求合入方先读到声明；wiki plugin-data-api §声明校验 3）。
    Category {
        name: "pdecl",
        prefixes: &["pdecl:"],
    },
    Category {
        name: "pdoc",
        prefixes: &["pdoc:"],
    },
    // O4 encrypted 集合：orgkey 表（personal 域，32B 集合对称密钥）经 pdsync
    // 自设备扩散（同账号设备间），**永不进 orgsync 组织流量**（orgsync 数据
    // 白名单只放行 orgd:/org:coll:/org:acl:/存量组织键，见 inbound_dm/orgsync）。
    Category {
        name: "orgkey",
        prefixes: &["orgkey:"],
    },
    // M3 epoch 状态/包裹：epoch:state 与 ikey:* 记录均通过 pdsync 在自设备间扩散。
    // ikey: 在前：category_for_key 取首个命中，两前缀互不包含，顺序无吞并风险；
    // 保持历史顺序避免无关变更。
    Category {
        name: "epoch",
        prefixes: &["ikey:", "epoch:"],
    },
    // M3 口令校验器：V/ack 在自设备间扩散，明文豁免。
    Category {
        name: "pwv",
        prefixes: &["pwv:"],
    },
    Category {
        name: "pwack",
        prefixes: &["pwack:"],
    },
    // S2 dm_e2e 会话密钥表（personal 域，AES-256-GCM 会话密钥）经 pdsync 自设备
    // 扩散：同一 rootId 的多台设备共享同一份 1:1 会话密钥（离线密文在换钥后
    // 仍可由同账号其它设备解密）。历史密钥随记录体同步，不单列键。
    Category {
        name: "dm:e2e",
        prefixes: &["dm:e2e:key:"],
    },
    // S3 dm_offline 离线投递队列（personal 域 `dm:pending:`，含 feed 复用）经
    // pdsync 自设备扩散：同一 rootId 的多台设备互为补投备份——任一台在线设备
    // 上线都 flush 补投。组织 pending（`org:dm:pending:`）不进此表，走 org-sync
    // 网关同步（通道未就绪，见 dm_offline 模块注释）。
    Category {
        name: "dm:pending",
        prefixes: &["dm:pending:"],
    },
    // 插件市场索引（plugin-dist §8；mobile-leaf-mode：公告分发从 gossipsub 订阅
    // 改为经 pdsync 同步，leaf 不再订阅 PLUGIN_ANNOUNCE_TOPIC）。公告自含签名
    // +PoW、为公开数据，推送明文豁免（epoch classify_for_push）。计数键
    // `mkt:ann-count`（连字符）不匹配 `mkt:ann:` 前缀，天然排除出同步。
    Category {
        name: "mkt:ann",
        prefixes: &["mkt:ann:"],
    },
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

/// 即时 hello 补发请求键（裸存储旗标，非同步数据）：入站副作用写（epoch
/// ikey 补发 / 轮换派发）走裸存储、不 touch `last_local_write_ms`，变更观察
/// 循环（p2p_ops watchdog）看不到它——授予方挂此旗标，watchdog 消费后补发
/// `SelfHelloNow`，使对端下一轮反熵即拿到 ikey（否则要等本机下一次受管写
/// 或周期 hello，qr/口令收敛拖到分钟级）。消费即删，重启残留至多多发一次
/// hello，无害。
pub const HELLO_REQUEST_KEY: &str = "pdsync:hello_request";

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
    /// 删除日志序号（仅墓碑记录携带）：接收方据以推进 `dlog:seen:{peer}`
    /// 并在下轮 hello/need 回执 `dlogAck`。普通记录为 None（线上不携带）。
    pub dseq: Option<u64>,
}

/// 按 `knownVv` 采集 category 增量（need 的处理）：扫描该 category 全部
/// 记录，凡本地 pmeta 相对 knownVv 不是 `Local`/`Equal`（即 `Remote`/
/// `Concurrent`，或 knownVv 里没有该 nodeId）→ 纳入返回。
///
/// 天然筛出"对端缺的"，不必逐条比较两侧记录集合。
///
/// 墓碑（`tombstone: true`）以 `{key, value: null, meta, dseq}` 纳入推送——
/// 由删除日志驱动（`dlog_ack` 为对端已确认序号，推送 seq > ack 的条目）；
/// 接收方据 `meta.tombstone` 执行删除（§5.3/§10）并回执 dseq。
///
/// `exclude_key`：对称排除的记录键（[`self_friend_key`] 的自记录）——本体与
/// 墓碑两条路径都跳过（本机删自记录不应删掉对端的）。
pub fn collect_incremental<S: StorageBackend>(
    storage: &S,
    category: &Category,
    known_vv: &VersionVector,
    exclude_key: Option<&str>,
    dlog_ack: u64,
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
            // 墓碑增量由下方删除日志驱动
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
                    records.push(PdsyncRecord {
                        key,
                        value,
                        meta,
                        dseq: None,
                    });
                }
            }
        }
    }
    // 墓碑增量：删除日志驱动（见 collect_tombstones_after）
    records.extend(collect_tombstones_after(
        storage,
        category,
        exclude_key,
        dlog_ack,
    )?);
    Ok(records)
}

/// 采集未确认墓碑：删除日志中 `seq > dlog_ack`（对端 ACK 游标）且当前
/// pmeta 确为墓碑的条目。
///
/// 不走折叠 vv 比大小（折叠丢失 key 维度，墓碑会被误判"已覆盖"）；未确认
/// 的下轮重推（落库 vv 幂等）。仍校验 pmeta 当前确为墓碑——删除后又重建
/// 的记录，其历史日志条目已失效，不得误推删除。
///
/// 独立于 vv diff 推送：折叠 vv Equal 不代表对端收齐墓碑（Equal 可能由
/// 同 nodeId 其他记录的分量撑起），hello 处理在 Equal 分支也应调用本函数。
pub fn collect_tombstones_after<S: StorageBackend>(
    storage: &S,
    category: &Category,
    exclude_key: Option<&str>,
    dlog_ack: u64,
) -> crate::sync::SyncResult<Vec<PdsyncRecord>> {
    let mut records = Vec::new();
    for (seq, record_key) in crate::sync::dlog::entries_after(storage, dlog_ack)? {
        if !category.prefixes.iter().any(|p| record_key.starts_with(p)) {
            continue;
        }
        // 排除键（自记录）：设备相对数据不推（本机删自记录不应删掉对端的）
        if exclude_key == Some(record_key.as_str()) {
            continue;
        }
        let Ok(Some(meta)) = get_personal_meta(storage, &record_key) else {
            continue;
        };
        if !crate::sync::personal::is_tombstone(&meta) {
            continue;
        }
        log::info!(
            "[CT_SYNC] tombstone push | key={} dseq={} dlogAck={}",
            record_key,
            seq,
            dlog_ack,
        );
        records.push(PdsyncRecord {
            key: record_key,
            value: Value::Null,
            meta,
            dseq: Some(seq),
        });
    }
    Ok(records)
}

// ── 信封 body 构造 ─────────────────────────────────────────────────

/// 构造 `pdsync-hello` body：`{categories, msgWindow, attachmentPolicy,
/// deviceClass}`。
///
/// `exclude_key`：折叠摘要对称排除的记录键（[`self_friend_key`] 的自记录）。
pub fn build_hello<S: StorageBackend>(
    storage: &S,
    msg_window_max_age_ms: i64,
    msg_window_max_per_conv: usize,
    attachment_policy: &str,
    exclude_key: Option<&str>,
    last_msg_sync_at: Option<i64>,
) -> crate::sync::SyncResult<Value> {
    let effective_epoch = crate::epoch::get_effective(storage).unwrap_or(0);
    let mut hello = json!({
        "categories": collect_all_categories(storage, exclude_key)?,
        "msgWindow": {
            "maxAgeMs": msg_window_max_age_ms,
            "maxPerConv": msg_window_max_per_conv,
        },
        "attachmentPolicy": attachment_policy,
        // P6 驻留裁剪依据：本机设备类（pc/mobile），对端据此裁剪 pdoc 推送
        "deviceClass": local_device_class(),
        // M3：宣告本机生效 epoch；对端据此决定加密 epoch 上限。
        "epoch": effective_epoch,
    });
    if let Some(at) = last_msg_sync_at {
        hello["lastMsgSyncAt"] = json!(at);
    }
    Ok(hello)
}

/// 本机设备类（P6 `devices` 轴的判定口径）：Android/iOS = mobile，其余 = pc。
pub fn local_device_class() -> &'static str {
    match std::env::consts::OS {
        "android" | "ios" => "mobile",
        _ => "pc",
    }
}

/// 从 hello body 解析对端设备类（缺省/非法 → None，裁剪按不过滤兜底）。
pub fn parse_device_class(body: &Value) -> Option<String> {
    match body.get("deviceClass").and_then(Value::as_str) {
        Some("pc") => Some("pc".to_string()),
        Some("mobile") => Some("mobile".to_string()),
        _ => None,
    }
}

/// 对端设备类的持久化键（hello 收讫时记录，need 响应侧裁剪用——need body
/// 不携带设备类，以最近一次 hello 为准）。
pub fn remote_device_class_key(peer_id: &str) -> String {
    format!("pdsync:devclass:{peer_id}")
}

/// 对端 hello 宣告 epoch 的持久化键（发送侧据此选择加密 epoch 上限）。
pub fn remote_epoch_key(peer_id: &str) -> String {
    format!("pdsync:epoch:{peer_id}")
}

/// 解析 hello 的 `epoch` 字段；缺省/非法 → `None`（发送侧按 `min(effective,0)`
/// 即不加密兜底）。
pub fn parse_remote_epoch(body: &Value) -> Option<u64> {
    body.get("epoch").and_then(Value::as_u64)
}

/// 持久化对端 hello 宣告的生效 epoch。
pub fn set_remote_epoch<S: StorageBackend>(
    storage: &mut S,
    peer_id: &str,
    epoch: u64,
) -> crate::sync::SyncResult<()> {
    storage.put(&remote_epoch_key(peer_id), &epoch.to_string())?;
    Ok(())
}

/// 读取对端 hello 持久化的生效 epoch；缺失返回 0（发送侧按未初始化/不加密兜底）。
pub fn get_remote_epoch<S: StorageBackend>(storage: &S, peer_id: &str) -> u64 {
    storage
        .get(&remote_epoch_key(peer_id))
        .ok()
        .flatten()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

/// P6 驻留裁剪（发送侧）：按集合声明的 `devices` 轴过滤 `pdoc:` 记录——
/// `pc-only` 不推给手机、`mobile-only` 不推给 PC。声明缺失（本地数据不可能
/// 先于声明存在，属损坏态）时**跳过不推**（宁可少推不泄露驻留边界）。
/// 非 pdoc 记录原样保留；`remote_class` 未知（旧对端）不过滤。
pub fn trim_records_by_residency<S: StorageBackend>(
    storage: &S,
    records: Vec<PdsyncRecord>,
    remote_class: Option<&str>,
) -> Vec<PdsyncRecord> {
    let Some(class) = remote_class else {
        return records;
    };
    records
        .into_iter()
        .filter(|r| {
            if !r.key.starts_with("pdoc:") {
                return true;
            }
            let Some(decl_key) = crate::plugindata::decl_key_for_data_key(&r.key) else {
                return false;
            };
            let Ok(Some(raw)) = storage.get(&decl_key) else {
                return false;
            };
            match serde_json::from_str::<crate::plugindata::CollectionDeclaration>(&raw) {
                Ok(decl) => decl.allows_device(class),
                Err(_) => false,
            }
        })
        .collect()
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

/// 构造 `pdsync-need` body：`{category, knownVv, dlogAck}`。
///
/// `dlogAck`：我对发送方删除日志的已收序号（对方据此推送 seq > ack 的
/// 墓碑条目）。
pub fn build_need(category: &str, known_vv: &VersionVector, dlog_ack: u64) -> Value {
    json!({
        "category": category,
        "knownVv": known_vv,
        "dlogAck": dlog_ack,
    })
}

/// 解析 need body → (category, knownVv, dlogAck)（dlogAck 缺省 0）。
pub fn parse_need(body: &Value) -> Option<(String, VersionVector, u64)> {
    let category = body.get("category")?.as_str()?;
    let known_vv = serde_json::from_value(body.get("knownVv")?.clone()).ok()?;
    Some((
        category.to_string(),
        known_vv,
        crate::sync::dlog::parse_dlog_ack(body),
    ))
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
    // [诊断] 发送侧打点：category + 每条 key + vv，定位"循环推 data"的源头与内容。
    log::info!(
        "[PDSYNC-DATA-OUT] cat={category} batch={batch_seq}/{batch_total} n={} keys={:?}",
        records.len(),
        records
            .iter()
            .map(|r| format!("{}(vv={:?})", r.key, r.meta.vv))
            .collect::<Vec<_>>(),
    );
    let items: Vec<Value> = records
        .iter()
        .map(|r| {
            let mut item = json!({
                "key": r.key,
                "value": r.value,
                "meta": serde_json::to_value(&r.meta).unwrap_or(Value::Null),
            });
            // 墓碑记录携带删除日志序号（接收方回执 dlogAck 的依据）
            if let Some(dseq) = r.dseq {
                item["dseq"] = json!(dseq);
            }
            item
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
        let dseq = item.get("dseq").and_then(Value::as_u64);
        records.push(PdsyncRecord {
            key,
            value,
            meta,
            dseq,
        });
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
    /// 对端上次收到 pdsync-data 的时间：仅推送晚于此时间的新消息。
    /// `None` 表示首轮同步，推完整窗口。
    pub msg_sync_after: Option<i64>,
}

impl MessageWindow {
    /// 默认窗口（对齐文档 §6：500 条 / 30 天）。
    pub fn default_() -> Self {
        Self {
            max_per_conv: 500,
            max_age_ms: 30 * 24 * 3600 * 1000,
            msg_sync_after: None,
        }
    }

    /// 从 hello 的 `msgWindow` + `lastMsgSyncAt` 解析；缺失或无效回退默认。
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
        if let Some(at) = body.get("lastMsgSyncAt").and_then(Value::as_i64) {
            w.msg_sync_after = (at >= 0).then_some(at);
        }
        w
    }

    /// "全部"窗口：条数与时间都极大（设备声明收齐完整历史）。
    pub fn all() -> Self {
        Self {
            max_per_conv: usize::MAX,
            max_age_ms: i64::MAX,
            msg_sync_after: None,
        }
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
    let age_lower =
        crate::p2p::node::system_now_ms().saturating_sub(window.max_age_ms.clamp(0, i64::MAX));
    // msg_sync_after：对端声明上次同步时间，取 max 得到有效下界
    let lower_bound = std::cmp::max(age_lower, window.msg_sync_after.unwrap_or(0));
    let mut out = Vec::new();
    for (conv_key, _) in storage.scan(&ScanOptions::prefix("msg:conv:personal:"))? {
        let Some(conv_id) = conv_key.strip_prefix("msg:conv:personal:") else {
            continue;
        };
        let msg_prefix =
            if let Some(plugin_id) = conv_id.strip_prefix(crate::message::APP_CONV_PREFIX) {
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
            conv_records.push(PdsyncRecord {
                key,
                value,
                meta: DocMeta::default(),
                dseq: None,
            });
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

// ── lastMsgSyncAt 读写 ──────────────────────────────────────────────

/// pdsync 上次收到对端数据的时间戳（毫秒）。
const LAST_MSG_SYNC_KEY_PREFIX: &str = "pdsync:last_msg_at:";

fn last_msg_sync_key(root_id: &str) -> String {
    format!("{LAST_MSG_SYNC_KEY_PREFIX}{root_id}")
}

/// 读取对端 device 上次收到本机 pdsync-data 的时间。
/// 无记录返回 `None`（首轮同步推完整窗口）。
pub fn get_last_msg_sync_at<S: StorageBackend>(storage: &S, root_id: &str) -> Option<i64> {
    storage
        .get(&last_msg_sync_key(root_id))
        .ok()
        .flatten()
        .and_then(|v| v.trim().parse::<i64>().ok())
}

/// 记录本机本次收到对端 device pdsync-data 的时间。
pub fn set_last_msg_sync_at<S: StorageBackend>(
    storage: &mut S,
    root_id: &str,
    at_ms: i64,
) -> crate::sync::SyncResult<()> {
    storage.put(&last_msg_sync_key(root_id), &at_ms.to_string())?;
    Ok(())
}

// ── 批量切分 ───────────────────────────────────────────────────────

/// 按单批字节上限切分记录列表（近似：以每条序列化长度为累加单位）。
pub fn split_batches(records: Vec<PdsyncRecord>, max_batch_bytes: usize) -> Vec<Vec<PdsyncRecord>> {
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
mod tests;

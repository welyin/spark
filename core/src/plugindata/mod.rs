//! 插件声明式数据 API（P6）：策略是声明，不是调用参数。
//!
//! 设计定稿见 wiki `design/plugin-data-api.md` / `design/personal-data-sync.md` §11。
//! 本模块为**个人空间**落地（org scope 的 accounts/confidentiality 轴随组织
//! 同步架构 O2–O4 实现）：
//!
//! - 插件 `declareCollection` 一次（不可变更），之后 `save`/`del`/`get`/`query`
//!   不携带任何同步参数；
//! - **键前缀编码同步意图**：
//!   - 声明记录 `pdecl:{name}@v{version}`——pdsync 受管 category（声明先行，
//!     对端合入数据前必已知策略）；
//!   - `scope: "sync"` 数据 `pdoc:{name}@v{version}:{key}`——pdsync 受管，
//!     经 VersionedStorage 写入即自动版本化/墓碑/删除日志/触发同步；
//!   - `scope: "local"` 数据 `ldoc:{name}@v{version}:{key}`——不在任何
//!     pdsync category 内，原样透传永不离开本机；
//! - **代际**：同 name 不同 version = 独立命名空间（声明/数据/删除日志均隔离）；
//!   代际内策略冻结；框架不解析版本号语义，新旧按 `declared_at` 排序；
//! - **驻留裁剪**：`devices` 轴在 pdsync 推送侧按对端 hello 声明的
//!   `deviceClass` 过滤（pc-only 不上手机等），见
//!   [`crate::sync::pdsync::trim_records_by_residency`]。

use serde::{Deserialize, Serialize};

use crate::storage::{ScanOptions, StorageBackend};

/// 同步范围（插件视角只有"同步/不同步"；personal/org 由内核按运行空间处理——
/// 本模块当前仅承载 personal 语义）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Scope {
    /// 同步（缺省）。
    #[default]
    #[serde(rename = "sync")]
    Sync,
    /// 仅本机，不参与任何同步。
    #[serde(rename = "local")]
    Local,
}

/// 设备驻留轴（设备类按 OS 判定：Windows/macOS/Linux=pc，Android/iOS=mobile）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Devices {
    /// 所有设备全副本（缺省）。
    #[default]
    #[serde(rename = "all")]
    All,
    /// PC eager 全量做备份点，手机 lazy（记录照常同步，blob 按需——blob 随
    /// 后续切片落地；记录级语义当前等价 all）。
    #[serde(rename = "pc-backup")]
    PcBackup,
    /// 只驻留 PC 类设备（记录也不上手机）。
    #[serde(rename = "pc-only")]
    PcOnly,
    /// 只驻留手机类设备。
    #[serde(rename = "mobile-only")]
    MobileOnly,
}

/// 合并规则（三档封顶，不开放自定义合并函数）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum MergeRule {
    /// 逐条 LWW（缺省）。
    #[default]
    #[serde(rename = "lww-record")]
    LwwRecord,
    /// 仅追加：拒绝覆盖已存在 key、拒绝删除。
    #[serde(rename = "append-only")]
    AppendOnly,
    /// 整域单记录（key 恒为 [`WHOLE_KEY`]，整域 LWW）。
    #[serde(rename = "whole")]
    Whole,
}

/// whole 集合的唯一记录 key。
pub const WHOLE_KEY: &str = "__whole__";

/// 集合声明记录（持久化于 `pdecl:`，代际内不可变更）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollectionDeclaration {
    /// 集合名 `{pluginId}:{collection}`。
    pub name: String,
    /// 代际标签（字符串，建议 semver；框架不解析语义）。
    pub version: String,
    /// 同步范围。
    #[serde(default)]
    pub scope: Scope,
    /// 设备驻留轴。
    #[serde(default)]
    pub devices: Devices,
    /// 合并规则。
    #[serde(default)]
    pub merge: MergeRule,
    /// 声明时间（ms；代际新旧排序依据）。
    #[serde(rename = "declaredAt", default)]
    pub declared_at: i64,
}

impl CollectionDeclaration {
    /// 声明记录键 `pdecl:{name}@v{version}`。
    pub fn decl_key(&self) -> String {
        decl_key(&self.name, &self.version)
    }

    /// 数据记录键前缀（按 scope 分流受管/不受管命名空间）。
    pub fn data_prefix(&self) -> String {
        let stem = match self.scope {
            Scope::Sync => "pdoc",
            Scope::Local => "ldoc",
        };
        format!("{stem}:{}@v{}:", self.name, self.version)
    }

    /// 数据记录键（whole 集合 key 恒为 [`WHOLE_KEY`]）。
    pub fn data_key(&self, key: &str) -> String {
        let effective = match self.merge {
            MergeRule::Whole => WHOLE_KEY,
            _ => key,
        };
        format!("{}{effective}", self.data_prefix())
    }

    /// 该集合的记录是否允许驻留到指定设备类（`"pc"` / `"mobile"`）。
    pub fn allows_device(&self, device_class: &str) -> bool {
        match self.devices {
            Devices::All | Devices::PcBackup => true,
            Devices::PcOnly => device_class == "pc",
            Devices::MobileOnly => device_class == "mobile",
        }
    }
}

/// 声明记录键。
pub fn decl_key(name: &str, version: &str) -> String {
    format!("pdecl:{name}@v{version}")
}

/// 从 pdoc 数据键解析声明记录键（`pdoc:{name}@v{version}:{key}` →
/// `pdecl:{name}@v{version}`）。name 不含 `@`（声明校验保证），rfind 定位
/// 代际分隔符。
pub fn decl_key_for_data_key(data_key: &str) -> Option<String> {
    let rest = data_key.strip_prefix("pdoc:")?;
    let at = rest.rfind("@v")?;
    // 版本段 = "@v" 起到其后首个 ':'（数据键分隔符）为止
    let ver_end = rest[at..].find(':').map(|i| at + i).unwrap_or(rest.len());
    Some(format!("pdecl:{}", &rest[..ver_end]))
}

/// 模块错误。
#[derive(Debug, thiserror::Error)]
pub enum PlugindataError {
    /// 集合未声明（先读写后声明）。
    #[error("collection \"{0}\" is not declared")]
    NotDeclared(String),

    /// 代际内重复声明策略冲突（声明不可变更）。
    #[error("collection \"{name}\" version \"{version}\" is already declared with a different strategy and cannot be re-declared")]
    ConflictingDeclaration {
        /// 集合名。
        name: String,
        /// 代际。
        version: String,
    },

    /// 集合名非法（必须是 `{pluginId}:{collection}`，collection 段 `[A-Za-z0-9_-]+`，
    /// 不得含 `@`）。
    #[error("invalid collection name \"{0}\": expected \"{{pluginId}}:{{collection}}\", collection part allows [A-Za-z0-9_-], \"@\" is reserved")]
    InvalidName(String),

    /// name 的插件前缀与调用方插件 id 不一致。
    #[error("collection \"{name}\" does not belong to plugin \"{plugin_id}\"")]
    NamePrefixMismatch {
        /// 集合名。
        name: String,
        /// 调用方插件 id。
        plugin_id: String,
    },

    /// 代际标签非法（非空、`[0-9A-Za-z.-]`，不含 `:`/`@`）。
    #[error("invalid version \"{0}\": allows [0-9A-Za-z.-], must not contain ':' or '@'")]
    InvalidVersion(String),

    /// append-only 集合拒绝覆盖/删除。
    #[error("collection \"{0}\" is append-only: overwrite and delete are rejected")]
    AppendOnlyViolation(String),

    /// blob 存取错误。
    #[error("blob error: {0}")]
    Blob(String),

    /// 存储后端错误。
    #[error(transparent)]
    Storage(#[from] crate::storage::StorageError),

    /// JSON 序列化错误。
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

/// 模块 Result 别名。
pub type Result<T> = std::result::Result<T, PlugindataError>;

fn is_valid_collection_part(part: &str) -> bool {
    !part.is_empty()
        && part
            .bytes()
            .all(|b| matches!(b, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-'))
}

fn is_valid_version(version: &str) -> bool {
    !version.is_empty()
        && version.bytes().all(|b| {
            matches!(b, b'0'..=b'9' | b'A'..=b'Z' | b'a'..=b'z' | b'.' | b'-')
        })
}

/// 声明输入（host 层从插件载荷解析；`name`/`version` 之外的轴均有缺省）。
#[derive(Clone, Debug, Default)]
pub struct DeclareInput {
    /// 集合名 `{pluginId}:{collection}`。
    pub name: String,
    /// 代际标签（缺省 `"1"`）。
    pub version: Option<String>,
    /// 同步范围（缺省 sync）。
    pub scope: Option<Scope>,
    /// 设备驻留轴（缺省 all）。
    pub devices: Option<Devices>,
    /// 合并规则（缺省 lww-record）。
    pub merge: Option<MergeRule>,
}

/// 校验 name 形态与插件前缀归属。
fn validate_name(name: &str, plugin_id: &str) -> Result<()> {
    let Some((prefix, collection)) = name.rsplit_once(':') else {
        return Err(PlugindataError::InvalidName(name.to_string()));
    };
    if prefix.is_empty() || name.contains('@') || !is_valid_collection_part(collection) {
        return Err(PlugindataError::InvalidName(name.to_string()));
    }
    if prefix != plugin_id {
        return Err(PlugindataError::NamePrefixMismatch {
            name: name.to_string(),
            plugin_id: plugin_id.to_string(),
        });
    }
    Ok(())
}

/// 声明集合（幂等）：代际内重复声明策略一致 → 返回既有记录；冲突 → 报错。
/// 声明记录写 `pdecl:`（pdsync 受管）——经 VersionedStorage 句柄写入即随
/// 同步扩散（声明先行）。
pub fn declare<S: StorageBackend>(
    storage: &mut S,
    plugin_id: &str,
    input: DeclareInput,
    now_ms: i64,
) -> Result<CollectionDeclaration> {
    validate_name(&input.name, plugin_id)?;
    let version = input.version.unwrap_or_else(|| "1".to_string());
    if !is_valid_version(&version) {
        return Err(PlugindataError::InvalidVersion(version));
    }
    let decl = CollectionDeclaration {
        name: input.name,
        version,
        scope: input.scope.unwrap_or_default(),
        devices: input.devices.unwrap_or_default(),
        merge: input.merge.unwrap_or_default(),
        declared_at: now_ms,
    };
    if let Some(existing) = get_declaration(storage, &decl.name, &decl.version)? {
        let same = existing.scope == decl.scope
            && existing.devices == decl.devices
            && existing.merge == decl.merge;
        if same {
            return Ok(existing);
        }
        return Err(PlugindataError::ConflictingDeclaration {
            name: decl.name,
            version: decl.version,
        });
    }
    storage.put(&decl.decl_key(), &serde_json::to_string(&decl)?)?;
    Ok(decl)
}

/// 读指定代际的声明。
pub fn get_declaration<S: StorageBackend>(
    storage: &S,
    name: &str,
    version: &str,
) -> Result<Option<CollectionDeclaration>> {
    let Some(raw) = storage.get(&decl_key(name, version))? else {
        return Ok(None);
    };
    Ok(serde_json::from_str(&raw).ok())
}

/// 列出某集合名的全部代际（按声明时间升序）。
pub fn list_versions<S: StorageBackend>(
    storage: &S,
    name: &str,
) -> Result<Vec<CollectionDeclaration>> {
    let prefix = format!("pdecl:{name}@v");
    let mut out = Vec::new();
    for (_key, raw) in storage.scan(&ScanOptions::prefix(&prefix))? {
        if let Ok(decl) = serde_json::from_str::<CollectionDeclaration>(&raw)
            && decl.name == name
        {
            out.push(decl);
        }
    }
    out.sort_by(|a, b| a.declared_at.cmp(&b.declared_at).then(a.version.cmp(&b.version)));
    Ok(out)
}

/// 解析目标声明：version 指定 → 该代际；缺省 → 最新代际（声明时间最新者）。
/// 未声明 → [`PlugindataError::NotDeclared`]。
pub fn resolve<S: StorageBackend>(
    storage: &S,
    name: &str,
    version: Option<&str>,
) -> Result<CollectionDeclaration> {
    if let Some(v) = version {
        return get_declaration(storage, name, v)?
            .ok_or_else(|| PlugindataError::NotDeclared(format!("{name}@v{v}")));
    }
    list_versions(storage, name)?
        .into_iter()
        .last()
        .ok_or_else(|| PlugindataError::NotDeclared(name.to_string()))
}

/// 写记录（自动版本化由 VersionedStorage 句柄保证；whole 集合 key 归一为
/// [`WHOLE_KEY`]；append-only 拒绝覆盖已存在 key）。
pub fn save<S: StorageBackend>(storage: &mut S, decl: &CollectionDeclaration, key: &str, value: &str) -> Result<()> {
    let data_key = decl.data_key(key);
    if decl.merge == MergeRule::AppendOnly && storage.get(&data_key)?.is_some() {
        return Err(PlugindataError::AppendOnlyViolation(decl.name.clone()));
    }
    storage.put(&data_key, value)?;
    Ok(())
}

/// 删记录（append-only 拒绝；删除经中间件自动墓碑 + 删除日志，传播到复制组）。
pub fn del<S: StorageBackend>(storage: &mut S, decl: &CollectionDeclaration, key: &str) -> Result<()> {
    if decl.merge == MergeRule::AppendOnly {
        return Err(PlugindataError::AppendOnlyViolation(decl.name.clone()));
    }
    storage.delete(&decl.data_key(key))?;
    Ok(())
}

/// 读单条（whole 集合忽略 key）。
pub fn get<S: StorageBackend>(storage: &S, decl: &CollectionDeclaration, key: &str) -> Result<Option<String>> {
    storage.get(&decl.data_key(key)).map_err(Into::into)
}

/// 一页查询结果（key 为集合内相对键，已剥离 `pdoc:/ldoc:` 与代际前缀）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct QueryPage {
    /// 命中项（相对键 → 值 JSON 串）。
    pub items: Vec<(String, String)>,
    /// 下一页游标（上一页末条相对键；本页条数 == limit 时给出）。
    pub next_cursor: Option<String>,
}

/// 前缀扫描分页：limit 缺省 500、上限 2000；cursor 为上一页末条相对键。
pub fn query<S: StorageBackend>(
    storage: &S,
    decl: &CollectionDeclaration,
    prefix: Option<&str>,
    limit: Option<usize>,
    cursor: Option<&str>,
) -> Result<QueryPage> {
    let limit = limit.unwrap_or(500).clamp(1, 2000);
    let base = decl.data_prefix();
    let match_prefix = match prefix {
        Some(p) => format!("{base}{p}"),
        None => base.clone(),
    };
    // cursor 是相对键（含调用方 prefix 文本），下界从 base 拼
    let scan_from = match cursor {
        Some(c) => format!("{base}{c}\u{0}"),
        None => match_prefix.clone(),
    };
    let options = ScanOptions {
        prefix: match_prefix,
        start: Some(scan_from),
        end: None, // 缺省 = prefix + U+10FFFF
        limit: Some(limit),
        reverse: false,
    };
    let mut page = QueryPage::default();
    for (key, value) in storage.scan(&options)? {
        let relative = key[base.len()..].to_string();
        page.items.push((relative, value));
        if page.items.len() == limit {
            page.next_cursor = page.items.last().map(|(k, _)| k.clone());
            break;
        }
    }
    Ok(page)
}

/// 清理一个代际：声明记录 + 全部数据键（受管命名空间经中间件墓碑化传播）。
pub fn drop_version<S: StorageBackend>(storage: &mut S, decl: &CollectionDeclaration) -> Result<()> {
    let prefix = decl.data_prefix();
    let keys: Vec<String> = storage
        .scan(&ScanOptions::prefix(&prefix))?
        .into_iter()
        .map(|(k, _)| k)
        .collect();
    for key in keys {
        storage.delete(&key)?;
    }
    storage.delete(&decl.decl_key())?;
    Ok(())
}

pub mod blob;

#[cfg(test)]
mod tests;

//! 插件声明式数据 API（P6）：策略是声明，不是调用参数。
//!
//! 设计定稿见 wiki `design/plugin-data-api.md` / `design/personal-data-sync.md` §11。
//! O2a 起扩展 org scope 的 accounts/confidentiality 轴：
//!
//! - 插件 `declareCollection` 一次（不可变更），之后 `save`/`del`/`get`/`query`
//!   不携带任何同步参数；
//! - **键前缀编码同步意图**：
//!   - 声明记录 `pdecl:{name}@v{version}`——personal scope 集合；
//!   - 声明记录 `org:coll:{orgId}:{name}@v{version}`——org scope 集合
//!     （all-members 系统数据，声明先行）；
//!   - `scope: "sync"` personal 数据 `pdoc:{name}@v{version}:{key}`——pdsync 受管；
//!   - `scope: "sync"` org 数据 `orgd:{orgId}:{name}@v{version}:{key}`——
//!     orgsync 受管，经 VersionedStorage 写入即自动版本化/墓碑/删除日志/触发同步；
//!   - `scope: "local"` 数据 `ldoc:{name}@v{version}:{key}`——不在任何
//!     sync category 内，原样透传永不离开本机；
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

/// 账号驻留轴（O2a，org scope 专有）：集合同步到哪些账号。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Accounts {
    /// 仅数据账号全量驻留（缺省）；普通成员不驻留，按需查询。
    #[default]
    #[serde(rename = "data-accounts")]
    DataAccounts,
    /// 组织内所有成员账号全量驻留。
    #[serde(rename = "all-members")]
    AllMembers,
}

/// 保密轴（O2a，org scope 专有）：数据可见性模型。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Confidentiality {
    /// 明文过滤（缺省）：数据账号持明文，执行权限钩子。
    #[default]
    #[serde(rename = "filtered")]
    Filtered,
    /// 密文仓库：数据账号只存密文，不执行内容级钩子。
    #[serde(rename = "encrypted")]
    Encrypted,
}

/// 敏感轴（M3，personal scope 专有）：决定是否用 epoch 包裹加密。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Sensitivity {
    /// 普通（缺省）：明文同步。
    #[default]
    #[serde(rename = "normal")]
    Normal,
    /// 敏感：pdsync 推送前用当前 effective epoch 密钥加密。
    #[serde(rename = "sensitive")]
    Sensitive,
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

/// 集合声明记录（持久化于 `pdecl:` 或 `org:coll:`，代际内不可变更）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollectionDeclaration {
    /// 集合名 `{pluginId}:{collection}`。
    pub name: String,
    /// 代际标签（字符串，建议 semver；框架不解析语义）。
    pub version: String,
    /// 同步范围。
    #[serde(default)]
    pub scope: Scope,
    /// 运行空间（内核解析：personal/org；插件 API 不感知）。线上记录由此字段
    /// 区分 personal 声明（`pdecl:`）与 org 声明（`org:coll:`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space: Option<Space>,
    /// 账号驻留轴（O2a，org scope 专有；personal scope 忽略）。
    #[serde(default)]
    pub accounts: Accounts,
    /// 设备驻留轴。
    #[serde(default)]
    pub devices: Devices,
    /// 保密轴（O2a，org scope 专有；personal scope 忽略）。
    #[serde(default)]
    pub confidentiality: Confidentiality,
    /// 敏感轴（M3，personal scope 专有；org scope 忽略）。
    #[serde(default)]
    pub sensitivity: Sensitivity,
    /// 合并规则。
    #[serde(default)]
    pub merge: MergeRule,
    /// 声明时间（ms；代际新旧排序依据）。
    #[serde(rename = "declaredAt", default)]
    pub declared_at: i64,
    /// 声明者 rootId（org 声明记录线形用；personal scope 可选）。
    #[serde(
        rename = "declaredBy",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub declared_by: Option<String>,
    /// 声明时间戳（org 声明记录线形用；与 declared_at 语义重复但名称不同，
    /// 用于对齐 org-orgsync.md §20.2.1 线形）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ts: Option<i64>,
    /// 所属组织 id（org scope 声明由内核按运行空间填入；personal scope 为
    /// None）。用于 save/del/get/query 按空间路由到 `orgd:` 键。随声明记录
    /// 持久化（org 声明存于 `org:coll:{orgId}:`，同代际内 org_id 恒定）。
    #[serde(rename = "orgId", default, skip_serializing_if = "Option::is_none")]
    pub org_id: Option<String>,
}

/// 运行空间（由内核按插件运行空间解析后写入线上记录）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Space {
    /// 个人空间（声明键 `pdecl:`，数据键 `pdoc:` / `ldoc:`）。
    #[default]
    #[serde(rename = "personal")]
    Personal,
    /// 组织空间（声明键 `org:coll:{orgId}:`，数据键 `orgd:{orgId}:`）。
    #[serde(rename = "org")]
    Org,
}

impl CollectionDeclaration {
    /// 声明记录键（personal scope：`pdecl:{name}@v{version}`）。
    ///
    /// org scope 必须走 [`Self::org_decl_key`]——本方法不再回退猜测 org 键，
    /// 调用方在 org 路径上须显式传入 org_id。
    pub fn decl_key(&self) -> String {
        decl_key(&self.name, &self.version)
    }

    /// org scope 声明记录键 `org:coll:{orgId}:{name}@v{version}`。
    pub fn org_decl_key(org_id: &str, name: &str, version: &str) -> String {
        format!("org:coll:{org_id}:{name}@v{version}")
    }

    /// 数据记录键前缀（按 scope 分流受管/不受管命名空间）。
    pub fn data_prefix(&self) -> String {
        let stem = match self.scope {
            Scope::Sync => "pdoc",
            Scope::Local => "ldoc",
        };
        format!("{stem}:{}@v{}:", self.name, self.version)
    }

    /// org scope 数据记录键前缀 `orgd:{orgId}:{name}@v{version}:`。
    pub fn org_data_prefix(org_id: &str, name: &str, version: &str) -> String {
        format!("orgd:{org_id}:{name}@v{version}:")
    }

    /// org scope 数据记录键。
    pub fn org_data_key(
        org_id: &str,
        name: &str,
        version: &str,
        key: &str,
        merge: MergeRule,
    ) -> String {
        let effective = match merge {
            MergeRule::Whole => WHOLE_KEY,
            _ => key,
        };
        format!(
            "{}{effective}",
            Self::org_data_prefix(org_id, name, version)
        )
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

    /// 该集合是否敏感（M3 epoch 加密）。
    pub fn is_sensitive(&self) -> bool {
        self.sensitivity == Sensitivity::Sensitive
    }
}

/// 声明记录键（personal scope）。
pub fn decl_key(name: &str, version: &str) -> String {
    format!("pdecl:{name}@v{version}")
}

/// org scope 声明记录键 `org:coll:{orgId}:{name}@v{version}`。
pub fn org_decl_key(org_id: &str, name: &str, version: &str) -> String {
    format!("org:coll:{org_id}:{name}@v{version}")
}

/// org scope 数据记录键前缀 `orgd:{orgId}:{name}@v{version}:`。
pub fn org_data_prefix(org_id: &str, name: &str, version: &str) -> String {
    format!("orgd:{org_id}:{name}@v{version}:")
}

/// org scope 删除日志键前缀（按 (orgId, collection) 作用域）。
pub fn org_dlog_entry_prefix(org_id: &str, name: &str, version: &str) -> String {
    format!("dlog:org:{org_id}:{name}@v{version}:entry:")
}

/// org scope 删除日志序号键。
pub fn org_dlog_seq_key(org_id: &str, name: &str, version: &str) -> String {
    format!("dlog:org:{org_id}:{name}@v{version}:seq")
}

/// org scope 删除日志已确认水位键
/// （`dlog:org:{orgId}:{name}@v{version}:wm:{rootId}:{peerId}`）。
///
/// dlog seq 空间按设备维护，水位不可按账号混用——同 rootId 的每台设备各自
/// 确认，故键段含 peerId（架构裁决，org-orgsync.md §20.9）。
pub fn org_dlog_wm_key(
    org_id: &str,
    name: &str,
    version: &str,
    root_id: &str,
    peer_id: &str,
) -> String {
    format!("dlog:org:{org_id}:{name}@v{version}:wm:{root_id}:{peer_id}")
}

/// org scope 删除日志已收序号键
/// （`dlog:org:{orgId}:{name}@v{version}:seen:{rootId}:{peerId}`）。
///
/// 与 [`Self::org_dlog_wm_key`] 同口径：按 (rootId, peerId) 设备粒度维护。
pub fn org_dlog_seen_key(
    org_id: &str,
    name: &str,
    version: &str,
    root_id: &str,
    peer_id: &str,
) -> String {
    format!("dlog:org:{org_id}:{name}@v{version}:seen:{root_id}:{peer_id}")
}

/// org scope 删除日志水位键前缀
/// （`dlog:org:{orgId}:{name}@v{version}:wm:`）——GC 扫描全部成员设备水位用。
pub fn org_dlog_wm_prefix(org_id: &str, name: &str, version: &str) -> String {
    format!("dlog:org:{org_id}:{name}@v{version}:wm:")
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

/// 从 orgd 数据键解析 (orgId, name, version)。
/// `orgd:{orgId}:{name}@v{version}:{key}` → `(orgId, name, version)`。
pub fn parse_org_data_key(data_key: &str) -> Option<(String, String, String)> {
    let rest = data_key.strip_prefix("orgd:")?;
    let (org_id, rest) = rest.split_once(':')?;
    let at = rest.rfind("@v")?;
    let name = &rest[..at];
    let ver_start = at + 2; // skip "@v"
    let rest_after = &rest[ver_start..];
    let ver_end = rest_after.find(':').unwrap_or(rest_after.len());
    let version = &rest_after[..ver_end];
    Some((org_id.to_string(), name.to_string(), version.to_string()))
}

/// 模块错误。
#[derive(Debug, thiserror::Error)]
pub enum PlugindataError {
    /// 集合未声明（先读写后声明）。
    #[error("collection \"{0}\" is not declared")]
    NotDeclared(String),

    /// 代际内重复声明策略冲突（声明不可变更）。
    #[error(
        "collection \"{name}\" version \"{version}\" is already declared with a different strategy and cannot be re-declared"
    )]
    ConflictingDeclaration {
        /// 集合名。
        name: String,
        /// 代际。
        version: String,
    },

    /// 声明轴取值冲突（带消息）。
    #[error("declaration conflict: {0}")]
    DeclarationConflict(String),

    /// 集合名非法（必须是 `{pluginId}:{collection}`，collection 段 `[A-Za-z0-9_-]+`，
    /// 不得含 `@`）。
    #[error(
        "invalid collection name \"{0}\": expected \"{{pluginId}}:{{collection}}\", collection part allows [A-Za-z0-9_-], \"@\" is reserved"
    )]
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
        && version
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'A'..=b'Z' | b'a'..=b'z' | b'.' | b'-'))
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
    /// 运行空间（O2a，内核按插件运行空间传入；缺省 personal）。
    pub space: Option<Space>,
    /// 账号驻留轴（O2a，org scope 专有；缺省 data-accounts）。
    pub accounts: Option<Accounts>,
    /// 设备驻留轴（缺省 all）。
    pub devices: Option<Devices>,
    /// 保密轴（O2a，org scope 专有；缺省 filtered）。
    pub confidentiality: Option<Confidentiality>,
    /// 敏感轴（M3，personal scope 专有；缺省 normal）。
    pub sensitivity: Option<Sensitivity>,
    /// 合并规则（缺省 lww-record）。
    pub merge: Option<MergeRule>,
    /// 声明者 rootId（org scope 用）。
    pub declared_by: Option<String>,
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
/// 声明记录写 `pdecl:`（personal）或 `org:coll:{orgId}:`（org）——经
/// VersionedStorage 句柄写入即随同步扩散（声明先行）。
///
/// `org_id`：org space 时传入组织 ID（用于构造 `org:coll:` 键与幂等/冲突
/// 检查）；personal space 时传 `None`。
pub fn declare<S: StorageBackend>(
    storage: &mut S,
    plugin_id: &str,
    input: DeclareInput,
    now_ms: i64,
    org_id: Option<&str>,
) -> Result<CollectionDeclaration> {
    validate_name(&input.name, plugin_id)?;
    let version = input.version.unwrap_or_else(|| "1".to_string());
    if !is_valid_version(&version) {
        return Err(PlugindataError::InvalidVersion(version));
    }
    let space = input.space.unwrap_or(Space::Personal);
    let accounts = input.accounts.unwrap_or_default();
    let confidentiality = input.confidentiality.unwrap_or_default();
    let sensitivity = input.sensitivity.unwrap_or_default();
    let scope = input.scope.unwrap_or_default();

    // ── org 轴校验 ──
    match space {
        Space::Org => {
            // encrypted 必须搭配 data-accounts（与 all-members 语义冲突）
            if confidentiality == Confidentiality::Encrypted && accounts == Accounts::AllMembers {
                return Err(PlugindataError::DeclarationConflict(
                    "encrypted requires accounts: data-accounts, not all-members".to_string(),
                ));
            }
            // sensitivity 仅在 personal 空间可用
            if sensitivity != Sensitivity::default() {
                return Err(PlugindataError::DeclarationConflict(
                    "sensitivity is only valid in personal space".to_string(),
                ));
            }
            // scope:local 忽略 accounts/devices/confidentiality（不驻留他端，无意义）
            if scope == Scope::Local {
                // 忽略三轴：使用缺省值（不影响 local 行为）
            }
        }
        Space::Personal => {
            // accounts/confidentiality 仅在 org 空间可用
            if accounts != Accounts::default() || confidentiality != Confidentiality::default() {
                return Err(PlugindataError::DeclarationConflict(
                    "accounts/confidentiality are only valid in org space".to_string(),
                ));
            }
        }
    }

    // F9 收敛：org space 的 org_id 必填校验只做一次——resolve 为
    // `Option<String>`（org space 时缺失即报错），后续既有查找与键构造
    // 复用，不再重复 `org_id.ok_or_else`。
    let org_id_owned = match space {
        Space::Org => Some(
            org_id
                .ok_or_else(|| {
                    PlugindataError::DeclarationConflict(
                        "org_id is required for org space declarations".to_string(),
                    )
                })?
                .to_string(),
        ),
        Space::Personal => None,
    };
    let decl = CollectionDeclaration {
        name: input.name.clone(),
        version: version.clone(),
        scope,
        space: Some(space),
        accounts,
        devices: input.devices.unwrap_or_default(),
        confidentiality,
        sensitivity,
        merge: input.merge.unwrap_or_default(),
        declared_at: now_ms,
        declared_by: input.declared_by,
        ts: Some(now_ms),
        org_id: org_id_owned.clone(),
    };

    // 查找既有声明（同一 name@version）
    let existing = match space {
        Space::Org => {
            let key = CollectionDeclaration::org_decl_key(
                org_id_owned.as_deref().unwrap_or(""),
                &decl.name,
                &decl.version,
            );
            storage
                .get(&key)?
                .and_then(|raw| serde_json::from_str::<CollectionDeclaration>(&raw).ok())
        }
        Space::Personal => get_declaration(storage, &decl.name, &decl.version)?,
    };
    if let Some(mut existing) = existing {
        let same = existing.scope == decl.scope
            && existing.devices == decl.devices
            && existing.merge == decl.merge
            && existing.accounts == decl.accounts
            && existing.confidentiality == decl.confidentiality
            && existing.space == decl.space;
        if same {
            // 幂等返回：补回 org_id（旧记录可能未存该字段，路由依赖它）
            existing.org_id = org_id_owned.clone();
            return Ok(existing);
        }
        return Err(PlugindataError::ConflictingDeclaration {
            name: decl.name,
            version: decl.version,
        });
    }

    let decl_key = match space {
        Space::Org => CollectionDeclaration::org_decl_key(
            org_id_owned.as_deref().unwrap_or(""),
            &decl.name,
            &decl.version,
        ),
        Space::Personal => decl.decl_key(),
    };
    storage.put(&decl_key, &serde_json::to_string(&decl)?)?;
    Ok(decl)
}

/// 为组织注册全部内建 all-members 集合（O2b 工作项 1）：组织结构/通讯录/
/// 邀请记录。声明记录 `org:coll:{orgId}:{name}@v1` 由内核在组织创建/迁移时
/// 自动写入（declaredBy = 创建者或内核），accounts 恒 all-members；键不搬家
/// （存量键前缀即集合键域，见 [`crate::sync::orgsync::BuiltinOrgCollection`]）。
///
/// 幂等：同 name@version 已声明且策略一致 → 保留既有记录返回；策略冲突 →
/// 报错（代际内不可变更）。
///
/// 本函数同时写声明记录与 pmeta（组织创建路径走原始 `&mut S`，非
/// VersionedStorage 句柄，须手动版本化——对齐 `save_record_pdsync` 口径）。
pub fn declare_builtin_org_collections<S: StorageBackend>(
    storage: &mut S,
    org_id: &str,
    declared_by: &str,
    now_ms: i64,
    node_id: &str,
) -> Result<()> {
    for builtin in crate::sync::orgsync::BuiltinOrgCollection::all() {
        let name = builtin.name();
        let version = builtin.version();
        let decl_key = CollectionDeclaration::org_decl_key(org_id, name, version);
        // 幂等：已声明且策略一致 → 跳过；冲突 → 报错（代际内不可变更）。
        if let Some(raw) = storage.get(&decl_key)? {
            if let Ok(existing) = serde_json::from_str::<CollectionDeclaration>(&raw) {
                if existing.accounts == Accounts::AllMembers && existing.space == Some(Space::Org) {
                    continue;
                }
            }
            return Err(PlugindataError::DeclarationConflict(format!(
                "builtin collection {name}@v{version} already declared with different strategy"
            )));
        }
        let decl = CollectionDeclaration {
            name: name.to_string(),
            version: version.to_string(),
            scope: Scope::Sync,
            space: Some(Space::Org),
            accounts: Accounts::AllMembers,
            devices: Devices::All,
            confidentiality: Confidentiality::Filtered,
            sensitivity: Sensitivity::default(),
            merge: builtin.merge(),
            declared_at: now_ms,
            declared_by: Some(declared_by.to_string()),
            ts: Some(now_ms),
            org_id: Some(org_id.to_string()),
        };
        let raw = serde_json::to_string(&decl)?;
        storage.put(&decl_key, &raw)?;
        crate::sync::personal::put_personal(storage, node_id, &decl_key, &raw, now_ms)
            .map_err(|e| PlugindataError::DeclarationConflict(e.to_string()))?;
    }
    Ok(())
}
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

/// 读指定代际的声明（org scope：org_id 定键到 `org:coll:{orgId}:`）。
pub fn get_declaration_org<S: StorageBackend>(
    storage: &S,
    org_id: &str,
    name: &str,
    version: &str,
) -> Result<Option<CollectionDeclaration>> {
    let Some(raw) = storage.get(&org_decl_key(org_id, name, version))? else {
        return Ok(None);
    };
    let mut decl: CollectionDeclaration = match serde_json::from_str(&raw) {
        Ok(d) => d,
        Err(_) => return Ok(None),
    };
    decl.org_id = Some(org_id.to_string());
    Ok(Some(decl))
}

/// 列出某集合名的全部代际（按声明时间升序；personal scope）。
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
    out.sort_by(|a, b| {
        a.declared_at
            .cmp(&b.declared_at)
            .then(a.version.cmp(&b.version))
    });
    Ok(out)
}

/// 列出某组织某集合名的全部代际（org scope：键域 `org:coll:{orgId}:`）。
pub fn list_versions_org<S: StorageBackend>(
    storage: &S,
    org_id: &str,
    name: &str,
) -> Result<Vec<CollectionDeclaration>> {
    let prefix = format!("org:coll:{org_id}:{name}@v");
    let mut out = Vec::new();
    for (_key, raw) in storage.scan(&ScanOptions::prefix(&prefix))? {
        if let Ok(mut decl) = serde_json::from_str::<CollectionDeclaration>(&raw)
            && decl.name == name
        {
            decl.org_id = Some(org_id.to_string());
            out.push(decl);
        }
    }
    out.sort_by(|a, b| {
        a.declared_at
            .cmp(&b.declared_at)
            .then(a.version.cmp(&b.version))
    });
    Ok(out)
}

/// 解析目标声明：version 指定 → 该代际；缺省 → 最新代际（声明时间最新者）。
/// 未声明 → [`PlugindataError::NotDeclared`]。（personal scope）
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

/// 解析目标声明（org scope：org_id 定键）。
pub fn resolve_org<S: StorageBackend>(
    storage: &S,
    org_id: &str,
    name: &str,
    version: Option<&str>,
) -> Result<CollectionDeclaration> {
    if let Some(v) = version {
        return get_declaration_org(storage, org_id, name, v)?
            .ok_or_else(|| PlugindataError::NotDeclared(format!("{name}@v{v}")));
    }
    list_versions_org(storage, org_id, name)?
        .into_iter()
        .last()
        .ok_or_else(|| PlugindataError::NotDeclared(name.to_string()))
}

/// 按运行空间解析数据记录键：org scope → `orgd:{orgId}:{name}@v{version}:`
/// （orgsync 受管）；personal scope → `pdoc:`/`ldoc:`（pdsync/本地）。
///
/// whole 集合 key 归一为 [`WHOLE_KEY`]。
fn data_key_for(decl: &CollectionDeclaration, key: &str) -> String {
    if decl.space == Some(Space::Org) {
        let oid = decl.org_id.as_deref().unwrap_or("");
        CollectionDeclaration::org_data_key(oid, &decl.name, &decl.version, key, decl.merge)
    } else {
        decl.data_key(key)
    }
}

/// 按运行空间解析数据记录键前缀（query 扫描用）。
fn data_prefix_for(decl: &CollectionDeclaration) -> String {
    if decl.space == Some(Space::Org) {
        let oid = decl.org_id.as_deref().unwrap_or("");
        CollectionDeclaration::org_data_prefix(oid, &decl.name, &decl.version)
    } else {
        decl.data_prefix()
    }
}

/// 写记录（自动版本化由 VersionedStorage 句柄保证；whole 集合 key 归一为
/// [`WHOLE_KEY`]；append-only 拒绝覆盖已存在 key；org scope 路由到 `orgd:` 键）。
pub fn save<S: StorageBackend>(
    storage: &mut S,
    decl: &CollectionDeclaration,
    key: &str,
    value: &str,
) -> Result<()> {
    let data_key = data_key_for(decl, key);
    if decl.merge == MergeRule::AppendOnly && storage.get(&data_key)?.is_some() {
        return Err(PlugindataError::AppendOnlyViolation(decl.name.clone()));
    }
    storage.put(&data_key, value)?;
    Ok(())
}

/// 删记录（append-only 拒绝；删除经中间件自动墓碑 + 删除日志，传播到复制组；
/// org scope 路由到 `orgd:` 键）。
pub fn del<S: StorageBackend>(
    storage: &mut S,
    decl: &CollectionDeclaration,
    key: &str,
) -> Result<()> {
    if decl.merge == MergeRule::AppendOnly {
        return Err(PlugindataError::AppendOnlyViolation(decl.name.clone()));
    }
    storage.delete(&data_key_for(decl, key))?;
    Ok(())
}

/// 读单条（whole 集合忽略 key；org scope 路由到 `orgd:` 键）。
pub fn get<S: StorageBackend>(
    storage: &S,
    decl: &CollectionDeclaration,
    key: &str,
) -> Result<Option<String>> {
    storage.get(&data_key_for(decl, key)).map_err(Into::into)
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
    let base = data_prefix_for(decl);
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
pub fn drop_version<S: StorageBackend>(
    storage: &mut S,
    decl: &CollectionDeclaration,
) -> Result<()> {
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
mod decl_converge;

pub use decl_converge::{apply_org_decl_convergent, org_has_data_account_collections};

#[cfg(test)]
mod tests;

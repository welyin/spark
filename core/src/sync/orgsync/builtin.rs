//! orgsync 内建 all-members 集合注册（O2b）：存量组织数据纳管为内建集合。
//!
//! 存量组织数据（org:meta / ct:org）纳管为内建 all-members 集合：
//! 键不搬家（存量数据零迁移，pdsync 自设备同步不动），成员间同步面从快照
//! 改为反熵。集合声明（org:coll:{orgId}:{name}@v1）由内核在组织创建/迁移时
//! 自动写入（declaredBy = 创建者或内核），accounts 恒 all-members。
//!
//! 键域归属（org-orgsync.md §20.8 灰度关系）：
//! - org:structure@v1 → org:meta:{orgId}（单记录，whole 语义）；
//! - org:contacts@v1  → ct:org:{orgId}:*（多记录，lww-record）；
//! - org:invitations@v1 → org:invpub:{orgId}:*（多记录，lww-record；batch3 §2
//!   管理面邀请投影，值不含 inviteCode）。
//!
//! F7（org-invite-scope-fix §2.1）：`org:inv:in:/out:`（邀请记录）**退出
//! orgsync**——它是 per-account 的一对一关系状态（入站=invitee 私有应答
//! 状态，出站=inviter 记账），本就不该是 all-members 集合（键碰撞+传播面
//! 错位实证）；仅经 pdsync `org:inv` category 在自设备间同步（换设备应答/
//! 对账），键形状不变。
//!
//! 集合名恒为 `org:{kind}`（保留 org 插件前缀），版本恒 "1"（初版无代际演进）。

use crate::plugindata::{MergeRule, org_data_prefix, org_decl_key};

// ── 常量 ────────────────────────────────────────────────────────────────

/// hello 触发防抖（1s，同个人域 watcher）。
pub const ORGSYNC_HELLO_DEBOUNCE_MS: i64 = 1_000;

/// dlogAck 重发水位：5s / 15s（同 pdsync §5.6）。
pub const ORGSYNC_DLOG_ACK_RETRY_FIRST_MS: i64 = 5_000;
pub const ORGSYNC_DLOG_ACK_RETRY_SECOND_MS: i64 = 15_000;

/// orgsync-data 单批字节上限（沿用 dm 信封体积约束）。
pub const ORGSYNC_BATCH_BYTES: usize = 256 * 1024;

/// 内建 all-members 集合枚举。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuiltinOrgCollection {
    /// 组织结构：`org:meta:{orgId}` 单记录（whole）。
    Structure,
    /// 组织通讯录/成员扩展：`ct:org:{orgId}:*` 多记录（lww-record）。
    Contacts,
    /// 管理面邀请投影：`org:invpub:{orgId}:{inviterRoot}:{inviteeRoot}`
    /// 多记录（lww-record，batch3 §2——值 = 不含 inviteCode 的公开元数据
    /// 投影，真源是 personal 域 `org:inv:out:` 记录）。
    Invitations,
}

impl BuiltinOrgCollection {
    /// 集合名（`org:{kind}`，保留 org 插件前缀）。
    pub fn name(&self) -> &'static str {
        match self {
            BuiltinOrgCollection::Structure => "org:structure",
            BuiltinOrgCollection::Contacts => "org:contacts",
            BuiltinOrgCollection::Invitations => "org:invitations",
        }
    }

    /// 代际版本（恒 "1"，初版无代际演进）。
    pub fn version(&self) -> &'static str {
        "1"
    }

    /// 线上集合标识 `{name}@v{version}`（hello/信封 `collection` 字段）。
    pub fn full_name(&self) -> String {
        format!("{}@v{}", self.name(), self.version())
    }

    /// 合并规则：org:meta 整域单记录（whole）；ct:org 多记录 lww-record。
    pub fn merge(&self) -> MergeRule {
        match self {
            BuiltinOrgCollection::Structure => MergeRule::Whole,
            BuiltinOrgCollection::Contacts | BuiltinOrgCollection::Invitations => {
                MergeRule::LwwRecord
            }
        }
    }

    /// 声明记录键 `org:coll:{orgId}:{name}@v{version}`。
    pub fn decl_key(&self, org_id: &str) -> String {
        org_decl_key(org_id, self.name(), self.version())
    }

    /// 该集合的数据键域：一组按 org 展开的存量键前缀（键不搬家）。
    pub fn data_prefixes(&self, org_id: &str) -> Vec<String> {
        match self {
            // R3：org:structure 额外承载 `org:acl:{orgId}:`（O4 授权名单）——
            // acl 本身是 all-members 系统数据（§20.7），复制组=全体成员。否则
            // acl 随 encrypted 集合（data-accounts 复制组）流动，普通成员读者
            // 收不到，orgkey-deliver 入站无法验 sender∈owners。把 acl 并入
            // org:structure 的 all-members 键域，全员经此集合同步可见。
            //
            // F2-P1（org-acl-genesis-fix §2.1）：`org:coll:{orgId}:`（集合声明）
            // 同性质并入——声明是全员可见的组织元数据（传播范围与数据面
            // accounts 轴解耦），修复「data-accounts 集合的声明普通成员永远
            // 收不到」的实现漂移。声明不经 hello 的复制组裁剪。
            // P1（阶段四A 分拆）：org:member:{orgId}: 并入——成员条目
            // lww-record 逐成员一条，与 org:acl:/org:coll: 同集合全员流动；
            // 成员移除 = 成员记录墓碑（org 域 dlog 传播）。
            BuiltinOrgCollection::Structure => vec![
                format!("org:meta:{org_id}"),
                format!("org:acl:{org_id}:"),
                format!("org:coll:{org_id}:"),
                format!("org:member:{org_id}:"),
            ],
            BuiltinOrgCollection::Contacts => vec![format!("ct:org:{org_id}:")],
            BuiltinOrgCollection::Invitations => vec![format!("org:invpub:{org_id}:")],
        }
    }

    /// 全部内建集合（供内核组织创建/迁移时逐一注册声明）。
    pub fn all() -> [BuiltinOrgCollection; 3] {
        [
            BuiltinOrgCollection::Structure,
            BuiltinOrgCollection::Contacts,
            BuiltinOrgCollection::Invitations,
        ]
    }
}

/// 按集合名解析内建集合（`org:structure`/`org:contacts`/`org:invitations`；
/// F7 起 `org:invites` 已退出）；非内建 → `None`（插件声明集合，数据键域
/// 走 `orgd:`）。
pub fn builtin_collection_by_name(name: &str) -> Option<BuiltinOrgCollection> {
    BuiltinOrgCollection::all()
        .into_iter()
        .find(|b| b.name() == name)
}

/// 集合的数据键域：一个集合要同步的全部数据记录键前缀（按 org 展开）。
///
/// - 插件集合 → `[orgd:{orgId}:{name}@v{version}:]`；
/// - 内建集合 → 存量键前缀（键不搬家）。
pub fn collection_data_prefixes(org_id: &str, name: &str, version: &str) -> Vec<String> {
    match builtin_collection_by_name(name) {
        Some(builtin) => builtin.data_prefixes(org_id),
        None => vec![org_data_prefix(org_id, name, version)],
    }
}

/// 把存量组织键解析为其所属内建集合的 (orgId, name, version) 作用域
/// （用于删除日志路由，工作项 2：存量键删除落 org 域 dlog）。
///
/// 存量键形态：
/// - `org:meta:{orgId}` → (orgId, org:structure, 1)；
/// - `ct:org:{orgId}:*` → (orgId, org:contacts, 1)。
///
/// F7：`org:inv:in/out:` 不再是任何 orgsync 集合的键域（退出全员同步，
/// 回归 personal 域自设备同步）——删除只登个人域 dlog。
///
/// 非存量组织键 → `None`（插件 `orgd:` 键另走 `parse_org_data_key`）。
pub fn legacy_org_key_scope(key: &str) -> Option<(String, String, String)> {
    if let Some(rest) = key.strip_prefix("org:meta:") {
        let org_id = rest.split(':').next()?;
        return Some((
            org_id.to_string(),
            BuiltinOrgCollection::Structure.name().to_string(),
            BuiltinOrgCollection::Structure.version().to_string(),
        ));
    }
    if let Some(rest) = key.strip_prefix("ct:org:") {
        let org_id = rest.split(':').next()?;
        return Some((
            org_id.to_string(),
            BuiltinOrgCollection::Contacts.name().to_string(),
            BuiltinOrgCollection::Contacts.version().to_string(),
        ));
    }
    None
}

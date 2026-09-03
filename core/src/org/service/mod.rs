//! 组织服务层（对齐 desktop/src/main/organization/service.ts）。
//!
//! 纯逻辑层：只操作 [`StorageBackend`]，不触碰网络。TS 中注入的
//! `syncContext`（推送快照）与 `inviteContext.connectAndPull`（连接拉取）
//! 属 p2p 模块职责，本层以返回值/参数形式对接：
//! - 成员变更后需要推送的接收方集合由 [`OrganizationService::sync_recipients`] 给出
//! - 邀请码接受的连接/拉取由调用方完成，随后用
//!   [`OrganizationService::check_invite_accepted`] 做落库确认
//!
//! 时间（`Date.now()`）一律以 `now_ms` 参数注入，保证纯函数可测。
//!
//! 代码组织：本文件为 [`OrganizationService`] 门面、公开输入/返回类型与各变更
//! 路径共用的私有辅助（记录读写、admin 校验、sync 重建）；创建/删除在 `create`，
//! 成员增删与各类视图在 `members`，邀请码生成/接受确认在 `invites`，入站数据
//! 落库（快照/nodeInfoClaim）在 `snapshot_apply`，网关与公开标志在 `settings`；
//! 阶段四A per-member 条目的自写/本地擦除在 `member_entries`（P2）；单测按域
//! 拆在 `tests/`。

mod atomic;
mod create;
mod invite_records;
mod invites;
mod member_entries;
mod members;
mod members_access;
mod settings;
mod snapshot_apply;

/// F8：org:meta 写路径的原子段原语与注入锁类型（org-meta-rmw-fix §2.2）。
pub use atomic::OrgMetaWriteLock;
/// 阶段四A P2：本机被移出组织的本地擦除（removed 通知入站 / legacy pull
/// Removed 分支共用）。
pub use member_entries::wipe_org_local;
/// F7 存量迁移（org-invite-scope-fix §2.3）：org:invites 退出 orgsync 的
/// 一次性清理（入站邀请记录清空 + 声明墓碑化），unlock 时幂等执行。
pub use invite_records::migrate_org_invites_out_of_orgsync;
/// batch3 §2 管理面邀请投影（org:invitations@v1）：键构造/投影/双写/对账。
pub use invite_records::{
    invpub_projection, org_invpub_key, put_invite_record_with_projection,
    reconcile_outbound_invites_with_members,
};

use serde_json::Value;

use crate::storage::{ScanOptions, StorageBackend};

use super::snapshot::{build_organization_sync_versions, pick_sync_sections_by_priority};
use super::types::{
    ORG_META_PREFIX, OrganizationMember, OrganizationNodeInfo, OrganizationRecord,
    OrganizationSyncState, org_member_key, organization_key,
};
use super::{OrgError, Result};

/// 创建组织输入（types.ts:95-99）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CreateOrganizationInput {
    /// 组织名（trim + 连续空白归一）。
    pub name: String,
    /// 描述（trim，可省）。
    pub description: Option<String>,
    /// 组织 logo（`data:image/` data URL，可省；空白等同未设置，非空时按
    /// `identity::validate_avatar` 同口径校验）。
    pub avatar: Option<String>,
    /// 基础插件域（`plugin:` 前缀，可省——组织与插件不再强关联，设计 §7.2）。
    pub base_plugin_domain: Option<String>,
}

/// `createOrgInvite` 的返回（service.ts:315-339）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreatedOrgInvite {
    /// 邀请码（base64url）。
    pub invite: String,
    /// 组织 id。
    pub org_id: String,
    /// 组织名。
    pub org_name: String,
}

/// `acceptOrgInvite` 成功确认后的返回（service.ts:373）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InviteAcceptance {
    /// 组织 id。
    pub org_id: String,
    /// 组织名。
    pub org_name: String,
    /// 成员数。
    pub member_count: usize,
}

/// 组织服务（无状态；全部方法以存储与参数为输入）。
pub struct OrganizationService;

/// `updateMyIdentity` 的身份补丁（成员更新自己的组织内身份字段）。
///
/// 语义对齐 identity `update_profile`：字符串字段 `None` 不变；
/// `avatar` 三态（`Some(Some)` 设置 / `Some(None)` 清除 / `None` 不变）；
/// `gender`/`region`/`signature` `Some("")`（或全空白）= 清除。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OrgIdentityPatch {
    /// 昵称（trim 后 1–24 字符；`None` 不变，不可清除）。
    pub nickname: Option<String>,
    /// 头像 data URL（三态；见上）。
    pub avatar: Option<Option<String>>,
    /// 性别（≤16 字符；`Some("")` 清除）。
    pub gender: Option<String>,
    /// 地区（≤64 字符；`Some("")` 清除）。
    pub region: Option<String>,
    /// 个性签名（≤128 字符；`Some("")` 清除）。
    pub signature: Option<String>,
    /// 是否在组织内展示个人身份（`None` 不变）。
    pub use_personal_identity: Option<bool>,
}

/// P1-b 读切换开关（阶段四A per-member 分拆）：true = 读路径走装配视图
///（org:meta summary + `org:member:` 前缀扫描合成 members 段）；false =
/// 回滚读 whole 记录（成员条目残留无害）。默认 true（本批 P1-b 上线）。
static READ_ASSEMBLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// P1-b 回滚开关（排障/评审用；测试翻转须串行）。
pub fn set_member_read_assembled(assembled: bool) {
    READ_ASSEMBLED.store(assembled, std::sync::atomic::Ordering::Relaxed);
}

/// 当前读口径（true = 装配视图）。
pub fn member_read_assembled() -> bool {
    READ_ASSEMBLED.load(std::sync::atomic::Ordering::Relaxed)
}

impl OrganizationService {
    /// 读取单个组织记录；不存在返回 `Ok(None)`。P1-b：开关开启时经
    /// [`Self::assemble_members`] 装配成员段。
    pub fn get_record<S: StorageBackend>(
        storage: &S,
        org_id: &str,
    ) -> Result<Option<OrganizationRecord>> {
        let Some(raw) = storage.get(&organization_key(org_id))? else {
            return Ok(None);
        };
        let record: OrganizationRecord = serde_json::from_str(&raw)?;
        Ok(Some(Self::assemble_members(storage, record)?))
    }

    /// 读取全部组织记录（`org:meta:` 前缀扫描，键升序）。
    ///
    /// 对齐 TS `readAllOrganizations`：损坏 JSON 直接报错（不静默跳过）。
    /// P1-b：逐组织装配成员段（开关开启时）。
    pub fn read_all_organizations<S: StorageBackend>(
        storage: &S,
    ) -> Result<Vec<OrganizationRecord>> {
        let rows = storage.scan(&ScanOptions::prefix(ORG_META_PREFIX))?;
        rows.into_iter()
            .map(|(_, value)| {
                let record: OrganizationRecord = serde_json::from_str(&value).map_err(OrgError::from)?;
                Self::assemble_members(storage, record)
            })
            .collect()
    }

    /// P1-b 装配视图：summary 记录 + 成员条目（`org:member:{orgId}:` 前缀
    /// 扫描）合成 members 段——成员条目逐 rootId 覆盖 whole 同名成员（条目
    /// 为权威）；**墓碑条目排除**（成员移除 = 值删除 + pmeta 墓碑——值不在
    /// 扫描结果内，须另扫 pmeta 前缀收墓碑 rootId 集合，whole 同名成员一并
    /// 排除）；whole 中无条目覆盖且未被墓碑排除的成员保留（投影缺口/迁移
    /// 窗口/混跑期旧端只有 whole）。无成员条目且无墓碑 → 原样返回（快速
    /// 路径，等价 whole 读）。
    fn assemble_members<S: StorageBackend>(
        storage: &S,
        mut record: OrganizationRecord,
    ) -> Result<OrganizationRecord> {
        if !member_read_assembled() {
            return Ok(record);
        }
        let prefix = format!("{}{}:", crate::org::types::ORG_MEMBER_PREFIX, record.org_id);
        let rows = storage.scan(&ScanOptions::prefix(&prefix))?;
        // 墓碑 rootId 集合：移除事件的值已删，只剩 pmeta 墓碑
        let tomb_prefix = crate::sync::personal_meta_key(&prefix);
        let mut tombstoned: std::collections::HashSet<String> = std::collections::HashSet::new();
        for (mkey, raw) in storage.scan(&ScanOptions::prefix(&tomb_prefix))? {
            let is_tomb = serde_json::from_str::<crate::sync::meta::DocMeta>(&raw)
                .ok()
                .and_then(|m| m.tombstone)
                .unwrap_or(false);
            if is_tomb && let Some(root_id) = mkey.strip_prefix(&tomb_prefix) {
                tombstoned.insert(root_id.to_string());
            }
        }
        if rows.is_empty() && tombstoned.is_empty() {
            return Ok(record);
        }
        let mut by_root: std::collections::HashMap<String, _> = record
            .members
            .drain(..)
            // 墓碑排除：whole 中的同名成员随移除事件出局（条目覆盖优先级更
            // 高——同名活条目在下方覆盖回来；活条目带墓碑 pmeta 为异常态，
            // 按条目为权威口径采信活条目）
            .filter(|m| !tombstoned.contains(&m.root_id))
            .map(|m| (m.root_id.clone(), m))
            .collect();
        for (key, raw) in rows {
            // 墓碑条目排除（成员移除 = 成员记录墓碑）
            let is_tomb = storage
                .get(&crate::sync::personal_meta_key(&key))?
                .and_then(|raw| {
                    serde_json::from_str::<crate::sync::meta::DocMeta>(&raw).ok()
                })
                .is_some_and(|m| m.tombstone == Some(true));
            if is_tomb {
                continue;
            }
            let member: super::types::OrganizationMember =
                serde_json::from_str(&raw).map_err(OrgError::from)?;
            by_root.insert(member.root_id.clone(), member);
        }
        record.members = super::types::sort_members(&by_root.into_values().collect::<Vec<_>>());
        Ok(record)
    }

    /// 持久化记录到 `org:meta:<orgId>`。
    pub fn save_record<S: StorageBackend>(
        storage: &mut S,
        record: &OrganizationRecord,
    ) -> Result<()> {
        storage.put(
            &organization_key(&record.org_id),
            &serde_json::to_string(record)?,
        )?;
        Ok(())
    }

    /// pdsync 感知的组织记录写入（P5）：落 `org:meta:{orgId}`；版本记账
    /// 由中间件自动完成（调用方传版本化句柄；裸句柄上本函数不写 pmeta
    /// ——仅 kernel 门面路径使用）。
    pub fn save_record_pdsync<S: StorageBackend>(
        storage: &mut S,
        record: &OrganizationRecord,
        now_ms: i64,
        node_id: &str,
    ) -> Result<()> {
        let _ = (now_ms, node_id); // 记账已下沉中间件，参数保留以稳定签名
        let key = organization_key(&record.org_id);
        let json = serde_json::to_string(record)?;
        storage.put(&key, &json)?;
        Ok(())
    }

    /// pdsync 感知的组织记录删除（P5）：墓碑/删除日志由版本化中间件在
    /// `delete` 时自动完成（调用方传版本化句柄；裸句柄上本函数不会写
    /// 墓碑——仅 kernel 门面路径使用）。
    pub fn delete_record_pdsync<S: StorageBackend>(
        storage: &mut S,
        org_id: &str,
        now_ms: i64,
        node_id: &str,
    ) -> Result<()> {
        let _ = (now_ms, node_id); // 记账已下沉中间件，参数保留以稳定签名
        storage.delete(&organization_key(org_id))?;
        Ok(())
    }

    fn require_organization<S: StorageBackend>(
        storage: &S,
        org_id: &str,
    ) -> Result<OrganizationRecord> {
        Self::get_record(storage, org_id)?.ok_or(OrgError::OrganizationNotFound)
    }

    fn require_admin(record: &OrganizationRecord, root_id: &str) -> Result<()> {
        if !record.is_admin(root_id) {
            return Err(OrgError::AdminRequired);
        }
        Ok(())
    }

    /// 成员变更后的 sync 重建（service.ts 各变更路径的公共收尾）：
    /// `versions = build(record, tx.createdAt)`、`sections = pickSyncSectionsByPriority`、
    /// `lastSyncedAt` 保留原值（无则 0）。
    fn rebuild_sync_after_mutation(
        record: &mut OrganizationRecord,
        previous_last_synced_at: i64,
        transaction_created_at: i64,
    ) {
        record.sync = Some(OrganizationSyncState {
            versions: build_organization_sync_versions(record, transaction_created_at),
            sections: pick_sync_sections_by_priority(),
            last_synced_at: previous_last_synced_at,
        });
    }
}

/// 阶段四A P1 混跑兼容（设计 §6）：whole 记录（org:meta / legacy org-share
/// 快照）合入后就地投影成员条目——把 whole 的每个成员写到
/// `org:member:{orgId}:{rootId}`，让只写 whole 的旧端流量在新端装配视图
/// 下完整可见。
///
/// 投影写一律**远端语义**（防回声）：不 bump 本机分量、不写序号键，直接
/// batch put 值 + pmeta。逐成员判定：
/// - 内容相同 → 跳过（幂等，不产生写）；
/// - 既有条目 vv **严格领先** whole vv（判 Local）→ 跳过（条目为权威，
///   装配视图以条目为准）；
/// - 无条目 / whole 严格领先（Remote）/ vv 相等（Equal，同 vv 内容漂移的
///   异常态，取 whole 同源侧）→ 覆盖写，pmeta 复制 whole 合入后的 pmeta
///   （内容与 vv 同源不脱节）；
/// - **并发（Concurrent）→ 条目级合并而非覆盖**（P2 重审落定，P1 挂账
///   收口）：成员自写条目（P2 L2）引入后，条目可能携带 whole 尚未合入的
///   本人字段组/nodeInfo——覆盖会回盖自写更新。合并 =
///   [`crate::org::meta_merge::merge_member_record`]（秩 = (各自 pmeta.ts,
///   canonical 字节)：混跑期旧端 whole 通常 ts 更高→其管理员字段组生效；
///   本人刚自写的条目 ts 更高→本人字段组存活；accessKey 写一次守卫与
///   nodeInfo/extra 并集双活），pmeta = vv 并集 + ts 取大（支配双输入，
///   值/vv 不脱节）。条目解析失败回退覆盖写。
///
/// 返回实际写入条数（观测用）。
pub fn project_member_entries_from_whole<S: StorageBackend>(
    storage: &mut S,
    org_id: &str,
    members: &[OrganizationMember],
    whole_meta: &crate::sync::meta::DocMeta,
) -> Result<usize> {
    let mut ops = Vec::new();
    for member in members {
        let key = org_member_key(org_id, &member.root_id);
        let value = serde_json::to_string(member)?;
        if storage.get(&key)?.as_deref() == Some(value.as_str()) {
            continue; // 内容相同：幂等跳过
        }
        let entry_meta = crate::sync::get_personal_meta(storage, &key)
            .map_err(|e| OrgError::Storage(crate::storage::StorageError::Backend(e.to_string())))?;
        let cmp = crate::sync::compare_version_vectors(
            entry_meta.as_ref().map(|m| &m.vv),
            Some(&whole_meta.vv),
        );
        use crate::sync::meta::CompareResult as Cmp;
        let (out_value, out_meta) = match cmp {
            Cmp::Local => continue, // 条目严格领先：条目为权威，不回投影
            Cmp::Concurrent => {
                // P2：并发做条目级合并（自写条目不被回盖）
                let entry_raw = storage.get(&key)?;
                let entry_member = entry_raw
                    .as_deref()
                    .and_then(|raw| serde_json::from_str::<OrganizationMember>(raw).ok());
                let Some(entry_meta) = entry_meta else {
                    // 无 pmeta 不判 Concurrent（None vs whole 判 Remote）——防御
                    // 分支：直接覆盖
                    ops.push(crate::storage::BatchOperation::put(key.clone(), value));
                    ops.push(crate::storage::BatchOperation::put(
                        crate::sync::personal_meta_key(&key),
                        serde_json::to_string(&crate::sync::meta::DocMeta {
                            vv: whole_meta.vv.clone(),
                            ts: whole_meta.ts,
                            node_id: None,
                            tombstone: None,
                        })?,
                    ));
                    continue;
                };
                let Some(entry_member) = entry_member else {
                    // 条目损坏：回退覆盖写（whole 同源侧兜底）
                    ops.push(crate::storage::BatchOperation::put(key.clone(), value));
                    ops.push(crate::storage::BatchOperation::put(
                        crate::sync::personal_meta_key(&key),
                        serde_json::to_string(&crate::sync::meta::DocMeta {
                            vv: whole_meta.vv.clone(),
                            ts: whole_meta.ts,
                            node_id: None,
                            tombstone: None,
                        })?,
                    ));
                    continue;
                };
                let entry_rank = (
                    entry_meta.ts,
                    serde_json::to_string(&entry_member).unwrap_or_default(),
                );
                let whole_rank = (whole_meta.ts, value.clone());
                let merged = crate::org::meta_merge::merge_member_record(
                    &entry_member,
                    member,
                    &entry_rank,
                    &whole_rank,
                );
                let merged_meta = crate::sync::meta::DocMeta {
                    vv: crate::sync::merge_version_vectors(
                        Some(&entry_meta.vv),
                        Some(&whole_meta.vv),
                    ),
                    ts: entry_meta.ts.max(whole_meta.ts),
                    node_id: None,
                    tombstone: None,
                };
                (serde_json::to_string(&merged)?, merged_meta)
            }
            Cmp::Remote | Cmp::Equal => (
                value,
                crate::sync::meta::DocMeta {
                    vv: whole_meta.vv.clone(),
                    ts: whole_meta.ts,
                    node_id: None,
                    tombstone: None,
                },
            ),
        };
        ops.push(crate::storage::BatchOperation::put(key.clone(), out_value));
        ops.push(crate::storage::BatchOperation::put(
            crate::sync::personal_meta_key(&key),
            serde_json::to_string(&out_meta)?,
        ));
    }
    let written = ops.len() / 2;
    if !ops.is_empty() {
        storage.batch(ops)?;
    }
    Ok(written)
}

/// 阶段四A P1 存量迁移（org-member-split §2.4）：读全部 org:meta → 逐成员
/// 写 `org:member:{orgId}:{rootId}` 条目（已存在跳过 = 幂等扫尾——迁移失败
/// 的半态由下轮启动补齐）；**不动 org:meta 的 members 段**（双写期保留，
/// 旧端读它无感）。
///
/// 调用方须传**版本化句柄**：迁移产出经中间件本机 bump——各端各自迁移
/// 产出内容相同、vv 不同的等价记录，首轮 orgsync 交换判 Concurrent →
/// 成员级合并收敛到同内容，幂等无害（一次性写放大，设计 §8 已裁决接受）。
/// unlock 时幂等执行（login.rs 挂接）。返回新写入的条目数。
pub fn migrate_org_members_split<S: StorageBackend>(storage: &mut S) -> Result<usize> {
    let orgs = OrganizationService::read_all_organizations(storage)?;
    let mut written = 0usize;
    for record in &orgs {
        for member in &record.members {
            let key = org_member_key(&record.org_id, &member.root_id);
            if storage.get(&key)?.is_some() {
                continue; // 已存在（含墓碑条目不复活：value 缺失但 pmeta 墓碑
                          // 时 get 为 None——此处重写会与 whole 一致地恢复该
                          // 成员，与 whole 的 members 段口径相同，可接受）
            }
            storage.put(&key, &serde_json::to_string(member)?)?;
            written += 1;
        }
    }
    Ok(written)
}

/// 事务 payload 的 `nodeInfo` 键：未提供时整个键缺省（对齐 TS
/// `{nodeInfo: undefined}` 被 `JSON.stringify` 丢弃的行为）。
fn node_info_payload(node_info: Option<&OrganizationNodeInfo>) -> serde_json::Map<String, Value> {
    let mut map = serde_json::Map::new();
    if let Some(info) = node_info
        && let Ok(value) = serde_json::to_value(info)
    {
        map.insert("nodeInfo".to_string(), value);
    }
    map
}

/// 三态字段（`Option<Option<&str>>`，如 avatar）的事务审计摘要（m1：payload
/// 不落完整内容——data URL 序列化可达 200KB）：`None` 未变更 → Null；
/// `Some(None)` 清除 → `false`；`Some(Some(_))` 设置 → 仅记内容长度。
fn tri_state_audit(value: Option<Option<&str>>) -> Value {
    match value {
        None => Value::Null,
        Some(None) => Value::from(false),
        Some(Some(content)) => Value::from(content.len() as i64),
    }
}

/// 空串清除字段（`Option<&str>`，空白 = 清除）的事务审计摘要（m1，口径同上）：
/// `None` 未变更 → Null；空白清除 → `false`；非空设置 → 仅记内容长度。
fn clearable_audit(value: Option<&str>) -> Value {
    match value {
        None => Value::Null,
        Some(content) if content.trim().is_empty() => Value::from(false),
        Some(content) => Value::from(content.len() as i64),
    }
}

//! orgsync-hello 出站触发（O2a §20.3）：本机作为复制组成员，向已连接的
//! 复制组成员发送 orgsync-hello 摘要。
//!
//! 从 `tick.rs` 拆出的子模块（F4，tick.rs 已超 650 行硬线）——orgsync 的
//! hello 触发逻辑单独成文件，不再加重 tick。
//!
//! B2 语义：
//! - **按收件人逐成员生成**（不克隆同一 body 群发）：dlogAck = 我已收讫**对端**
//!   该集合删除日志最大序号（`org_dlog_get_seen(recipient)`）；
//! - **只向复制组成员发送**：对端必须是至少一个本组织集合的复制组成员；
//! - **collections 按「收件人 ∈ 该集合复制组」逐集合裁剪**：recipient 不在某
//!   集合复制组（如 data-accounts 集合的普通成员）则不向它发该集合。

use super::OrgSyncContext;
use crate::p2p::peer_targets::PeerNodeInfo;
use crate::storage::{ScanOptions, StorageBackend};

impl OrgSyncContext {
    /// orgsync-hello 触发：遍历组织与 org 集合，向已连接的复制组成员逐成员
    /// 发送 hello（每收件人独立裁剪 collections + 独立 dlogAck）。
    pub(crate) async fn maybe_send_orgsync_hello(&self, root_id: &str, connected: &std::collections::HashSet<String>) {
        let signing_key = self.signing_key.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let Some(signing_key) = signing_key else {
            return;
        };
        let now = self.now();
        let records =
            crate::org::OrganizationService::read_all_organizations(&self.storage).unwrap_or_default();
        for record in records {
            let org_id = &record.org_id;
            // 本机必须是成员
            if record.find_member(root_id).is_none() {
                continue;
            }
            // O2b 存量迁移（惰性）：为存量组织补注册内建 all-members 集合声明
            // （组织创建时已注册；存量组织首次走 orgsync 时补齐），声明先行
            // 随 hello/data 同步给其他成员。
            self.ensure_builtin_collections(org_id, root_id, now);
            // 扫描 org:coll: 前缀获取本组织的集合声明（含保留系统集合）
            let Ok(decls) = self
                .storage
                .scan(&ScanOptions::prefix(&format!("org:coll:{org_id}:")))
            else {
                continue;
            };
            // 收集 (collection 全名, name, version, accounts, confidentiality)
            // ——accounts 决定复制组（按收件人裁剪）；confidentiality 供 O3
            // hello 降标判定（filtered 集合无插件运行时支撑 → 标记不服务）。
            let mut collections: Vec<(
                String,
                String,
                String,
                crate::plugindata::Accounts,
                crate::plugindata::Confidentiality,
            )> = Vec::new();
            for (key, raw) in &decls {
                let Ok(decl) =
                    serde_json::from_str::<crate::plugindata::CollectionDeclaration>(raw)
                else {
                    continue;
                };
                let stem = key.strip_prefix(&format!("org:coll:{org_id}:")).unwrap_or(key);
                if let Some(at) = stem.rfind("@v") {
                    let name = &stem[..at];
                    let version = &stem[at + 2..];
                    collections.push((
                        stem.to_string(),
                        name.to_string(),
                        version.to_string(),
                        decl.accounts,
                        decl.confidentiality,
                    ));
                }
            }
            if collections.is_empty() {
                continue;
            }
            let roles = crate::sync::orgsync::self_roles(&record, root_id, now);
            let device_class = crate::sync::pdsync::local_device_class();

            // 遍历本组织其他成员，仅向复制组成员逐成员发送
            for member in &record.members {
                if member.root_id == root_id {
                    continue;
                }
                let Some(set) = &member.node_info else {
                    continue;
                };
                // 端点化：只向已连接的对端设备发送（同成员多设备逐台发，
                // dlogAck 按设备粒度）。
                for node_info in set.iter() {
                    let Some(peer_id) = &node_info.peer_id else {
                        continue;
                    };
                    if !connected.contains(peer_id.as_str()) {
                        continue;
                    }
                    // 裁剪该收件人可见的集合（收件人 ∈ 该集合复制组）
                    let mut col_map = serde_json::Map::new();
                    for (col_full, name, version, accounts, confidentiality) in &collections {
                        if !crate::sync::orgsync::is_in_replication_group(
                            &record, &member.root_id, *accounts,
                        ) {
                            continue;
                        }
                        // 逐集合折叠 vv + 对收件人的 dlogAck
                        let Ok(vv) = crate::sync::orgsync::collect_org_collection_vv(
                            &self.storage, org_id, name, version,
                        ) else {
                            continue;
                        };
                        let Ok(dlog_ack) = crate::sync::orgsync::org_dlog_get_seen(
                            &self.storage, org_id, name, version, &member.root_id, peer_id,
                        ) else {
                            continue;
                        };
                        // O3 hello roles 降标：本机是数据账号、集合为 filtered、
                        // 且插件运行时未注册该集合的读/写钩子（filter_caps 无条目）
                        // → 该数据账号**不能服务**此 filtered 集合（只存不服务）。
                        // hello 摘要中标注 `degraded:true`（roles 仍为 data，语义
                        // 是不宣告此集合可服务），成员侧读到 degraded 即知 orgq
                        // 查询此集合会降级/应路由其他数据账号。插件未运行时的
                        // fail-closed 口径与 inbound_dm/orgq.rs 的只存不服务一致。
                        let mut entry = serde_json::json!({ "vv": vv, "dlogAck": dlog_ack });
                        let is_data = crate::org::roles::is_data_account(&record, root_id);
                        let is_filtered = matches!(
                            *confidentiality,
                            crate::plugindata::Confidentiality::Filtered
                        );
                        let degraded = hello_collection_degraded(
                            is_data,
                            is_filtered,
                            self.has_filter_cap(name, "read"),
                            self.has_filter_cap(name, "write"),
                        );
                        if degraded {
                            entry["degraded"] = serde_json::Value::Bool(true);
                        }
                        col_map.insert(col_full.clone(), entry);
                    }
                    if col_map.is_empty() {
                        continue;
                    }
                    let hello_body = crate::sync::orgsync::build_orgsync_hello(
                        org_id, col_map, &roles, device_class,
                    );
                    let target = PeerNodeInfo {
                        peer_id: Some(peer_id.clone()),
                        addresses: node_info.addresses.clone(),
                    };
                    let envelope = crate::kernel::dm_envelope::build_envelope(
                        crate::kernel::dm_envelope::KIND_ORGSYNC_HELLO,
                        root_id,
                        &member.root_id,
                        now,
                        hello_body,
                        &signing_key,
                    );
                    let _ = self.node.dm_direct(&target, envelope).await;
                }
            }
        }
    }

    /// O3 hello 降标判定：filter_caps 注册表是否含该集合指定种类的过滤钩子。
    /// `collection` 为不含 @v 代际的集合名（与 `data.onReadFilter`/`onWriteFilter`
    /// 注册键一致）。插件未运行即无条目 → false（fail-closed，与宿主口径一致）。
    fn has_filter_cap(&self, collection: &str, kind: &str) -> bool {
        self.filter_caps
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(collection)
            .is_some_and(|caps| caps.contains(kind))
    }

    /// O2b 存量迁移（惰性，幂等）：为组织补注册内建 all-members 集合声明
    /// （org:structure/org:contacts/org:invites）。新建组织在 create 时已注册；
    /// 存量组织首次走 orgsync 时经本方法补齐——使存量键域（org:meta/ct:org/
    /// org:inv）纳入 orgsync 反熵面（键不搬家、零迁移）。
    ///
    /// 声明记录走 **raw 句柄**写入（不经 VersionedStorage 自动版本化——本
    /// helper 手动写 pmeta，避免双 bump vv）。写入失败仅告警，不阻断 hello
    /// 发送（下次触发重试）。
    fn ensure_builtin_collections(&self, org_id: &str, declared_by: &str, now_ms: i64) {
        // F3：无条件调用（被调函数 `declare_builtin_org_collections` 逐集合
        // 幂等——已声明且策略一致跳过）。移除旧的 any() 闸门：半迁移态（只
        // 注册了部分内建集合）会被闸门误判为"已迁移"而漏注册其余集合；
        // 无条件调用由被调函数逐集合幂等兜底，恒收敛到全部注册。
        let mut storage = self.storage.clone();
        let raw = storage.raw_mut();
        let node_id = self.node.peer_id().to_string();
        if let Err(e) = crate::plugindata::declare_builtin_org_collections(
            raw,
            org_id,
            declared_by,
            now_ms,
            &node_id,
        ) {
            self.warn(format!("[orgsync] ensure builtin collections failed: {e}"));
        }
    }
}

// ------------------------------------------------------------------
// O3 工作项 4：hello roles 降标（纯决策，可单测）
// ------------------------------------------------------------------

/// 判定某集合在本机发出的 orgsync-hello 中是否应标注 `degraded`（降标）。
///
/// 降标条件：本机是数据账号（`is_data`）∧ 集合为 filtered（`is_filtered`）∧
/// 插件运行时未注册该集合的读/写过滤钩子（`has_read_cap`/`has_write_cap` 均
/// false，即 filter_caps 无条目，fail-closed 只存不服务）。
///
/// 语义：roles 仍为 data（本机确为数据账号），但该 filtered 集合因插件未运行
/// 而**不能服务 orgq 查询/写入**，hello 摘要标注 `degraded:true` 不宣告可服务
/// ——成员侧读到即知此数据账号对该集合是降级态（orgq 会回 denied 或应路由
/// 其他数据账号）。
fn hello_collection_degraded(
    is_data: bool,
    is_filtered: bool,
    has_read_cap: bool,
    has_write_cap: bool,
) -> bool {
    is_data && is_filtered && !has_read_cap && !has_write_cap
}

#[cfg(test)]
mod tests {
    use super::*;

    /// O3 hello roles 降标：数据账号 + filtered 集合 + 无插件运行时钩子 →
    /// 降标；注册任一钩子（插件运行）→ 不降标；非数据账号 / 非 filtered → 不降标。
    #[test]
    fn hello_collection_degraded_semantics() {
        // 数据账号 + filtered + 无钩子（插件未运行）→ 降标
        assert!(hello_collection_degraded(true, true, false, false));
        // 注册读钩子（插件运行中可服务查询）→ 不降标
        assert!(!hello_collection_degraded(true, true, true, false));
        // 注册写钩子（插件运行中）→ 不降标
        assert!(!hello_collection_degraded(true, true, false, true));
        // 非数据账号 → 不降标（普通成员本就不宣告 data 服务）
        assert!(!hello_collection_degraded(false, true, false, false));
        // 非 filtered（encrypted/all-members）→ 不降标
        assert!(!hello_collection_degraded(true, false, false, false));
    }
}

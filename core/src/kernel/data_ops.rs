//! P6 声明式数据 API 的 `Kernel` 门面（iframe 桥 / Tauri 命令侧入口；
//! QuickJS 后台运行时侧的同语义入口在 `plugin::host_env` 的 `data.*`
//! capability）。两侧共享 `plugindata` 模块语义，写入均经版本化句柄
//! （写库即同步）。
//!
//! `domain` 参数沿用桥层口径（`plugin:{pluginId}` 或裸 pluginId），本模块
//! 统一归一为裸 pluginId 再做前缀归属校验。

use serde_json::Value;

use super::data_orgq::WriteRoute;
use super::{Kernel, Result};
use crate::plugindata::{self, DeclareInput};

/// 桥层 domain → 裸 pluginId（`plugin:ai-chat` → `ai-chat`；无前缀原样）。
fn plugin_id_of(domain: &str) -> &str {
    domain.strip_prefix("plugin:").unwrap_or(domain)
}

/// 归属校验 + 声明解析：插件只能触达自己前缀的集合（与 host_env 同口径）。
fn resolve_owned<S: crate::storage::StorageBackend>(
    storage: &S,
    domain: &str,
    name: &str,
    version: Option<&str>,
) -> Result<crate::plugindata::CollectionDeclaration> {
    let plugin_id = plugin_id_of(domain);
    check_prefix(plugin_id, name)?;
    Ok(plugindata::resolve(storage, name, version)?)
}

/// O7：org 分支（Some(oid) → resolve_org）同样强制插件前缀归属——与个人路径
/// 对称（host_env 的 resolve_data_declaration 对 org 集合也先查前缀）。
/// 防插件以任意 org_id + 他插件前缀越权触达他插件集合。
fn resolve_org_owned<S: crate::storage::StorageBackend>(
    storage: &S,
    domain: &str,
    org_id: &str,
    name: &str,
    version: Option<&str>,
) -> Result<crate::plugindata::CollectionDeclaration> {
    let plugin_id = plugin_id_of(domain);
    check_prefix(plugin_id, name)?;
    Ok(plugindata::resolve_org(storage, org_id, name, version)?)
}

/// 集合名必须归属插件前缀 `{plugin_id}:`（与 host_env 同口径）。
fn check_prefix(plugin_id: &str, name: &str) -> Result<()> {
    if !name.starts_with(&format!("{plugin_id}:")) {
        return Err(crate::plugindata::PlugindataError::NamePrefixMismatch {
            name: name.to_string(),
            plugin_id: plugin_id.to_string(),
        }
        .into());
    }
    Ok(())
}

impl Kernel {
    /// 声明集合（幂等；代际内策略冲突报错）。返回声明记录。
    /// `org_id`：org space 时传入组织 ID；personal space 时传 `None`。
    pub fn data_declare_collection(
        &mut self,
        domain: &str,
        input: DeclareInput,
        org_id: Option<&str>,
    ) -> Result<crate::plugindata::CollectionDeclaration> {
        // F10：declaredBy 防伪造——kernel 侧强制覆盖为调用方 rootId，不信任
        // 插件自报（声明记录属审计面，declaredBy 须可信）。
        let my_root = self.require_current_root_id()?;
        let mut input = input;
        input.declared_by = Some(my_root.clone());
        // F9：org space 声明须校验调用方确为该组织成员（orgId 来源可信）——
        // 对齐 host_env.rs 的 data_declare_collection 成员资格校验，防止桥层以
        // 任意 org_id 越权声明组织集合。iframe 桥按插件实例绑定的 orgId 校验。
        if input.space == Some(crate::plugindata::Space::Org) {
            let Some(oid) = org_id else {
                return Err(super::KernelError::Internal(
                    "org space declaration requires orgId".to_string(),
                ));
            };
            let storage = self.require_storage()?;
            let is_member = crate::org::OrganizationService::get_record(storage, oid)
                .ok()
                .flatten()
                .is_some_and(|rec| rec.find_member(&my_root).is_some());
            if !is_member {
                return Err(super::KernelError::Internal(format!(
                    "caller {my_root} is not a member of org {oid}"
                )));
            }
        }
        let storage = self.require_storage_mut()?;
        Ok(plugindata::declare(
            storage,
            plugin_id_of(domain),
            input,
            crate::p2p::node::system_now_ms(),
            org_id,
        )?)
    }

    /// 写记录（version 缺省 = 最新代际）。
    ///
    /// `org_id`：org scope 集合时传入组织 ID（personal scope 传 `None`）。
    /// O3 工作项 3/4：本机为**非数据账号**写入 org data-accounts 集合——
    /// 有在线数据账号走 orgq-req 实时受理（denied → AccessDenied）；全部离线
    /// 写入本地 orgq 队列（数据账号上线后经统一同步链路冲刷），而非直接落
    /// `orgd:` 副本或报错。
    pub fn data_save(
        &mut self,
        domain: &str,
        name: &str,
        key: &str,
        value: Value,
        version: Option<&str>,
        org_id: Option<&str>,
    ) -> Result<()> {
        let storage = self.require_storage()?;
        let decl = match org_id {
            Some(oid) => resolve_org_owned(storage, domain, oid, name, version)?,
            None => resolve_owned(storage, domain, name, version)?,
        };
        // nit：Online 分支仅在 org 集合时返回（org_id 有值），用 if let 显式
        // 解构避免 unwrap。
        match self.data_org_write_route(&decl, org_id, key, &value)? {
            WriteRoute::Local => {}
            WriteRoute::Enqueued => return Ok(()),
            WriteRoute::Online { target_root_id } => {
                let Some(oid) = org_id else {
                    return Ok(());
                };
                match self.data_orgq_write(oid, &decl, &target_root_id, key, &value)? {
                    Some(true) => return Ok(()), // 受理（数据账号侧已落库，随复制组扩散）
                    Some(false) => return Err(super::KernelError::AccessDenied),
                    // 超时/失败 → 回退离线入队（不丢写）
                    None => {
                        self.data_org_enqueue(oid, &decl, key, &value);
                        return Ok(());
                    }
                }
            }
        }
        let storage = self.require_storage_mut()?;
        Ok(plugindata::save(storage, &decl, key, &value.to_string())?)
    }

    /// 删记录（墓碑传播）。
    pub fn data_delete(
        &mut self,
        domain: &str,
        name: &str,
        key: &str,
        version: Option<&str>,
        org_id: Option<&str>,
    ) -> Result<()> {
        let storage = self.require_storage()?;
        let decl = match org_id {
            Some(oid) => resolve_org_owned(storage, domain, oid, name, version)?,
            None => resolve_owned(storage, domain, name, version)?,
        };
        match self.data_org_write_route(&decl, org_id, key, &Value::Null)? {
            WriteRoute::Local => {}
            WriteRoute::Enqueued => return Ok(()),
            WriteRoute::Online { target_root_id } => {
                let Some(oid) = org_id else {
                    return Ok(());
                };
                match self.data_orgq_write(oid, &decl, &target_root_id, key, &Value::Null)? {
                    Some(true) => return Ok(()), // 受理（数据账号侧已落墓碑）
                    // F5：denied → AccessDenied（与 data_save 对称，不吞 denied）
                    Some(false) => return Err(super::KernelError::AccessDenied),
                    // 超时/失败 → 回退离线入队（不丢删）
                    None => {
                        self.data_org_enqueue(oid, &decl, key, &Value::Null);
                        return Ok(());
                    }
                }
            }
        }
        let storage = self.require_storage_mut()?;
        Ok(plugindata::del(storage, &decl, key)?)
    }

    /// 读单条（未命中 → None）。
    ///
    /// `org_id`：org scope 集合时传入组织 ID（personal scope 传 `None`）。
    /// O3 读路径透明路由：非数据账号本机读 org data-accounts 集合走 orgq——
    /// 数据账号离线时回**成员侧缓存**（UI 标注陈旧），无缓存 → `None`（等价
    /// UnavailableOffline）；本机是数据账号 / all-members 集合直接读本地。
    pub fn data_get(
        &self,
        domain: &str,
        name: &str,
        key: &str,
        version: Option<&str>,
        org_id: Option<&str>,
    ) -> Result<Option<Value>> {
        let storage = self.require_storage()?;
        let decl = match org_id {
            Some(oid) => resolve_org_owned(storage, domain, oid, name, version)?,
            None => resolve_owned(storage, domain, name, version)?,
        };
        // org 集合：本机驻留（数据账号 / all-members）→ 读本地；非驻留
        // （普通成员读 data-accounts）→ 有在线数据账号走 orgq-req 投递后读
        // 缓存，全部离线回缓存（无缓存 → None=UnavailableOffline 语义）。
        if let Some(oid) = org_id {
            let resident = self.data_org_local_resident(&decl, storage)?;
            if !resident {
                let col_full = format!("{}@v{}", decl.name, decl.version);
                if let Some(target) = self.orgq_online_target(oid, &col_full) {
                    // 投递 orgq-req 查询（prefix=key 精确拉取），等待应答落缓存
                    let _ =
                        self.data_orgq_query(domain, oid, &decl, &target, Some(key), Some(1), None)?;
                }
                return self.data_orgq_cached_get(storage, &decl, key);
            }
        }
        let raw = plugindata::get(storage, &decl, key)?;
        Ok(match raw {
            // C7 后 orgd 值恒为明文（encrypted 轴已退役）
            Some(text) => Some(serde_json::from_str(&text).unwrap_or(Value::String(text))),
            None => None,
        })
    }

    /// O3 读路径驻留判定：本机对某 org 集合是否本地驻留——数据账号（对
    /// data-accounts 天然驻留）或 all-members 集合（全员驻留）→ 本地直读；
    /// 普通成员对 data-accounts 恒非驻留（走 orgq 路由）。
    fn data_org_local_resident<S: crate::storage::StorageBackend>(
        &self,
        decl: &crate::plugindata::CollectionDeclaration,
        storage: &S,
    ) -> Result<bool> {
        let my_root = self.require_current_root_id()?;
        let Some(oid) = decl.org_id.as_deref() else {
            return Ok(true);
        };
        if decl.accounts == crate::plugindata::Accounts::AllMembers {
            return Ok(true);
        }
        let Ok(Some(record)) = crate::org::OrganizationService::get_record(storage, oid) else {
            return Ok(false);
        };
        Ok(crate::org::roles::is_data_account(&record, &my_root))
    }

    /// 前缀分页查询。
    pub fn data_query(
        &self,
        domain: &str,
        name: &str,
        prefix: Option<&str>,
        limit: Option<usize>,
        cursor: Option<&str>,
        version: Option<&str>,
        org_id: Option<&str>,
    ) -> Result<crate::plugindata::QueryPage> {
        let storage = self.require_storage()?;
        let decl = match org_id {
            Some(oid) => resolve_org_owned(storage, domain, oid, name, version)?,
            None => resolve_owned(storage, domain, name, version)?,
        };
        // org 集合：本机驻留 → 读本地；非驻留 → 有在线数据账号走 orgq-req
        // 投递后从缓存读，全部离线回缓存/空页。
        if let Some(oid) = org_id {
            if !self.data_org_local_resident(&decl, storage)? {
                let col_full = format!("{}@v{}", decl.name, decl.version);
                if let Some(target) = self.orgq_online_target(oid, &col_full) {
                    return self.data_orgq_query(domain, oid, &decl, &target, prefix, limit, cursor);
                }
                return Ok(self.data_orgq_cached_query(storage, &decl, prefix, limit, cursor)?);
            }
        }
        let page = plugindata::query(storage, &decl, prefix, limit, cursor)?;
        Ok(page)
    }

    /// 清理一个代际（声明 + 全部数据键墓碑化传播）。
    pub fn data_drop_version(&mut self, domain: &str, name: &str, version: &str) -> Result<()> {
        let decl = resolve_owned(self.require_storage()?, domain, name, Some(version))?;
        let storage = self.require_storage_mut()?;
        Ok(plugindata::drop_version(storage, &decl)?)
    }

    /// blob 保存（base64 入、内容哈希出）。
    pub fn data_save_blob(
        &mut self,
        data_base64: &str,
    ) -> Result<crate::plugindata::blob::BlobInfo> {
        use base64::Engine as _;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(data_base64)
            .map_err(|e| {
                crate::plugindata::PlugindataError::Blob(format!("invalid base64: {e}"))
            })?;
        let storage = self.require_storage_mut()?;
        Ok(plugindata::blob::save_blob(storage, &bytes)?)
    }

    /// O3 成员侧读路由决策：org 数据-accounts 集合对**非数据账号**本机按需
    /// 查询的路由规划（本地直读 / 走 orgq / 全离线回缓存或 UnavailableOffline）。
    ///
    /// `online_peer_ids` 为当前在线 libp2p peerId 集合（事件循环快照，host
    /// orgq 发送方从连接层取）。本方法只做**决策**（纯逻辑、无副作用）——
    /// 实际 orgq-req 的异步投递与应答关联由 host 层完成（读路径透明路由的
    /// 宿主接线点）；本机是数据账号或集合非 data-accounts 时恒 `Local`。
    pub fn data_orgq_read_plan(
        &self,
        org_id: &str,
        name: &str,
        version: &str,
        online_peer_ids: &std::collections::HashSet<String>,
    ) -> Result<crate::sync::orgsync::MemberReadPlan> {
        let storage = self.require_storage()?;
        let my_root = self.require_current_root_id()?;
        let Some(record) = crate::org::OrganizationService::get_record(storage, org_id)
            .ok()
            .flatten()
        else {
            return Ok(crate::sync::orgsync::MemberReadPlan::Local);
        };
        let decl = crate::plugindata::get_declaration_org(storage, org_id, name, version)
            .map_err(super::KernelError::from)?
            .ok_or_else(|| {
                super::KernelError::Internal(format!(
                    "collection {name}@v{version} not declared in org {org_id}"
                ))
            })?;
        // 本地驻留判定：数据账号（对 data-accounts 集合天然驻留）或 all-members
        // 集合（全员驻留）→ 直接读本地。普通成员对 data-accounts 恒非驻留。
        let is_data = crate::org::roles::is_data_account(&record, &my_root);
        let is_all_members = decl.accounts == crate::plugindata::Accounts::AllMembers;
        let local_resident = is_data || is_all_members;
        let col_full = format!("{name}@v{version}");
        let cache_populated = crate::sync::orgsync::orgq_cache_has_data(storage, org_id, &col_full);
        let degraded =
            crate::sync::orgsync::orgq_degraded_for_collection(storage, org_id, &col_full);
        Ok(crate::sync::orgsync::member_orgq_read_plan(
            &record,
            &my_root,
            online_peer_ids,
            local_resident,
            cache_populated,
            &degraded,
        ))
    }

    /// blob 读取（命中 → base64；未命中 → 置 want 标记后返回 None——lazy
    /// 拉取由 pdsync hello 调和完成，调用方稍后重读）。
    pub fn data_read_blob(&mut self, hash: &str) -> Result<Option<String>> {
        if let Some(data) = plugindata::blob::read_blob(self.require_storage()?, hash)? {
            return Ok(Some(data));
        }
        let storage = self.require_storage_mut()?;
        plugindata::blob::mark_want(storage, hash)?;
        Ok(None)
    }
}

#[cfg(test)]
#[path = "data_ops_tests.rs"]
mod tests;

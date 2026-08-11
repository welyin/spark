//! O4 插件 API（QuickJS 通路）的访问控制能力（工作项 5）：
//! `data.grantAccess` / `data.revokeAccess` / `data.listAccess`。
//!
//! 对应 plugin-data-api §5.2（encrypted 授权名单）。分工：插件按自己的权限
//! 模型评估「谁该有访问权」，把结果物化为名单经本模块维护；内核（access.rs
//! 纯逻辑 + 本模块）管密码学机械层（acl 签名落库、epoch 密钥生成/轮换、
//! orgkey-deliver 定向投递），插件不接触密钥。
//!
//! 名单管理权属于 owner；变更经组织域身份签名、落 `org:acl:`（all-members
//! 系统数据，随 orgsync 全员同步）。owner 独立于 spark 管理员。
//!
//! 本模块为 `PluginHostShared` 的方法（插件后台运行时线程调用，不持
//! `Mutex<Kernel>`——沿用 `*_shared` 模式：存储镜像 + seed + p2p 节点句柄
//! 均为共享格克隆）。与 `kernel/data_access.rs` 的 `Kernel::data_grant_access`
//! 家族语义一致（同一份 acl 落库/密钥逻辑），仅出站投递走本宿主 p2p 节点。

use serde_json::{Value, json};

use super::error::{PluginError, Result};
use super::host_env::PluginHostShared;
use crate::p2p::peer_targets::PeerNodeInfo;
use crate::storage::StorageBackend;

/// 组织域身份域串（与 Kernel::org_access_domain 同口径）。
fn org_access_domain(org_id: &str) -> String {
    format!("org-access:{org_id}")
}

impl PluginHostShared {
    /// 当前解锁 rootId（能力归属校验：只对本账号组织身份操作）。
    fn current_root(&self) -> Result<String> {
        self.my_root_id
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .ok_or(PluginError::InvalidCall("not unlocked".to_string()))
    }

    /// 读取某集合当前 acl（缺省空记录）。
    fn read_acl(
        &self,
        storage: &crate::kernel::KernelStorage,
        org_id: &str,
        name: &str,
        version: &str,
    ) -> crate::sync::orgsync::AclRecord {
        storage
            .get(&crate::sync::orgsync::acl_key(org_id, name, version))
            .ok()
            .flatten()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or(crate::sync::orgsync::AclRecord {
                owners: Vec::new(),
                readers: Vec::new(),
                epoch: 0,
                updated_at: 0,
                reset_by: None,
                sig: String::new(),
            })
    }

    /// 以本机组织域身份签 acl 并落库。`epoch`/`readers`/`reset_by` 为写入后
    /// 的最终名单字段。返回写入的 acl。
    fn sign_and_put_acl(
        &self,
        storage: &mut crate::kernel::KernelStorage,
        org_id: &str,
        name: &str,
        version: &str,
        epoch: u64,
        owners: Vec<String>,
        readers: Vec<String>,
        reset_by: Option<String>,
    ) -> Result<crate::sync::orgsync::AclRecord> {
        let seed = self
            .seed_shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .ok_or(PluginError::InvalidCall("not unlocked".to_string()))?;
        let domain = org_access_domain(org_id);
        let derived = crate::identity::derive_domain_identity(&seed, &domain);
        let now = crate::p2p::node::system_now_ms();
        let col_full = format!("{name}@v{version}");
        let payload = crate::sync::orgsync::acl_sign_payload(
            epoch, org_id, &col_full, &owners, &readers, reset_by.as_deref(), now,
        );
        let sig = crate::sync::orgsync::acl_sign(&derived.signing_key, &payload);
        let acl = crate::sync::orgsync::AclRecord {
            owners,
            readers,
            epoch,
            updated_at: now,
            reset_by,
            sig,
        };
        storage.put(
            &crate::sync::orgsync::acl_key(org_id, name, version),
            &serde_json::to_string(&acl).map_err(|e| PluginError::InvalidCall(e.to_string()))?,
        )?;
        Ok(acl)
    }

    /// O4 出站投递（`PluginHostShared` 侧）：把集合密钥逐 epoch 定向投递给
    /// readers（orgkey-deliver dm 信封，§20.6）。与
    /// `Kernel::deliver_orgkey_to_readers` 语义一致，仅走宿主 p2p 节点。
    /// 收件人无 accessKey → 跳过；离线经退避重试（2s/5s）。
    fn deliver_orgkey(
        &self,
        org_id: &str,
        name: &str,
        version: &str,
        recipients: &[String],
        epochs: std::ops::RangeInclusive<u64>,
    ) {
        let Some(seed) = self
            .seed_shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        else {
            return;
        };
        let Some(node) = self
            .p2p_node
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        else {
            log::info!("[ORGKEY] deliver skip: p2p not running | org={org_id}");
            return;
        };
        let Some(sender_root_id) = self
            .my_root_id
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        else {
            return;
        };
        // 根私钥：dm 信封必须由根身份签名（from==rootId 绑定，R1；与
        // Kernel::build_dm_envelope 同口径）。body 内 sig 仍为 owner 组织域
        // 身份签名（验签锚 = 成员表 accessKey，§20.6）。
        let Some(root_key) = self
            .signing_key
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        else {
            log::info!("[ORGKEY] deliver skip: no root signing key | org={org_id}");
            return;
        };
        let Ok(storage) = self.require_storage() else {
            return;
        };
        let Ok(Some(record)) = crate::org::OrganizationService::get_record(&storage, org_id) else {
            return;
        };
        let derived = crate::identity::derive_domain_identity(&seed, &org_access_domain(org_id));
        let owner_x25519_priv = crate::sync::orgsync::ed_sk_to_x25519(&derived.signing_key.to_bytes());
        let now = crate::p2p::node::system_now_ms();
        let col_full = format!("{name}@v{version}");
        let mut deliveries: Vec<(PeerNodeInfo, Value)> = Vec::new();
        for recipient in recipients {
            let Some(member) = record.find_member(recipient) else { continue };
            let Some(access_key) = member.access_key.as_ref() else { continue };
            use base64::Engine as _;
            let Ok(pk_bytes) =
                base64::engine::general_purpose::STANDARD.decode(&access_key.public_key)
            else {
                continue;
            };
            let Ok(pk_arr) = <[u8; 32]>::try_from(pk_bytes.as_slice()) else { continue };
            let Some(recipient_x25519) = crate::sync::orgsync::ed_pk_to_x25519(&pk_arr) else {
                continue;
            };
            let Some(node_info) = member.node_info.clone() else { continue };
            for epoch in epochs.clone() {
                let Some(epoch_key) =
                    crate::sync::orgsync::get_epoch_key(&storage, org_id, name, version, epoch)
                else {
                    continue;
                };
                let Some(body) = crate::sync::orgsync::build_orgkey_deliver(
                    org_id,
                    name,
                    version,
                    epoch,
                    &epoch_key,
                    &sender_root_id,
                    recipient,
                    &recipient_x25519,
                    &derived.signing_key,
                    &owner_x25519_priv,
                    now,
                ) else {
                    continue;
                };
                // dm 信封用**根私钥**签名（from==rootId 绑定，R1）；body 内 sig
                // 保持 org-access 域身份签名（验签锚 = 成员表 accessKey）。
                let envelope = crate::kernel::dm_envelope::build_envelope(
                    crate::kernel::dm_envelope::KIND_ORGKEY_DELIVER,
                    &sender_root_id,
                    recipient,
                    now,
                    body,
                    &root_key,
                );
                for info in node_info.iter() {
                    deliveries.push((
                        PeerNodeInfo {
                            peer_id: info.peer_id.clone(),
                            addresses: info.addresses.clone(),
                        },
                        envelope.clone(),
                    ));
                }
            }
        }
        let _ = col_full;
        if deliveries.is_empty() {
            return;
        }
        // spawn 到内核 runtime 逐个投递（2s/5s 退避，与 dm_delivery 同口径）
        self.runtime.spawn(async move {
            for (peer, envelope) in deliveries {
                let mut result = node.dm_direct(&peer, envelope.clone()).await;
                for delay_ms in [2000u64, 5000] {
                    let needs_retry = match &result {
                        Ok(Some(resp)) => !resp.get("ok").and_then(Value::as_bool).unwrap_or(false),
                        _ => true,
                    };
                    if !needs_retry {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                    result = node.dm_direct(&peer, envelope.clone()).await;
                }
            }
        });
    }

    /// R2：解析 access 方法的集合声明——强制集合名前缀归属（`{plugin_id}:`
    /// 前缀，与 `resolve_data_declaration` 同口径）+ **orgId 按运行空间绑定**
    /// （取声明记录的 `org_id`，不接受 payload 自报 orgId——插件不可越权
    /// 操作其它组织的 encrypted 集合）。返回 (org_id, name, version, storage)。
    fn resolve_access_decl(
        &self,
        plugin_id: &str,
        payload: &Value,
    ) -> Result<(String, String, String, crate::kernel::KernelStorage)> {
        let name = req_str(payload, "collection")?;
        // 前缀归属（与 resolve_data_declaration 同口径）：插件只能触达自己
        // 前缀的集合——grant/revoke/list 是 owner 侧能力，越权操作他插件/他
        // 组织的集合即越权改名单。
        if !name.starts_with(&format!("{plugin_id}:")) {
            return Err(PluginError::InvalidCall(format!(
                "collection {name:?} does not belong to plugin {plugin_id}"
            )));
        }
        let version = payload
            .get("version")
            .and_then(Value::as_str)
            .unwrap_or("1")
            .to_string();
        let storage = self.require_storage()?;
        // orgId 绑定注入：取声明记录的 org_id（声明在创建时以插件运行空间
        // 绑定，是可信锚）——不接受 payload 自报 orgId（自报可越权到其它组织）。
        let decl = crate::plugindata::resolve(&storage, name, Some(&version))
            .map_err(|e| PluginError::InvalidCall(e.to_string()))?;
        let org_id = decl.org_id.ok_or_else(|| {
            PluginError::InvalidCall(format!(
                "collection {name}@v{version} is not an org (encrypted) collection"
            ))
        })?;
        Ok((org_id, name.to_string(), version, storage))
    }

    /// R2：调用方须为该组织成员（grant/revoke 名单是组织数据，非成员无
    /// 资格操作）。
    fn require_org_member(
        &self,
        storage: &crate::kernel::KernelStorage,
        org_id: &str,
        my_root: &str,
    ) -> Result<()> {
        let is_member = crate::org::OrganizationService::get_record(storage, org_id)
            .ok()
            .flatten()
            .is_some_and(|rec| rec.find_member(my_root).is_some());
        if !is_member {
            return Err(PluginError::InvalidCall(format!(
                "caller {my_root} is not a member of org {org_id}"
            )));
        }
        Ok(())
    }

    /// `data.grantAccess(name@v, [members])`：owner 将成员加入 readers。内核
    /// 生成/保留当前 epoch 密钥并落 orgkey 表；对新读者逐 epoch 投递密钥
    /// （orgkey-deliver）。
    pub(crate) fn data_grant_access(&self, plugin_id: &str, payload: &Value) -> Result<Value> {
        let (org_id, name, version, mut storage) = self.resolve_access_decl(plugin_id, payload)?;
        let members: Vec<String> = payload
            .get("members")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
            .unwrap_or_default();
        let my_root = self.current_root()?;
        self.require_org_member(&storage, &org_id, &my_root)?;
        let _io = self.io_lock.lock().unwrap_or_else(|e| e.into_inner());
        let acl = self.read_acl(&storage, &org_id, &name, &version);
        // owner 校验：创世（空记录）→ 声明者为首个 owner；否则须为 owner。
        let is_owner = acl.is_empty() || acl.is_owner(&my_root);
        if !is_owner {
            return Err(PluginError::InvalidCall("Access denied".to_string()));
        }
        let mut readers = acl.readers.clone();
        let new_readers: Vec<String> = members
            .iter()
            .filter(|r| !readers.contains(r))
            .cloned()
            .collect();
        for r in members {
            if !readers.contains(&r) {
                readers.push(r);
            }
        }
        let owners = if acl.is_empty() {
            vec![my_root.clone()]
        } else {
            acl.owners.clone()
        };
        let epoch = acl.epoch.max(1);
        let new_acl = self.sign_and_put_acl(
            &mut storage,
            &org_id,
            &name,
            &version,
            epoch,
            owners,
            readers,
            None,
        )?;
        // 创世：生成并落 epoch=1 密钥（owner 写数据资格；新读者逐 epoch 收历史）。
        // O6：orgkey 写经版本化句柄（orgkey: 已注册 pdsync category，自设备扩散）。
        if acl.is_empty() {
            let key = crate::sync::orgsync::generate_epoch_key();
            crate::sync::orgsync::put_epoch_key(
                &mut storage,
                &org_id,
                &name,
                &version,
                epoch,
                &key,
            );
        }
        drop(_io);
        if !new_readers.is_empty() {
            self.deliver_orgkey(&org_id, &name, &version, &new_readers, 1..=epoch);
        }
        serde_json::to_value(&new_acl).map_err(|e| PluginError::InvalidCall(e.to_string()))
    }

    /// `data.revokeAccess(name@v, [members])`：owner 将成员移出 readers，
    /// epoch+1 并生成新密钥（新数据对已移出成员不可读）。对剩余 readers
    /// 投递新 epoch。
    pub(crate) fn data_revoke_access(&self, plugin_id: &str, payload: &Value) -> Result<Value> {
        let (org_id, name, version, mut storage) = self.resolve_access_decl(plugin_id, payload)?;
        let remove: Vec<String> = payload
            .get("members")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
            .unwrap_or_default();
        let my_root = self.current_root()?;
        self.require_org_member(&storage, &org_id, &my_root)?;
        let _io = self.io_lock.lock().unwrap_or_else(|e| e.into_inner());
        let acl = self.read_acl(&storage, &org_id, &name, &version);
        if !acl.is_owner(&my_root) {
            return Err(PluginError::InvalidCall("Access denied".to_string()));
        }
        let readers: Vec<String> = acl
            .readers
            .iter()
            .filter(|r| !remove.contains(r))
            .cloned()
            .collect();
        let remaining = readers.clone();
        let epoch = acl.epoch + 1;
        let new_acl = self.sign_and_put_acl(
            &mut storage,
            &org_id,
            &name,
            &version,
            epoch,
            acl.owners.clone(),
            readers,
            None,
        )?;
        let new_key = crate::sync::orgsync::generate_epoch_key();
        crate::sync::orgsync::put_epoch_key(
            &mut storage,
            &org_id,
            &name,
            &version,
            epoch,
            &new_key,
        );
        drop(_io);
        if !remaining.is_empty() {
            self.deliver_orgkey(&org_id, &name, &version, &remaining, epoch..=epoch);
        }
        serde_json::to_value(&new_acl).map_err(|e| PluginError::InvalidCall(e.to_string()))
    }

    /// `data.listAccess(name@v)` → `{ owners, readers, epoch }`。
    pub(crate) fn data_list_access(&self, plugin_id: &str, payload: &Value) -> Result<Value> {
        let (org_id, name, version, storage) = self.resolve_access_decl(plugin_id, payload)?;
        let acl = self.read_acl(&storage, &org_id, &name, &version);
        Ok(json!({
            "owners": acl.owners,
            "readers": acl.readers,
            "epoch": acl.epoch,
        }))
    }
}

// 保留 `req_str`；`req_collection` 已由 `resolve_access_decl` 取代（R2：orgId
// 按声明绑定注入，不再从 payload 自报）。

/// 取必填字符串字段。
fn req_str<'a>(payload: &'a Value, field: &str) -> Result<&'a str> {
    payload
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| PluginError::InvalidCall(format!("missing string field: {field}")))
}

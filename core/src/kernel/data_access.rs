//! O4 encrypted 授权名单 API（owner 侧，plugin-data-api §5.2）：
//! `grantAccess` / `revokeAccess` / `listAccess`。
//!
//! 名单管理权属于 owner（插件声明时指定的业务负责人），不属于 spark 管理员；
//! 内核按 owner 签名校验，不理解插件权限逻辑。变更经组织域身份签名、落
//! `org:acl:{orgId}:{name}@v{version}`（all-members 系统数据，随 orgsync 全员
//! 同步）；密钥生命周期（生成/轮换/自设备扩散）由内核完成。
//!
//! 组织域身份 = `derive_domain_identity(seed, "org-access:{orgId}")`（全员同域串
//! 派生各自 Ed25519 组织身份，密钥即时派生不持久化）。acl 变更由
//! `sign_with_domain_identity` 签名。
//!
//! 本文件从 `data_ops.rs` 拆分（Z5 650 行硬线），`impl Kernel` 分文件挂载。
//!
//! ## 边界（host 层接线点）
//!
//! orgkey-deliver 信封对新增读者/剩余读者的实际 dm 定向投递（含离线暂存）由
//! host 层完成——本文件只负责 acl 落库与密钥表写入；内核的加密值机械层见
//! `sync/orgsync/access.rs`。

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use serde_json::Value;

use super::dm_delivery::DM_RETRY_DELAYS;
use super::{Kernel, Result};
use crate::org::OrganizationService;
use crate::org::types::access_key_bind_payload;
use crate::storage::StorageBackend;
use crate::sync::orgsync;

impl Kernel {
    /// 组织域身份域串（acl/orgkey-deliver 签名与密钥派生用）：按 org 隔离。
    /// 全员以同域串派生各自组织身份（Ed25519），密钥即时派生不持久化。
    pub fn org_access_domain(org_id: &str) -> String {
        format!("org-access:{org_id}")
    }

    /// O4 发布本人组织身份访问密钥（`OrganizationMember.accessKey`）：派生
    /// `org-access:{orgId}` 域身份公钥 + 根密钥对该公钥的绑定签名，落本人成员
    /// 记录（仅本人可改，经快照 members 段全员传播）。
    ///
    /// 供 members/owners 首次启用 encrypted 能力时惰性调用——发布后全员经
    /// org:structure 集合同步获得，owner 侧据此取成员公钥验 acl/orgkey-deliver
    /// 签名。幂等（accessKey 未变不 bump）。
    pub fn org_publish_access_key(&mut self, org_id: &str) -> Result<()> {
        let root_id = self.require_unlocked_root_id()?;
        let domain = Self::org_access_domain(org_id);
        let derived = self.derive_domain_identity(&domain)?;
        let bind_payload = access_key_bind_payload(org_id, &derived.public_key);
        let bind = self.sign(&bind_payload)?;
        let ak = crate::org::types::OrganizationAccessKey {
            public_key: derived.public_key,
            bind_sig: bind.signature,
        };
        let node_id = self.sync_node_id();
        let io_lock = std::sync::Arc::clone(&self.io_lock);
        let _record = OrganizationService::publish_access_key_pdsync(
            self.require_storage_mut()?,
            &io_lock,
            org_id,
            &ak,
            &root_id,
            crate::p2p::node::system_now_ms(),
            &node_id,
        )?;
        Ok(())
    }

    /// 授权名单（`listAccess` → { owners, readers, epoch }）。
    pub fn data_list_access(
        &self,
        org_id: &str,
        name: &str,
        version: &str,
    ) -> Result<orgsync::AclRecord> {
        let storage = self.require_storage()?;
        let acl_key = orgsync::acl_key(org_id, name, version);
        let raw = storage.get(&acl_key)?;
        let acl: orgsync::AclRecord =
            raw.and_then(|r| serde_json::from_str(&r).ok())
                .unwrap_or(orgsync::AclRecord {
                    owners: Vec::new(),
                    readers: Vec::new(),
                    epoch: 0,
                    updated_at: 0,
                    reset_by: None,
                    sig: String::new(),
                });
        Ok(acl)
    }

    /// 用本机组织域身份为 acl 签名并落库（managed → 随 orgsync 全员同步）。
    /// `epoch`/`readers`/`reset_by` 为写入后的最终名单字段。调用方须先校验
    /// 自身为 owner（或创世/reset）。返回写入的 acl。
    ///
    /// H2：本地 acl 写直接落库，不经 `apply_acl_record_verified`（那是远端合入
    /// 的验签+whole 合并路径）。取舍（注释口径）：本地写由本机 owner 自签
    /// （sign_with_domain_identity），可信锚是本机已解锁身份，无 TOCTOU 注入
    /// 面；对端合入时仍走 `apply_acl_record_verified` 验签兜底，故本地写与远端
    /// 验签的时间窗内本机状态是自洽的（恶意改 acl 的威胁来自远端，已在合入侧
    /// 拦截）。
    fn sign_and_put_acl(
        &mut self,
        org_id: &str,
        name: &str,
        version: &str,
        epoch: u64,
        owners: Vec<String>,
        readers: Vec<String>,
        reset_by: Option<String>,
    ) -> Result<orgsync::AclRecord> {
        let now = crate::p2p::node::system_now_ms();
        let domain = Self::org_access_domain(org_id);
        let col_full = format!("{name}@v{version}");
        let payload = orgsync::acl_sign_payload(
            epoch,
            org_id,
            &col_full,
            &owners,
            &readers,
            reset_by.as_deref(),
            now,
        );
        let sig_info = self.sign_with_domain_identity(&domain, &payload)?;
        let acl = orgsync::AclRecord {
            owners,
            readers,
            epoch,
            updated_at: now,
            reset_by,
            sig: sig_info.signature,
        };
        let storage = self.require_storage_mut()?;
        storage.put(
            &orgsync::acl_key(org_id, name, version),
            &serde_json::to_string(&acl)?,
        )?;
        Ok(acl)
    }

    /// 读取当前 acl 的 owners（校验调用方为 owner 的公共前置）。
    ///
    /// **创世特例**（org-orgsync.md §20.7）：acl 尚不存在（空记录）时，集合
    /// 声明者即初始 owner——首个 `grantAccess` 以调用方为 owner 自签创世 acl。
    fn require_owner(
        &self,
        org_id: &str,
        name: &str,
        version: &str,
    ) -> Result<(orgsync::AclRecord, String)> {
        let my_root = self.require_current_root_id()?;
        let acl = self.data_list_access(org_id, name, version)?;
        if acl.is_empty() {
            // 创世：声明者为首个 owner
            return Ok((acl, my_root));
        }
        if !acl.is_owner(&my_root) {
            return Err(super::KernelError::AccessDenied);
        }
        Ok((acl, my_root))
    }

    /// `grantAccess(name@v, [members])`：owner 将成员加入 readers。内核生成/
    /// 保留当前 epoch 密钥并落 orgkey 表（自设备经 pdsync 扩散；orgkey-deliver
    /// 信封对新增读者的实际投递由 host 层经 dm 定向完成）。
    pub fn data_grant_access(
        &mut self,
        org_id: &str,
        name: &str,
        version: &str,
        add_readers: &[String],
    ) -> Result<orgsync::AclRecord> {
        // O4 惰性发布：owner 侧首次启用 encrypted 能力即发布本人 accessKey
        // （org:structure 全员同步），供验签方取公钥验 acl 签名。
        self.org_publish_access_key(org_id)?;
        let (acl, my_root) = self.require_owner(org_id, name, version)?;
        let mut readers = acl.readers.clone();
        for r in add_readers {
            if !readers.contains(r) {
                readers.push(r.clone());
            }
        }
        // grant：epoch 不变，沿用当前密钥（新读者经 orgkey-deliver 收历史密钥）
        // 创世：owners = [声明者（首个 grant 调用方）] 自签。
        // 新增读者（此前不在 readers 的 add_readers 成员）——grant 对其逐 epoch
        // 投递历史密钥（§20.6：新读者可解密历史）。
        let new_readers: Vec<String> = add_readers
            .iter()
            .filter(|r| !acl.readers.contains(r))
            .cloned()
            .collect();
        let owners = if acl.is_empty() {
            vec![my_root]
        } else {
            acl.owners.clone()
        };
        let epoch = acl.epoch.max(1);
        let new_acl = self.sign_and_put_acl(org_id, name, version, epoch, owners, readers, None)?;
        // O4 工作项 4：创世（acl 先前为空 → epoch 从 0 跳到 1）时生成并落
        // 本机 orgkey 表 epoch=1 密钥——owner 侧写 encrypted 数据须持有当前
        // epoch 密钥（AEAD 语义：密钥持有者集合 = 写权限集合）。新读者经
        // orgkey-deliver 逐 epoch 收历史密钥（工作项 3 投递）。
        if acl.is_empty() {
            let key = orgsync::generate_epoch_key();
            // O6：orgkey 写经版本化句柄（orgkey: 已注册 pdsync category）——
            // put 即自动 pmeta 记账，自设备经 pdsync 扩散才真实生效。
            let storage = self.require_storage_mut()?;
            orgsync::put_epoch_key(storage, org_id, name, version, epoch, &key);
        }
        // O4 工作项 3（出站）：grant 对新读者逐 epoch(1..current) 投递密钥。
        if !new_readers.is_empty() {
            self.deliver_orgkey_to_readers(org_id, name, version, &new_readers, 1..=epoch);
        }
        Ok(new_acl)
    }

    /// `revokeAccess(name@v, [members])`：owner 将成员移出 readers，epoch+1 并
    /// 生成新密钥（新数据对已移出成员不可读，历史数据不追溯重加密）。本机写入
    /// 新 epoch 密钥；对剩余 readers 的 orgkey-deliver 投递由 host 层完成。
    pub fn data_revoke_access(
        &mut self,
        org_id: &str,
        name: &str,
        version: &str,
        remove_members: &[String],
    ) -> Result<orgsync::AclRecord> {
        self.org_publish_access_key(org_id)?;
        let (acl, _my_root) = self.require_owner(org_id, name, version)?;
        let readers: Vec<String> = acl
            .readers
            .iter()
            .filter(|r| !remove_members.contains(r))
            .cloned()
            .collect();
        let epoch = acl.epoch + 1;
        // 剩余 readers（移出者之外的成员）：revoke 轮换后向其投递新 epoch 密钥
        let remaining_readers = readers.clone();
        let new_acl = self.sign_and_put_acl(
            org_id,
            name,
            version,
            epoch,
            acl.owners.clone(),
            readers,
            None,
        )?;
        // 生成新 epoch 密钥并落本机 orgkey 表（新数据以新密钥加密；投递走 host）
        let new_key = orgsync::generate_epoch_key();
        let storage = self.require_storage_mut()?;
        orgsync::put_epoch_key(storage, org_id, name, version, epoch, &new_key);
        // O4 工作项 3（出站）：revoke 轮换后向剩余 readers 逐一投递新 epoch。
        if !remaining_readers.is_empty() {
            self.deliver_orgkey_to_readers(
                org_id,
                name,
                version,
                &remaining_readers,
                epoch..=epoch,
            );
        }
        Ok(new_acl)
    }

    /// **重置接管**（org-orgsync.md §20.7 / org-data-sync §7 红线 2）：owner 全部
    /// 离开/失联 → 集合锁死，spark 管理员（org admin）可写入新 acl：
    /// - `resetBy` = 管理员 rootId（接管记录全员可见，构成审计事件）；
    /// - `epoch` 重置为 1、名单全新（owners/readers 由管理员指定）；
    /// - 无历史密钥——**只能重启、读不到历史**（旧密文以旧密钥加密，接管者
    ///   拿不到；历史恢复须仍在组的旧读者配合重加密迁移）。
    pub fn data_reset_access(
        &mut self,
        org_id: &str,
        name: &str,
        version: &str,
        new_owners: &[String],
        new_readers: &[String],
    ) -> Result<orgsync::AclRecord> {
        self.org_publish_access_key(org_id)?;
        let my_root = self.require_current_root_id()?;
        // 接管资格 = spark 管理员（org admin）——覆盖 owner 名单的锁死恢复路径
        let storage = self.require_storage()?;
        let is_admin = crate::org::OrganizationService::get_record(storage, org_id)
            .ok()
            .flatten()
            .is_some_and(|rec| {
                rec.find_member(&my_root)
                    .is_some_and(|m| m.role == crate::org::OrganizationRole::Admin)
            });
        if !is_admin {
            return Err(super::KernelError::AccessDenied);
        }
        let mut owners = new_owners.to_vec();
        if !owners.contains(&my_root) {
            owners.push(my_root.clone());
        }
        // O3：reset **不复用历史 epoch 号**——取本机 orgkey 表已知最大 epoch+1
        // （无历史则回退 1）。否则旧读者旧 epoch-1 密钥 vs 新 epoch-1 投递会
        // 被幂等丢弃（「本地已有 ≥ epoch」）导致新密钥不到达，新旧读者分裂。
        // §20.7 语义保留：接管后无历史密钥（只能重启、读不到历史），仅 epoch
        // 号不复用。
        let reset_epoch =
            orgsync::max_known_epoch(storage, org_id, name, version).map_or(1, |m| m + 1);
        let new_acl = self.sign_and_put_acl(
            org_id,
            name,
            version,
            reset_epoch,
            owners,
            new_readers.to_vec(),
            Some(my_root.clone()), // resetBy = 管理员 rootId
        )?;
        // 接管者自持新 reset_epoch 密钥（无历史，重启集合）
        let new_key = orgsync::generate_epoch_key();
        let storage = self.require_storage_mut()?;
        orgsync::put_epoch_key(storage, org_id, name, version, reset_epoch, &new_key);
        // O4 工作项 3（出站）：reset 后向新名单投递 reset_epoch（接管者自持
        // 密钥，不需自投）。
        let recipients: Vec<String> = new_readers
            .iter()
            .filter(|r| r.as_str() != my_root.as_str())
            .cloned()
            .collect();
        if !recipients.is_empty() {
            self.deliver_orgkey_to_readers(
                org_id,
                name,
                version,
                &recipients,
                reset_epoch..=reset_epoch,
            );
        }
        Ok(new_acl)
    }

    /// O4 工作项 3（出站）：把集合密钥逐 epoch 定向投递给 readers（orgkey-deliver
    /// dm 信封，§20.6）。由 grant/revoke/reset 触发：
    ///
    /// - **grant**：对新读者逐 epoch（1..current）各发一条（新读者可解密历史）；
    /// - **revoke**：对剩余 readers 投递新 epoch；
    /// - **reset**：对名单投递 epoch 1。
    ///
    /// 收件人无 accessKey（未发布组织身份访问密钥）→ 记日志跳过（无法投递，
    /// 不失败）；离线经 dm_delivery 退避重试（2s/5s，`spawn_deliveries_with_retry`）。
    ///
    /// 信封：owner 组织身份 Ed25519 签名 + crypto_box（recipient 组织身份公钥
    /// X25519）包裹 epoch 密钥；`recipientRootId` 防转投。本方法只做尽力投递
    /// （spawn 到 runtime，不阻塞；投递失败由退避重试兜底）。
    ///
    /// 装配逻辑在 [`plan_orgkey_deliveries`]（与 host 层 hello 触发的 pending
    /// 重投共用，F3）。
    pub(crate) fn deliver_orgkey_to_readers(
        &mut self,
        org_id: &str,
        name: &str,
        version: &str,
        recipients: &[String],
        epochs: std::ops::RangeInclusive<u64>,
    ) {
        // owner 组织身份（签名 + X25519 私钥）：seed 派生
        let Some(seed) = self.unlocked.as_ref().map(|u| u.seed) else {
            log::warn!("[ORGKEY] deliver skipped: no seed (locked) | org={org_id}");
            return;
        };
        let Ok(sender_root_id) = self.require_current_root_id() else {
            log::warn!("[ORGKEY] deliver skipped: no current root id | org={org_id}");
            return;
        };
        // 根私钥：dm 信封必须由根身份签名（from==rootId 绑定，verify_envelope
        // 以 pubKey=根公钥 → from 强绑定；orgkey-deliver 的 dm 信封签名者必须是
        // root，否则对端「from 验签」失败——信封的 dm 层签名与 body 内的
        // org-access 域签名是两套独立体系）。body sig 仍为 owner 组织域身份
        // 签名（验签锚 = 成员表 accessKey，§20.6）。
        let Some(root_key) = self
            .unlocked
            .as_ref()
            .map(|u| u.identity.signing_key.clone())
        else {
            log::warn!("[ORGKEY] deliver skipped: no root signing key | org={org_id}");
            return;
        };
        let deliveries = {
            let Ok(storage) = self.require_storage_mut() else {
                return;
            };
            let mut ctx = OrgkeyDeliverCtx {
                storage,
                seed,
                sender_root_id,
                root_key,
                now_ms: crate::p2p::node::system_now_ms(),
            };
            plan_orgkey_deliveries(&mut ctx, org_id, name, version, recipients, epochs)
        };
        if deliveries.is_empty() {
            return;
        }
        self.spawn_deliveries_with_retry(deliveries, &DM_RETRY_DELAYS);
    }

    /// O5：收到对方 orgsync-hello（成员上线）后扫描本机 orgkey pending，向该
    /// 成员重投未送达的 orgkey-deliver（离线期间 grant/revoke 的密钥补投）。
    ///
    /// F3 接线：host 层（`host/dm_handler/replies.rs::spawn_orgkey_pending_resend`）
    /// 不持 `&mut Kernel`，hello 触发路径直接复用 [`plan_pending_orgkey_resend`]
    /// + 共享装配原语；本方法保留为 Kernel 内入口（测试直调覆盖）。
    /// 解锁态缺失（无 seed/根私钥）时**保留 pending** 等下次 hello，不清键。
    #[allow(dead_code)]
    pub(crate) fn resend_pending_orgkey(&mut self, org_id: &str, recipient_root_id: &str) {
        let (Some(seed), Some(root_key)) = (
            self.unlocked.as_ref().map(|u| u.seed),
            self.unlocked
                .as_ref()
                .map(|u| u.identity.signing_key.clone()),
        ) else {
            return;
        };
        let Ok(sender_root_id) = self.require_current_root_id() else {
            return;
        };
        let deliveries = {
            let Ok(storage) = self.require_storage_mut() else {
                return;
            };
            let mut ctx = OrgkeyDeliverCtx {
                storage,
                seed,
                sender_root_id,
                root_key,
                now_ms: crate::p2p::node::system_now_ms(),
            };
            plan_pending_orgkey_resend(&mut ctx, org_id, recipient_root_id)
        };
        if deliveries.is_empty() {
            return;
        }
        self.spawn_deliveries_with_retry(deliveries, &DM_RETRY_DELAYS);
    }
}

/// orgkey-deliver 投递装配上下文（F3 提取）：Kernel 出站（grant/revoke/reset/
/// resend）与 host 层 hello 触发的 pending 重投共用同一装配逻辑——两侧持有
/// 的资源形态不同（Kernel 字段 vs host 共享格），装配规则只有一份。
pub(crate) struct OrgkeyDeliverCtx<'a, S: StorageBackend> {
    /// 存储句柄（读成员表/epoch 密钥；收件人不可达的 skip 分支落 pending）。
    pub storage: &'a mut S,
    /// 解锁期 BIP39 种子（owner 组织域身份即时派生，不持久化）。
    pub seed: [u8; 64],
    /// 本机 rootId（投递 sender；收端以 from 匹配其成员表 accessKey 验签）。
    pub sender_root_id: String,
    /// 根签名私钥（dm 信封签名，from==rootId 绑定；body 内 sig 为组织域身份
    /// 签名，两套独立体系）。
    pub root_key: ed25519_dalek::SigningKey,
    /// 当前时间（ms）。
    pub now_ms: i64,
}

/// 装配 orgkey-deliver 投递（§20.6）：逐 (recipient, epoch) 构造「根签名 dm
/// 信封 + 组织域身份签名 body + crypto_box 包裹 epoch 密钥」，按收件人成员
/// 记录的 node_info 逐端点展开为 (peer, envelope) 列表。
///
/// 收件人未发布 accessKey 或无 node_info（离线/不可寻址）→ 落
/// `orgkey-pending:` 待重投键并跳过（对端上线 hello 时由
/// [`plan_pending_orgkey_resend`] 重投）。
pub(crate) fn plan_orgkey_deliveries<S: StorageBackend>(
    ctx: &mut OrgkeyDeliverCtx<'_, S>,
    org_id: &str,
    name: &str,
    version: &str,
    recipients: &[String],
    epochs: std::ops::RangeInclusive<u64>,
) -> Vec<(crate::p2p::peer_targets::PeerNodeInfo, Value)> {
    let domain = Kernel::org_access_domain(org_id);
    let owner_org = crate::identity::derive_domain_identity(&ctx.seed, &domain);
    let owner_x25519_priv = orgsync::ed_sk_to_x25519(&owner_org.signing_key.to_bytes());
    let now = ctx.now_ms;
    // 收件人成员记录（取 accessKey 公钥 + peer 寻址）
    let Ok(Some(record)) = OrganizationService::get_record(&*ctx.storage, org_id) else {
        return Vec::new();
    };
    let col_full = format!("{name}@v{version}");
    let mut deliveries: Vec<(crate::p2p::peer_targets::PeerNodeInfo, Value)> = Vec::new();
    for recipient in recipients {
        let Some(member) = record.find_member(recipient) else {
            continue;
        };
        // 收件人须已发布 accessKey（组织身份公钥）；否则无法投递 → 跳过。
        // O5：落 orgkey pending，对方发布 accessKey / 上线 orgsync-hello 时重投。
        let Some(access_key) = member.access_key.as_ref() else {
            log::info!(
                "[ORGKEY] deliver skip: recipient={} has no accessKey | col={}",
                &recipient[..std::cmp::min(16, recipient.len())],
                col_full
            );
            for epoch in epochs.clone() {
                orgsync::orgkey_pending_put(ctx.storage, org_id, &col_full, recipient, epoch, now);
            }
            continue;
        };
        let Ok(pk_bytes) = B64.decode(&access_key.public_key) else {
            continue;
        };
        let Ok(pk_arr) = <[u8; 32]>::try_from(pk_bytes.as_slice()) else {
            continue;
        };
        let Some(recipient_x25519) = orgsync::ed_pk_to_x25519(&pk_arr) else {
            continue;
        };
        // 收件人 peer 寻址（member node_info 端点）
        let Some(node_info) = member.node_info.clone() else {
            log::info!(
                "[ORGKEY] deliver skip: recipient={} no node info | col={}",
                &recipient[..std::cmp::min(16, recipient.len())],
                col_full
            );
            for epoch in epochs.clone() {
                orgsync::orgkey_pending_put(ctx.storage, org_id, &col_full, recipient, epoch, now);
            }
            continue;
        };
        // 逐 epoch 投递
        for epoch in epochs.clone() {
            let Some(epoch_key) =
                orgsync::get_epoch_key(&*ctx.storage, org_id, name, version, epoch)
            else {
                log::warn!(
                    "[ORGKEY] deliver skip: no epoch key {} | col={}",
                    epoch,
                    col_full
                );
                continue;
            };
            let Some(body) = orgsync::build_orgkey_deliver(
                org_id,
                name,
                version,
                epoch,
                &epoch_key,
                &ctx.sender_root_id,
                recipient,
                &recipient_x25519,
                &owner_org.signing_key,
                &owner_x25519_priv,
                now,
            ) else {
                continue;
            };
            // 装配 dm 信封（from=owner rootId、to=recipient）：**dm 信封用
            // 根私钥签名**（from==rootId 绑定，R1），body 内 sig 保持
            // org-access 域身份签名（验签锚 = 成员表 accessKey）。
            let envelope = crate::kernel::dm_envelope::build_envelope(
                crate::kernel::dm_envelope::KIND_ORGKEY_DELIVER,
                &ctx.sender_root_id,
                recipient,
                now,
                body,
                &ctx.root_key,
            );
            // 逐端点投递（多设备聚合）
            for info in node_info.iter() {
                let peer = crate::p2p::peer_targets::PeerNodeInfo {
                    peer_id: info.peer_id.clone(),
                    addresses: info.addresses.clone(),
                };
                deliveries.push((peer, envelope.clone()));
            }
        }
    }
    deliveries
}

/// hello 触发的 pending 重投装配（F3，§20.6 离线补投）：扫描本机
/// `orgkey-pending:{orgId}:` 中收件人为 `recipient_root_id` 的条目，逐条经
/// [`plan_orgkey_deliveries`] 装配投递并删除 pending 键。
///
/// 删除口径：重投幂等（收端已有 ≥ epoch 密钥即静默丢弃，重复无害）；投递仍
/// 不可达（无 accessKey/node_info）时 skip 分支会重写 pending，净效果保留。
///
/// 返回待投递的 (peer, envelope) 列表，发送由调用方 spawn（退避重试）。
pub(crate) fn plan_pending_orgkey_resend<S: StorageBackend>(
    ctx: &mut OrgkeyDeliverCtx<'_, S>,
    org_id: &str,
    recipient_root_id: &str,
) -> Vec<(crate::p2p::peer_targets::PeerNodeInfo, Value)> {
    let pending = orgsync::orgkey_pending_for_org(&*ctx.storage, org_id);
    let mut deliveries = Vec::new();
    for (col_full, recipient, epoch, _ts) in pending {
        if recipient != recipient_root_id {
            continue;
        }
        // 解析 name/version（collection 为 `{name}@v{version}`）
        let Some(at) = col_full.rfind("@v") else {
            continue;
        };
        let name = &col_full[..at];
        let version = &col_full[at + 2..];
        deliveries.extend(plan_orgkey_deliveries(
            ctx,
            org_id,
            name,
            version,
            std::slice::from_ref(&recipient),
            epoch..=epoch,
        ));
        // 重投后删除 pending（投递成功/对端幂等丢弃均无害）。
        orgsync::orgkey_pending_remove(ctx.storage, org_id, &col_full, &recipient, epoch);
    }
    deliveries
}

#[cfg(test)]
#[path = "data_access_tests.rs"]
mod tests;

//! 设备清单门面（`Kernel` 的查询方法）：设备管理页的数据来源。
//!
//! 设备记录由 device 模块落库：本机条目在 p2p start 时采集刷新，其他设备
//! 条目经 device-sync 入站落库。本方法做视图装配：本机标记 + 在线状态
//! （peerId 命中当前连接快照）。

use serde::Serialize;

use super::{Kernel, KernelError, Result};
use crate::device::{DeviceRecord, DeviceService};
use crate::kernel::dm_delivery::heal_self_friend_to_healthy_device;
use crate::p2p::P2pEvent;
use crate::p2p::node::system_now_ms;
use crate::p2p::priority_peers::PriorityPeerStore;
use crate::storage::StorageBackend;

/// 设备清单视图项（壳层 DTO 同源；serde camelCase 线形）。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceView {
    /// 设备标识（libp2p peerId）。
    pub peer_id: String,
    /// 设备名（hostname）。
    pub device_name: String,
    /// 操作系统友好名。
    pub os: String,
    /// CPU 架构。
    pub arch: String,
    /// 物理地址列表（平台限制采集不到时为空）。
    pub macs: Vec<String>,
    /// 本机运行的应用版本（空串 = 旧记录/旧版本对端，前端展示「—」）。
    pub app_version: String,
    /// 操作系统版本号（采集失败为空串）。
    pub os_version: String,
    /// 记录内容更新时间（ms）。
    pub updated_at: i64,
    /// 最近在线证据时间（ms；本机=最近采集，对端=最近收到其 device-sync）。
    pub last_seen_at: i64,
    /// 是否本机。
    pub is_self: bool,
    /// 是否当前在线（peerId 命中 p2p 连接快照；本机恒 true）。
    pub online: bool,
    /// 撤销时间戳（ms）；未撤销为 null/None。
    pub revoked_at: Option<i64>,
}

impl Kernel {
    /// 设备清单查询：本机条目置顶，其余按 last_seen_at 降序。
    ///
    /// 本机记录缺失时（p2p 未启动过、存储迁移前）兜底采集落库一条——
    /// 设备管理页任意时刻打开都有本机设备可看。
    pub fn devices_list(&mut self) -> Result<Vec<DeviceView>> {
        let local_peer_id = self
            .p2p
            .as_ref()
            .map(|n| n.peer_id().to_string());
        let connected: Vec<String> = match self.p2p.as_ref() {
            Some(node) => self
                .runtime
                .handle()
                .block_on(node.local_node_info())
                .map(|i| i.connected_peers)
                .unwrap_or_default(),
            None => Vec::new(),
        };
        let now = crate::p2p::node::system_now_ms();
        let node_id = self.sync_node_id();
        let root_id = self.require_unlocked_root_id().ok();
        // app_version 先取出再借 storage：避免与 require_storage_mut 的互斥借用冲突
        let app_version = self.config.app_version.clone();
        // D2：在取 storage 可变借用前先派生会话 Kverify（需要读 self.unlocked + storage）。
        let kverify = crate::kernel::pw_ops::derive_session_kverify(self).ok().flatten();
        {
            let storage = self.require_storage_mut()?;
            // 本机记录兜底：p2p 已启动但清单无本机条目时采集落库
            if let Some(peer_id) = &local_peer_id {
                if crate::device::DeviceService::get(storage, peer_id)?.is_none() {
                    let device_pub_key = crate::p2p::identity_store::load_libp2p_pub_key(storage);
                    let record =
                        crate::device::DeviceService::upsert_self(
                            storage,
                            peer_id,
                            now,
                            &node_id,
                            &app_version,
                            device_pub_key,
                        )?;
                    if let Some(ref root_id) = root_id {
                        let _ = crate::epoch::EpochService::maybe_grant_epoch_key(
                            storage,
                            root_id,
                            &node_id,
                            &node_id,
                            now,
                            &record.peer_id,
                            record.device_pub_key.as_deref(),
                            record.revoked_at,
                            kverify.as_ref(),
                        );
                    }
                    if let Ok(data) = serde_json::to_value(&record) {
                        let _ = self.event_tx.send(crate::p2p::P2pEvent::DeviceUpdated(data));
                    }
                }
            }
        }
        let records = crate::device::DeviceService::list(self.require_storage()?)?;
        let views = records
            .into_iter()
            .map(|r| self.to_device_view(r, local_peer_id.as_deref(), &connected))
            .collect::<Vec<_>>();
        let mut views = views;
        views.sort_by_key(|v| (std::cmp::Reverse(v.is_self), std::cmp::Reverse(v.last_seen_at)));
        Ok(views)
    }

    /// 撤销指定设备（M2）。`device_id` 可以是 peerId 或 deviceUid。
    ///
    /// 流程：
    /// 1. 空 ID 校验。
    /// 2. 本机双重保护：比对本地 peerId + deviceUid，命中任一即拒绝。
    /// 3. 按 peerId / deviceUid 查找目标，找不到返回 "Device not found"。
    /// 4. 标记撤销、写安全日志、self FriendRecord peer 迁移、PriorityPeerStore 移除、
    ///    DeviceUpdated 事件、即时断连。
    pub fn revoke_device(&mut self, device_id: &str) -> Result<()> {
        let device_id = device_id.trim();
        if device_id.is_empty() {
            return Err(KernelError::Internal("deviceId is empty".into()));
        }

        // read-modify-write 串行化：安全日志 → mark_revoked → heal → priority remove
        // 多步写需与并发写互斥（同 devices_view 模式）。
        let __io = std::sync::Arc::clone(&self.io_lock);
        let _io_guard = __io.lock().unwrap_or_else(|e| e.into_inner());

        let now_ms = system_now_ms();
        let node_id = self.sync_node_id();

        // 本机 peerId：优先取运行中 p2p；未启动则从持久化私钥推导，保证离线
        // 也能完成本机保护校验与撤销。
        let local_peer_id = self
            .p2p_status()
            .ok()
            .flatten()
            .and_then(|i| i.peer_id)
            .or_else(|| crate::p2p::identity_store::load_peer_id(self.require_storage().ok()?))
            .ok_or_else(|| KernelError::Internal("Cannot determine local device".into()))?;

        // 本机双重保护：peerId 或 deviceUid 命中本地记录均拒绝。
        if local_peer_id == device_id {
            return Err(KernelError::Internal("Cannot revoke current device".into()));
        }
        let local_device_uid = DeviceService::get(self.require_storage()?, &local_peer_id)?
            .and_then(|r| r.device_uid);
        if local_device_uid.as_deref() == Some(device_id) {
            return Err(KernelError::Internal("Cannot revoke current device".into()));
        }

        // 按 peerId 或 deviceUid 定位目标。
        let storage = self.require_storage()?;
        let mut target = DeviceService::get(storage, device_id)?;
        if target.is_none() {
            target = DeviceService::get_by_device_uid(storage, device_id)?;
        }
        let Some(target) = target else {
            return Err(KernelError::Internal("Device not found".into()));
        };
        let Some(device_uid) = target.device_uid.as_deref() else {
            return Err(KernelError::Internal("Device not found".into()));
        };

        DeviceService::append_security_log(
            self.require_storage_mut()?,
            "device_revoke_initiated",
            serde_json::json!({
                "deviceId": target.peer_id,
                "deviceName": target.device_name,
                "actor": "local",
            }),
            now_ms,
        )?;

        let record = DeviceService::mark_revoked(
            self.require_storage_mut()?,
            device_uid,
            now_ms,
            now_ms,
            &node_id,
        )?
        .ok_or_else(|| KernelError::Internal("Device not found".into()))?;

        // self FriendRecord peer 清除 + 迁移到最新健康设备。
        let root_id = self
            .current_root_id_shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(root_id) = root_id.as_deref() {
            let _ = heal_self_friend_to_healthy_device(
                self.require_storage_mut()?,
                root_id,
                &local_peer_id,
                &node_id,
                now_ms,
            );
        }

        // 从优先恢复集合移除。
        let mut priority = PriorityPeerStore::new(self.require_storage_mut()?);
        if let Err(e) = priority.remove(&record.peer_id) {
            eprintln!("[revoke-device] priority peer remove failed: {e}");
        }

        if let Ok(data) = serde_json::to_value(&record) {
            let _ = self.event_tx.send(P2pEvent::DeviceUpdated(data));
        }

        DeviceService::append_security_log(
            self.require_storage_mut()?,
            "device_revoke_effective",
            serde_json::json!({
                "deviceId": record.peer_id,
            }),
            now_ms,
        )?;

        // 向已配对自设备广播带 revokedAt 的设备快照（M2 §4.2-②）。
        self.broadcast_device_sync(&record);

        // M3：撤销触发 epoch 密钥轮换。
        if let Err(e) = crate::kernel::epoch_ops::after_revoke_snapshot(self, &record.peer_id) {
            log::error!("[revoke-device] epoch rotation after revoke failed for peer={}: {}", record.peer_id, e);
            let _ = DeviceService::append_security_log(
                self.require_storage_mut()?,
                "rotation_failed",
                serde_json::json!({"reason": "revoke", "peer_id": record.peer_id, "error": format!("{e}")}),
                now_ms,
            );
        }

        // 即时断连：交给 runtime spawn，避免阻塞 API 返回。
        if let Some(node) = self.p2p.clone() {
            let peer_id = record.peer_id.clone();
            self.runtime.spawn(async move {
                if let Err(e) = node.disconnect_peer(&peer_id).await {
                    eprintln!("[revoke-device] disconnect_peer failed: {e}");
                }
            });
        }

        Ok(())
    }

    /// 读取本地安全日志（`security:log:` 前缀），返回 `(key, raw_json)` 数组。
    /// 内部调试命令，不进 pdsync。结果按 key 倒序（时间从新到旧），并受
    /// `limit` 限制。
    pub fn security_log_list(&self, limit: Option<usize>) -> Result<Vec<(String, String)>> {
        let storage = self.require_storage()?;
        let opts = crate::storage::ScanOptions {
            prefix: "security:log:".to_string(),
            ..Default::default()
        };
        let mut rows = storage.scan(&opts)?;
        rows.reverse();
        if let Some(limit) = limit {
            rows.truncate(limit);
        }
        Ok(rows)
    }

    fn to_device_view(
        &self,
        r: DeviceRecord,
        local_peer_id: Option<&str>,
        connected: &[String],
    ) -> DeviceView {
        let is_self = local_peer_id.is_some_and(|p| p == r.peer_id);
        let online = is_self || connected.iter().any(|p| p == &r.peer_id);
        DeviceView {
            peer_id: r.peer_id,
            device_name: r.device_name,
            os: r.os,
            os_version: r.os_version,
            arch: r.arch,
            macs: r.macs,
            app_version: r.app_version,
            updated_at: r.updated_at,
            last_seen_at: r.last_seen_at,
            is_self,
            online,
            revoked_at: r.revoked_at,
        }
    }
}

//! 设备清单门面（`Kernel` 的查询方法）：设备管理页的数据来源。
//!
//! 设备记录由 device 模块落库：本机条目在 p2p start 时采集刷新，其他设备
//! 条目经 device-sync 入站落库。本方法做视图装配：本机标记 + 在线状态
//! （peerId 命中当前连接快照）。

use serde::Serialize;

use super::{Kernel, Result};
use crate::device::DeviceRecord;

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
    /// 记录内容更新时间（ms）。
    pub updated_at: i64,
    /// 最近在线证据时间（ms；本机=最近采集，对端=最近收到其 device-sync）。
    pub last_seen_at: i64,
    /// 是否本机。
    pub is_self: bool,
    /// 是否当前在线（peerId 命中 p2p 连接快照；本机恒 true）。
    pub online: bool,
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
        {
            let storage = self.require_storage_mut()?;
            // 本机记录兜底：p2p 已启动但清单无本机条目时采集落库
            if let Some(peer_id) = &local_peer_id {
                if crate::device::DeviceService::get(storage, peer_id)?.is_none() {
                    let record =
                        crate::device::DeviceService::upsert_self(storage, peer_id, now, &node_id)?;
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
            arch: r.arch,
            macs: r.macs,
            updated_at: r.updated_at,
            last_seen_at: r.last_seen_at,
            is_self,
            online,
        }
    }
}

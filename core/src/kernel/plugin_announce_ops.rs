//! 插件市场广播索引门面（plugin-dist §8，阶段 C 波次 2a）：
//! 发布声明（字段校验 → 根身份签名 → 算 PoW → 广播 → 入本地索引）、
//! 本地索引查询与懒惰核查终态回写。
//!
//! PoW 计算为 CPU 密集（秒级）：壳层命令必须经 `spawn_blocking` 调用
//! `publish_plugin_announce`，避免阻塞 UI/事件循环线程。

use crate::p2p::constants::PLUGIN_ANNOUNCE_MIN_POW_BITS;
use crate::p2p::node::system_now_ms;
use crate::p2p::plugin_announce::{
    AnnouncePow, AnnounceVerified, PluginAnnounceIndexEntry, PluginAnnounceInput,
    PluginAnnounceStore, build_signed_announce, mine_announce_nonce, plugin_announce_to_json,
};
use crate::p2p::{P2pError, P2pEvent};

use super::{Kernel, KernelError, Result};

impl Kernel {
    /// 发布插件声明（§8.2-8.4）：字段校验 → 根身份签名（publisher=当前
    /// rootId）→ mine PoW（难度取节点配置覆盖，缺省网络常量 20）→ 广播
    /// → 自发声明同样入本地索引。需身份已解锁且 P2P 已启动。
    pub fn publish_plugin_announce(
        &self,
        input: &PluginAnnounceInput,
    ) -> Result<PluginAnnounceIndexEntry> {
        let unlocked = self.unlocked.as_ref().ok_or(KernelError::Locked)?;
        let now = system_now_ms();
        // 先字段校验 + 签名（错误优先于 P2P 状态检查），再要求 P2P 已启动，
        // PoW（秒级 CPU）放到最后算，避免未启动时白算
        let (mut announce, payload) =
            build_signed_announce(input, &unlocked.identity.signing_key, now)
                .map_err(KernelError::Internal)?;
        let node = self.p2p.as_ref().ok_or(P2pError::NotStarted)?;
        let bits = self
            .config
            .p2p
            .as_ref()
            .and_then(|c| c.plugin_announce_pow_bits)
            .unwrap_or(PLUGIN_ANNOUNCE_MIN_POW_BITS);
        announce.pow = AnnouncePow {
            bits,
            nonce: mine_announce_nonce(&payload, bits),
        };
        let json = plugin_announce_to_json(&announce);
        self.runtime
            .handle()
            .block_on(node.publish_plugin_announce(&json))?;
        // 自发声明入本地索引（verified 走懒惰核查，同网络口径）
        let mut storage = self.require_storage()?.clone();
        let mut store = PluginAnnounceStore::new(&mut storage);
        store.upsert(&announce, now)?;
        Ok(store.get(&announce.id)?.expect("own announce indexed"))
    }

    /// 本地索引列表（§8.7；惰性清过期，按 updatedAt 降序）。市场视图
    /// （波次 2b）只展示 `verified == Verified` 的条目。
    pub fn list_plugin_announces(&self) -> Result<Vec<PluginAnnounceIndexEntry>> {
        let mut storage = self.require_storage()?.clone();
        Ok(PluginAnnounceStore::new(&mut storage).list(system_now_ms())?)
    }

    /// 单条索引查询（verified 状态查询；id 须为规范化线形）。
    pub fn get_plugin_announce(&self, id: &str) -> Result<Option<PluginAnnounceIndexEntry>> {
        let mut storage = self.require_storage()?.clone();
        Ok(PluginAnnounceStore::new(&mut storage).get(id)?)
    }

    /// 懒惰核查终态回写（§8.8，壳层核查队列调用）：verified=true 标
    /// Verified（进入市场视图），false 标 Failed 并记原因；条目持久化，
    /// 并向壳层/渲染端发 `PluginAnnounceVerified` 事件。条目不存在返回 false。
    pub fn mark_plugin_announce_verified(
        &self,
        id: &str,
        verified: bool,
        error: Option<&str>,
    ) -> Result<bool> {
        let now = system_now_ms();
        let mut storage = self.require_storage()?.clone();
        let state = if verified {
            AnnounceVerified::Verified
        } else {
            AnnounceVerified::Failed
        };
        let marked =
            PluginAnnounceStore::new(&mut storage).mark_verified(id, state, error.unwrap_or(""), now)?;
        if marked {
            let _ = self.event_tx.send(P2pEvent::PluginAnnounceVerified {
                id: id.to_string(),
                verified,
                error: error.map(ToString::to_string),
            });
        }
        Ok(marked)
    }
}

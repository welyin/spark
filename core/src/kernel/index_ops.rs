//! indexer 角色门面（community-affairs §9 C10、§10 决策 4）：节点自选启用
//! 的角色开关 + 子集覆盖配置（affair-metadata §7 目录面）、元数据公告发布
//! （修订生效后的新代际公告，§4）、目录名片发布/查询（轻客户端发现可用
//! indexer 的路径）与本地直查（轻客户端 loopback / 壳层调试与 p2p 应答共用
//! 同一路径 `crate::index::query`，保证任何入口结果一致）。簿记与推导全在
//! `crate::index` 纯逻辑层，本文件只做编排与错误映射。

use serde_json::{Map, Value};

use super::{Kernel, KernelError, Result};
use crate::affair::is_valid_identity_id;
use crate::index::directory::{IndexCoverage, IndexRoleConfig};
use crate::p2p::constants::AFFAIR_META_TOPIC;
use crate::p2p::node::system_now_ms;

impl Kernel {
    /// 启用/关闭 indexer 角色（节点配置开关；会话级内存态，默认关闭）。
    /// 关闭时本节点不应答 `affair-meta-query`（回 indexer-disabled），
    /// gossip 元数据面仍照常订阅（暂存区是客户端缓存语义，与角色无关）。
    /// 只写启用位，保留已配置的子集覆盖。
    pub fn set_indexer_enabled(&mut self, enabled: bool) {
        self.indexer_role_shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .enabled = enabled;
    }

    /// 当前 indexer 角色状态。
    pub fn indexer_enabled(&self) -> bool {
        self.indexer_role_shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .enabled
    }

    /// 配置子集覆盖（affair-metadata §7 目录面）：regions/topics 双维度，
    /// 空列表 = 该维度不限（两维皆空 = 全覆盖，与纯启用等价）。生效路径：
    /// gossip 收录过滤（只收录子集内公告）+ 查询应答门控（覆盖外查询回
    /// indexer-not-covered）+ 目录名片宣告内容。线形校验失败报错。
    pub fn set_indexer_coverage(&mut self, regions: Vec<String>, topics: Vec<String>) -> Result<()> {
        let coverage = IndexCoverage { regions, topics };
        if !crate::index::directory::coverage_valid(&coverage) {
            return Err(KernelError::Internal(
                "invalid coverage: each dimension ≤16 items, each 1–32 UTF-16".to_string(),
            ));
        }
        self.indexer_role_shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .coverage = coverage;
        Ok(())
    }

    /// 当前 indexer 角色配置（启用位 + 覆盖）。
    pub fn indexer_config(&self) -> IndexRoleConfig {
        self.indexer_role_shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// 发布本节点 indexer 目录名片（§7：启用角色后向 `spark-affair-meta`
    /// 洪泛自公告，轻客户端据此发现可用 indexer）。角色未启用报错；p2p 未
    /// 启动报 `NotStarted`。周期重发由事件循环 keepalive tick 承担，本方法
    /// 供启用后即时宣告与调试。返回名片 payload。
    pub fn indexer_publish_card(&self) -> Result<Value> {
        let config = self.indexer_config();
        if !config.enabled {
            return Err(KernelError::Internal("indexer role disabled".to_string()));
        }
        let node = self.p2p.as_ref().ok_or(crate::p2p::P2pError::NotStarted)?;
        let payload = self
            .runtime
            .handle()
            .block_on(node.publish_indexer_card(config.coverage))?;
        Ok(payload)
    }

    /// 本地 indexer 目录（§7）：新鲜名片条目（TTL 内，按 peerId 升序）。
    /// 轻客户端发现可用 indexer 的读路径；目录条目是线索而非信任根。
    pub fn indexer_directory(&self) -> Result<Vec<Value>> {
        let mut storage = self.require_storage()?.clone();
        let entries = crate::index::directory::list_indexers(&mut storage, system_now_ms())
            .map_err(index_err)?;
        Ok(entries
            .iter()
            .map(|e| {
                serde_json::json!({
                    "peerId": e.peer_id,
                    "regions": e.coverage.regions,
                    "topics": e.coverage.topics,
                    "updatedAt": e.updated_at,
                    "lastSeenAt": e.last_seen_at,
                })
            })
            .collect())
    }

    /// 轻客户端查询路径（§7/§8）：向目录选定的 indexer 发查询帧。未连接时
    /// 先按覆盖网邻居池地址直连（目录只存 peerId，地址由 node-announce 邻居
    /// 池提供），再经 `/spark/affairmeta/1.0.0` 单发单收。返回响应帧文本。
    pub fn indexer_query(&self, peer_id: &str, request_json: &str) -> Result<String> {
        let node = self.p2p.as_ref().ok_or(crate::p2p::P2pError::NotStarted)?;
        let info = self.runtime.handle().block_on(node.local_node_info())?;
        if !info.connected_peers.iter().any(|p| p == peer_id) {
            let mut storage = self.require_storage()?.clone();
            let addresses = crate::p2p::overlay_store::OverlayPeerStore::new(&mut storage)
                .get(peer_id)?
                .map(|r| r.addresses)
                .unwrap_or_default();
            let target = crate::p2p::peer_targets::PeerNodeInfo {
                peer_id: Some(peer_id.to_string()),
                addresses,
            };
            self.runtime.handle().block_on(node.connect_peer(&target))?;
        }
        let response = self
            .runtime
            .handle()
            .block_on(node.query_affair_meta(peer_id, request_json))?;
        Ok(response)
    }

    /// 发布本事务当前生效代际的元数据公告（affair-metadata §4：修订生效后
    /// 由主持人/任一副本发起新代际公告）。从本地日志复算修订链取最新代际，
    /// 经 `spark-affair-meta` gossip 洪泛；同事务多副本重复发布无害——
    /// 收录方按 §5 裁决去重。返回公告 payload。
    pub fn indexer_publish_meta(&mut self, affair_id: &str) -> Result<Value> {
        if !is_valid_identity_id(affair_id) {
            return Err(KernelError::Internal("invalid affairId".to_string()));
        }
        let storage = self.require_storage()?;
        let Some(log) = crate::index::log::load_log(storage, affair_id).map_err(index_err)? else {
            return Err(KernelError::Internal(format!(
                "unknown affair: {affair_id}"
            )));
        };
        let now = system_now_ms();
        let announce = crate::index::query::announce_from_local_log(&log, now);
        let payload = crate::index::announce::announce_to_value(&announce);
        let mut body = Map::new();
        body.insert("type".to_string(), Value::String("affair-meta".to_string()));
        body.insert("domain".to_string(), Value::String("affair".to_string()));
        body.insert("id".to_string(), Value::String(affair_id.to_string()));
        body.insert("payload".to_string(), payload.clone());
        self.p2p_broadcast(AFFAIR_META_TOPIC, body)?;
        Ok(payload)
    }

    /// 本地直查（轻客户端 loopback 与壳层调试用）：请求帧文本进、响应帧
    /// 文本出——与 p2p 应答路径同一解析/分发函数，结果天然一致。loopback
    /// 不做覆盖门控（本地用户查本机缓存不受角色服务范围约束）。
    pub fn indexer_search(&self, request_json: &str) -> Result<String> {
        let Some(query) = crate::index::query::parse_query(request_json) else {
            return Ok(crate::index::query::build_error("unknown", "bad-query"));
        };
        let storage = self.require_storage()?;
        let payload = crate::index::query::run_search(storage, &query.search, system_now_ms())
            .map_err(index_err)?;
        Ok(crate::index::query::build_result(&query.query_id, &payload))
    }
}

fn index_err(e: crate::index::IndexError) -> KernelError {
    KernelError::Internal(format!("indexer: {e}"))
}

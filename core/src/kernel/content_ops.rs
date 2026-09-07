//! 内容面门面（`Kernel` 的 content API）：blob 按 CID 的存/取/做种/拉取与
//! GC 衔接（public-topics §七「持有即做种」）。
//!
//! 线形：本地 content store（`cblob:` 命名空间）为权威；`content_fetch_blob`
//! 本地未命中时经 Kad provider 检索 → 逐 provider 解析地址（覆盖网邻居池 +
//! DHT 节点存在记录，验签）→ 连接 → `/spark/blob-fetch/1.0.0` 拉回本体
//! （接收侧 CID 哈希校验与自动做种在 p2p 事件循环内完成，见
//! `p2p/node/blob_fetch.rs`）。
//!
//! GC 衔接（`content/store.rs` 的调用方约定在此落地）：`content_gc_sweep`
//! 每回收一个 blob 即同步 `stop_providing_blob` 停止做种。

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;

use super::{Kernel, KernelError, Result};
use crate::content::{self, Cid, ContentBlobInfo};
use crate::p2p::node::system_now_ms;
use crate::p2p::{PeerNodeInfo, node_presence_record_key, verify_announce_text};

impl Kernel {
    /// 保存 blob 本体（base64 入，幂等）并按「持有即做种」声明 provider。
    /// p2p 未启动时仅落本地；leaf 模式的 provide 拒绝属预期，静默放过。
    pub fn content_save_blob(&mut self, data_base64: &str) -> Result<ContentBlobInfo> {
        let bytes = B64.decode(data_base64).map_err(|e| {
            KernelError::Internal(format!("content save blob: invalid base64: {e}"))
        })?;
        let info = content::save_blob(self.require_storage_mut()?, &bytes)?;
        if let Some(node) = &self.p2p {
            match self.runtime.handle().block_on(node.provide_blob(info.cid.as_str())) {
                Ok(()) => {}
                // leaf 模式持有副本但不服务（预期行为）；其余失败仅告警——
                // 本地副本已落库，做种可由 keepalive 重发/调用方重试补上
                Err(e) if e.to_string().contains("leaf mode") => {}
                Err(e) => log::warn!("[content] provide after save failed: {e}"),
            }
        }
        Ok(info)
    }

    /// 本地读取 blob（命中 → base64；未命中 → None，不触发网络拉取）。
    pub fn content_read_blob(&self, cid: &str) -> Result<Option<String>> {
        let cid = Cid::parse(cid)?;
        Ok(content::read_blob(self.require_storage()?, &cid)?.map(|d| B64.encode(&d)))
    }

    /// 按 CID 取 blob：本地命中直接返回；否则经 Kad 找 provider 并逐台拉取
    /// （任一台成功即返回，接收侧校验 + 落库 + 自动做种在 p2p 层完成）；
    /// 全部失败/无 provider 返回 `Ok(None)`。
    pub fn content_fetch_blob(&mut self, cid: &str) -> Result<Option<String>> {
        let cid = Cid::parse(cid)?;
        if let Some(data) = content::read_blob(self.require_storage()?, &cid)? {
            return Ok(Some(B64.encode(&data)));
        }
        let Some(node) = self.p2p.clone() else {
            return Ok(None);
        };
        let self_peer = node.peer_id().to_string();
        let providers = self
            .runtime
            .handle()
            .block_on(node.find_blob_providers(cid.as_str()))?;
        for peer_id in providers {
            if peer_id == self_peer {
                continue;
            }
            let addresses = self.resolve_provider_addresses(&node, &peer_id);
            let target = PeerNodeInfo {
                peer_id: Some(peer_id.clone()),
                addresses,
            };
            // 连接失败即试下一台（best-effort 逐台收敛）
            if self
                .runtime
                .handle()
                .block_on(node.connect_peer(&target))
                .is_err()
            {
                continue;
            }
            match self
                .runtime
                .handle()
                .block_on(node.fetch_blob(&peer_id, cid.as_str()))
            {
                Ok(info) => {
                    return content::read_blob(self.require_storage()?, &info.cid)
                        .map(|opt| opt.map(|d| B64.encode(&d)))
                        .map_err(KernelError::from);
                }
                Err(e) => {
                    log::info!("[content] fetch from {peer_id} failed: {e}（试下一台）");
                }
            }
        }
        Ok(None)
    }

    /// 列出本地持有的全部 blob CID（升序）。
    pub fn content_list_blobs(&self) -> Result<Vec<String>> {
        Ok(content::list_blobs(self.require_storage()?)?
            .into_iter()
            .map(|c| c.to_string())
            .collect())
    }

    /// 打 GC 根标记（`root` 为持有理由标签，如 `topic:{topicId}`、`user-pin`）。
    pub fn content_pin_root(&mut self, cid: &str, root: &str) -> Result<()> {
        let cid = Cid::parse(cid)?;
        Ok(content::pin_root(self.require_storage_mut()?, &cid, root)?)
    }

    /// 移除一个 GC 根标记；最后一个根移除后 blob 进入宽限期回收路径。
    pub fn content_unpin_root(&mut self, cid: &str, root: &str) -> Result<()> {
        let cid = Cid::parse(cid)?;
        Ok(content::unpin_root(self.require_storage_mut()?, &cid, root)?)
    }

    /// 无根 blob 两段式回收；**GC 衔接**：每回收一个本体即同步
    /// `stop_providing_blob` 停止做种（store.rs 的调用方约定在此落地）。
    /// 返回回收的 CID 列表。
    pub fn content_gc_sweep(&mut self) -> Result<Vec<String>> {
        let collected = content::gc_sweep(self.require_storage_mut()?, system_now_ms())?;
        if let Some(node) = &self.p2p {
            for cid in &collected {
                // best-effort：停做种失败不使 GC 失败（本体已回收，provider
                // 记录会随后续 stop/重启收敛）
                let _ = self
                    .runtime
                    .handle()
                    .block_on(node.stop_providing_blob(cid.as_str()));
            }
        }
        Ok(collected.into_iter().map(|c| c.to_string()).collect())
    }

    /// provider 地址解析：覆盖网邻居池缓存地址 + DHT 节点存在记录（验签后
    /// 的新鲜地址优先，缓存地址兜底），去重返回。
    fn resolve_provider_addresses(
        &self,
        node: &crate::p2p::P2pNode,
        peer_id: &str,
    ) -> Vec<String> {
        let mut addrs: Vec<String> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        // DHT 节点存在记录（验签，新鲜地址优先）
        let key = node_presence_record_key(peer_id);
        if let Ok(Some(raw)) = self
            .runtime
            .handle()
            .block_on(node.dht_get_record(key.as_bytes()))
            && let Ok(text) = String::from_utf8(raw)
            && let Some(announce) = verify_announce_text(&text)
            && announce.peer_id == peer_id
        {
            for addr in announce.addresses {
                if seen.insert(addr.clone()) {
                    addrs.push(addr);
                }
            }
        }
        // 覆盖网邻居池缓存兜底（node-announce 收录的地址，目录只存 peerId）
        if let Ok(storage) = self.require_storage() {
            let mut storage = storage.clone();
            if let Ok(Some(record)) = crate::p2p::OverlayPeerStore::new(&mut storage).get(peer_id) {
                for addr in record.addresses {
                    if seen.insert(addr.clone()) {
                        addrs.push(addr);
                    }
                }
            }
        }
        addrs
    }
}

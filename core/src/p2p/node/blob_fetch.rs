//! blob-fetch 直连（`/spark/blob-fetch/1.0.0`，public-topics §七「持有即
//! 做种」的传输协议）：出站按 CID 向 provider 拉取本体，入站从本地 content
//! store 服务本体。
//!
//! 接收侧承诺（内容寻址的根本承诺在传输层的落点）：
//! - 应答侧：`read_blob` 读出即重算校验，存盘损坏宁可回 `integrity-error`
//!   也绝不发出与 CID 不符的字节；
//! - 请求侧：响应先验 CID 声明一致 + 重算哈希匹配才落 store，随后按
//!   「持有即做种」自动 begin provide（leaf 模式除外——手机持有副本但不服务，
//!   与 `dht.rs::begin_blob_provide` 同判断）。
//!
//! 防护：请求/响应帧上限由 codec（`BLOB_FETCH_FRAME_MAX_LEN`）与协议超时
//! （`BLOB_FETCH_READ_TIMEOUT_MS`）把控；应答侧逐请求方限流
//! （`BLOB_FETCH_MIN_INTERVAL_MS`）；请求侧在落库前再查一次内容大小上限。

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use libp2p::{PeerId, request_response};
use tokio::sync::oneshot;

use crate::content::{self, Cid, ContentBlobInfo};
use crate::p2p::direct;
use crate::p2p::{P2pError, Result};
use crate::storage::StorageBackend;

use super::P2pEvent;
use super::event_loop::EventLoop;

/// 拉取结果的等待通道负载：请求 id →（目标 CID，调用方等待器）。
pub(super) type PendingBlobFetch = (Cid, oneshot::Sender<Result<ContentBlobInfo>>);

impl<S: StorageBackend> EventLoop<S> {
    /// 发起：向**已连接**的 provider 拉取 blob（连接编排由调用方完成——
    /// kernel 门面经 Kad provider + 节点存在记录解析地址并 connect 后再调；
    /// 未连接按失败处理，不做 fan-out）。
    pub(super) fn begin_blob_fetch(&mut self, peer_id: &str, cid: Cid, tx: oneshot::Sender<Result<ContentBlobInfo>>) {
        let Ok(peer) = peer_id.parse::<PeerId>() else {
            let _ = tx.send(Err(P2pError::Malformed("invalid peer id".to_string())));
            return;
        };
        if !self.connected_peers().contains(&peer) {
            let _ = tx.send(Err(P2pError::Protocol(
                "blob provider not connected".to_string(),
            )));
            return;
        }
        let request_id = self
            .swarm
            .behaviour_mut()
            .blob_fetch_rr
            .send_request(&peer, direct::build_blob_fetch_request(cid.as_str()));
        self.pending_blob_fetch.insert(request_id, (cid, tx));
    }

    /// 应答侧：帧解析 → 限流 → 本地 content store 读出（读出重算校验完整性）。
    /// leaf 模式不服务（只消费），与 provide 关闭同口径；其余失败一律回错误
    /// 帧（线形不变），请求方按原因收敛。
    pub(super) fn handle_blob_fetch_inbound(
        &mut self,
        peer: PeerId,
        request: String,
        channel: request_response::ResponseChannel<String>,
    ) {
        let respond = |behaviour: &mut crate::p2p::behaviour::SparkBehaviour, text: String| {
            let _ = behaviour.blob_fetch_rr.send_response(channel, text);
        };
        if self.leaf_mode {
            respond(
                self.swarm.behaviour_mut(),
                direct::build_blob_fetch_response_err("leaf"),
            );
            return;
        }
        let Some(cid) = direct::parse_blob_fetch_request(&request) else {
            respond(
                self.swarm.behaviour_mut(),
                direct::build_blob_fetch_response_err("invalid-request"),
            );
            return;
        };
        if self
            .blob_fetch_limiter
            .is_rate_limited(&peer.to_base58(), self.now())
        {
            respond(
                self.swarm.behaviour_mut(),
                direct::build_blob_fetch_response_err("rate-limited"),
            );
            return;
        }
        match content::read_blob(&self.storage, &cid) {
            Ok(Some(data)) => respond(
                self.swarm.behaviour_mut(),
                direct::build_blob_fetch_response_ok(cid.as_str(), &B64.encode(&data)),
            ),
            Ok(None) => respond(
                self.swarm.behaviour_mut(),
                direct::build_blob_fetch_response_err("not-found"),
            ),
            // 存盘损坏/被篡改：绝不服务与 CID 不符的字节
            Err(_) => respond(
                self.swarm.behaviour_mut(),
                direct::build_blob_fetch_response_err("integrity-error"),
            ),
        }
    }

    /// 出站汇总：响应校验（帧形状 → CID 声明一致 → 大小上限 → 重算哈希
    /// 匹配）→ 落 content store → 持有即做种（非 leaf 自动登记 provider）。
    pub(super) fn resolve_blob_fetch(
        &mut self,
        request_id: request_response::OutboundRequestId,
        response: Option<String>,
    ) {
        let Some((cid, tx)) = self.pending_blob_fetch.remove(&request_id) else {
            return;
        };
        let Some(text) = response else {
            let _ = tx.send(Err(P2pError::Timeout("blob fetch failed".to_string())));
            return;
        };
        let parsed = direct::parse_blob_fetch_response(&text);
        let data = match parsed {
            Some(direct::BlobFetchResponse::Ok {
                cid: served_cid,
                data_base64,
            }) if served_cid == cid.as_str() => match B64.decode(&data_base64) {
                Ok(data) => data,
                Err(_) => {
                    let _ = tx.send(Err(P2pError::Protocol(
                        "blob fetch: invalid base64 in response".to_string(),
                    )));
                    return;
                }
            },
            Some(direct::BlobFetchResponse::Ok { .. }) => {
                let _ = tx.send(Err(P2pError::Protocol(
                    "blob fetch: response cid mismatch".to_string(),
                )));
                return;
            }
            Some(direct::BlobFetchResponse::Err(reason)) => {
                let _ = tx.send(Err(P2pError::Protocol(format!(
                    "blob fetch rejected: {reason}"
                ))));
                return;
            }
            None => {
                let _ = tx.send(Err(P2pError::Protocol(
                    "blob fetch: malformed response".to_string(),
                )));
                return;
            }
        };
        // 内容寻址校验：重算哈希与请求 CID 不符即拒绝，不落库不做种
        if Cid::from_data(&data) != cid {
            self.emit(P2pEvent::Warning(format!(
                "blob fetch: hash mismatch for {cid} (provider served wrong bytes)"
            )));
            let _ = tx.send(Err(P2pError::Protocol(
                "blob fetch: hash mismatch".to_string(),
            )));
            return;
        }
        let info = match content::save_blob(&mut self.storage, &data) {
            Ok(info) => info,
            Err(e) => {
                let _ = tx.send(Err(P2pError::Protocol(format!("blob store failed: {e}"))));
                return;
            }
        };
        // 持有即做种（public-topics §七）：拉到副本即自动成为 provider；
        // leaf 模式持有但不服务（与 begin_blob_provide 同判断）
        if !self.leaf_mode {
            let key = content::blob_kad_key(cid.as_str());
            self.provided_blobs.insert(key.clone());
            if let Some(kad) = self.swarm.behaviour_mut().kad.as_mut() {
                let _ = kad.start_providing(libp2p::kad::RecordKey::new(&key));
            }
        }
        let _ = tx.send(Ok(info));
    }
}

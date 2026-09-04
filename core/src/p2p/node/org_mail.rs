//! org-mail 直连入站（阶段四E，`/spark/org-mail/1.0.0`）：两 op（deliver/
//! fetch）的应答侧处理——解析帧 → 宿主 `handle_org_mail`（网关代收/挑战
//! 拉取在 kernel 层，见 kernel/org_mail_ops.rs）→ 序列化应答回传。
//!
//! 轻量同步路径（网关侧是 sled 读写的毫秒级操作，对齐 org-share 入站不走
//! spawn_blocking 的既有口径）。

use libp2p::PeerId;
use libp2p::request_response;

use super::event_loop::EventLoop;
use crate::storage::StorageBackend;

impl<S: StorageBackend> EventLoop<S> {
    /// org-mail 入站：形状非法/宿主错误 → `{ok:false}` 错误应答（错误文本
    /// 原样，对齐 §9.1 既有口径）；被撤销 peer 直接 internal-error。
    pub(super) fn handle_org_mail_inbound(
        &mut self,
        peer: PeerId,
        request: String,
        channel: request_response::ResponseChannel<String>,
    ) {
        let peer_id_str = peer.to_base58();
        let response = if self.host.is_revoked_peer(&peer_id_str) {
            serde_json::json!({ "ok": false, "reason": "internal-error" }).to_string()
        } else {
            match serde_json::from_str::<serde_json::Value>(&request) {
                Ok(payload) if payload.is_object() => {
                    match self.host.handle_org_mail(&payload, &peer_id_str) {
                        Ok(value) => value.to_string(),
                        Err(e) => {
                            self.emit(super::P2pEvent::Warning(format!(
                                "org-mail handle failed (peer {peer_id_str}): {e}"
                            )));
                            serde_json::json!({ "ok": false, "reason": "internal-error" })
                                .to_string()
                        }
                    }
                }
                _ => serde_json::json!({ "ok": false, "reason": "invalid-request" }).to_string(),
            }
        };
        let _ = self
            .swarm
            .behaviour_mut()
            .org_mail_rr
            .send_response(channel, response);
    }
}

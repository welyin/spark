//! dm 投递编排（`Kernel` 的内部方法）：信封构造/签名、对端寻址解析
//! （会话 peer → 朋友记录 → 组织 nodeInfo 择优回退）、spawn 异步投递
//! （chat 带状态回写 + ChatStatus 事件；控制信封与设备同步为尽力而为）。
//!
//! 全部方法为 `pub(crate)`/私有，供 `message_ops`/`contact_ops` 复用；
//! 异步投递 spawn 到 kernel runtime（不捕获 `&Kernel`），避免 Tauri
//! `Mutex<Kernel>` 横跨 dm_direct 的等待。
//!
//! 代码组织：本文件为入口——信封构造与 chat 投递准备/通知；异步投递任务
//! （spawn/退避重试）在 [`spawn`]，对端与自设备寻址解析（含自指污染自愈）
//! 在 [`addressing`]，资料同步广播与 sled 镜像在 [`profile_sync`]，通讯录/
//! 会话/设备快照广播在 [`device_sync`]，插件共享句柄
//! （[`PluginHostShared`](crate::plugin::PluginHostShared)）的同语义投递
//! 实现在 [`plugin_shared`]。

use serde_json::Value;

use super::dm_envelope::{self, KIND_CHAT};
use super::{Kernel, KernelError, Result};
use crate::message::{ConversationRecord, MessageRecord, MessageService};
use crate::p2p::PeerNodeInfo;
use crate::p2p::node::system_now_ms;

mod addressing;
mod device_sync;
mod flush;
mod plugin_shared;
mod profile_sync;
mod spawn;

pub(crate) use addressing::{
    heal_self_friend_to_healthy_device, heal_self_pointing_friend_record,
    list_self_device_peer_infos,
};
pub(crate) use flush::{flush_pending_for_recipient, is_terminal_rejection};
pub(crate) use spawn::{deliver_with_retry, enqueue_sync_pending};

/// 退避重试节奏（[`Kernel::spawn_deliveries_with_retry`]）：首次失败后 +2s、+5s。
pub(crate) const DM_RETRY_DELAYS: [std::time::Duration; 2] = [
    std::time::Duration::from_secs(2),
    std::time::Duration::from_secs(5),
];

impl Kernel {
    /// 以当前已解锁身份构造并签名 dm 信封。
    pub(crate) fn build_dm_envelope(&self, kind: &str, to: &str, body: Value) -> Result<Value> {
        let unlocked = self.unlocked.as_ref().ok_or(KernelError::Locked)?;
        Ok(dm_envelope::build_envelope(
            kind,
            &unlocked.root_id(),
            to,
            system_now_ms(),
            body,
            &unlocked.identity.signing_key,
        ))
    }

    /// 向所有已配对设备尽力投递 dm 信封（自消息/自回执的「同步到其他节点」
    /// 路径；单设备失败静默——离线设备恢复后的历史同步依赖后续个人空间
    /// 同步机制）。投递 spawn 到 kernel runtime，不阻塞命令线程。
    pub(crate) fn deliver_to_devices(&self, my_root_id: &str, kind: &str, body: Value) {
        self.plugin_host.deliver_to_devices(my_root_id, kind, body);
    }

    /// 解析对端并构造 chat 信封（发送的同步部分）；对端无地址或 p2p 未
    /// 运行返回 `Ok(None)`（调用方按 failed 处理）。
    ///
    /// **E2E 加密（S6 尾，2026-08-11 架构师裁决 root 密钥直接转换）**：个人
    /// 空间 direct 人际会话（1:1，feed 同语义）出站启用 E2E——用「我方 root
    /// 私钥 + 对端 root 公钥 X25519」派生临时会话密钥加密 body，携带 `ephPub`
    /// 构造签名信封。对端 root 公钥来自密钥表 `peerRootPub`（入站验签时积累）；
    /// 无记录视为内部错误（不静默降级明文）。组织空间 chat / 自消息 / bot
    /// 会话保持现状不加密（组织空间经 org-sync 链、对端 root 公钥来源未在
    /// 本期接线范围）。
    pub(crate) fn prepare_chat_delivery(
        &mut self,
        space: &str,
        conv: &ConversationRecord,
        record: &MessageRecord,
    ) -> Result<Option<(PeerNodeInfo, Value)>> {
        let Some(peer) = self.resolve_conv_peer(space, conv)? else {
            return Ok(None);
        };
        if self.p2p.is_none() {
            return Ok(None);
        }
        let body = serde_json::json!({
            "spaceKey": space,
            "message": serde_json::to_value(record)?,
        });
        let unlocked = self.unlocked.as_ref().ok_or(KernelError::Locked)?;
        // 先克隆出 E2E 加密所需的身份值（root 签名私钥 + rootId），避免后续
        // `require_storage_raw_mut` 的可变借用与 `self.unlocked` 不可变借用冲突。
        let my_signing_key = unlocked.identity.signing_key.clone();
        let my_root_id = unlocked.root_id();
        // 个人空间 direct 人际会话（非自消息）：E2E 加密出站
        let e2e_chat = space == "personal"
            && conv.kind == crate::message::ConversationKind::Direct
            && conv.peer_root_id != my_root_id;
        if e2e_chat {
            let ts = system_now_ms();
            let node_id = self.sync_node_id();
            let encrypt_result = crate::dm_e2e::encrypt_outbound_body(
                self.require_storage_raw_mut()?,
                &my_signing_key,
                &my_root_id,
                &conv.peer_root_id,
                KIND_CHAT,
                ts,
                &body,
                &node_id,
                ts,
            );
            // B1：E2E 加密失败（NoSessionKey 等，存量好友公钥尚未经入站验签积累）
            // → 消息置 failed + 用户可懂文案，不卡 sending。失败不可靠首投重试
            // （会话密钥未建立，重投无意义），等首次互连（profile-sync）交换公钥后
            // 重发。
            let (encrypted, eph_pub_b64) = match encrypt_result {
                Ok(v) => v,
                Err(e) => {
                    let copy = "加密会话尚未建立，请稍后重发";
                    // 调用方（message_send_text）已持 io_lock，此处直接落库
                    let wrote = {
                        MessageService::set_message_status_if_sending(
                            self.require_storage_raw_mut()?,
                            space,
                            &conv.id,
                            &record.id,
                            "failed",
                        )
                        .unwrap_or(false)
                    };
                    if wrote {
                        let _ = self.event_tx.send(crate::p2p::P2pEvent::ChatStatus(
                            serde_json::json!({
                                "spaceKey": space,
                                "convId": conv.id,
                                "messageId": record.id,
                                "status": "failed",
                            }),
                        ));
                    }
                    log::warn!("[chat] E2E encrypt failed, msg set failed: {e}");
                    return Err(KernelError::Internal(copy.to_string()));
                }
            };
            let envelope = dm_envelope::build_envelope_with_eph(
                KIND_CHAT,
                &my_root_id,
                &conv.peer_root_id,
                ts,
                encrypted,
                Some(&eph_pub_b64),
                &my_signing_key,
            );
            Ok(Some((peer, envelope)))
        } else {
            let envelope = self.build_dm_envelope(KIND_CHAT, &conv.peer_root_id, body)?;
            Ok(Some((peer, envelope)))
        }
    }

    /// 尽力向会话对端投递 read/recall 控制信封（失败静默；投递 spawn 到
    /// kernel runtime，不阻塞命令线程）。
    pub(crate) fn notify_peer(&self, space: &str, conv: &ConversationRecord, kind: &str, body: Value) {
        let Ok(Some(peer)) = self.resolve_conv_peer(space, conv) else {
            return;
        };
        if self.p2p.is_none() {
            return;
        }
        let Ok(envelope) = self.build_dm_envelope(kind, &conv.peer_root_id, body) else {
            return;
        };
        self.spawn_deliveries(vec![(peer, envelope)]);
    }
}

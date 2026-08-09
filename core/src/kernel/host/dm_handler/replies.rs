//! 入站结果的回发信封 spawn：把 `handle_dm` 产出的回发指令（friend-accept
//! 自动接受、profile/contact/conv/device 快照回发、pdsync 出站）装配成
//! 完整信封，经节点命令通道 `dm_direct` 尽力投递。全部方法在阻塞线程池
//! 线程内运行（不能 `block_on`）——`tokio::spawn` 到同一 runtime 驱动；
//! 节点未回填或身份已锁（无签名私钥）时静默跳过，不影响已完成的本地落库。

use serde_json::Value;

use crate::p2p::node::system_now_ms;
use crate::p2p::peer_targets::PeerNodeInfo;

use super::KernelDmHandler;
use crate::kernel::dm_envelope::{
    self, KIND_CONTACT_SYNC, KIND_CONV_SYNC, KIND_FRIEND_ACCEPT, KIND_PROFILE_SYNC,
};
use crate::kernel::inbound_dm::AutoAccept;

/// pdsync 发送侧 rate-limited 重试节奏：间隔 1.2s（略大于应答侧 1s 限流窗口
/// [`crate::p2p::constants::DM_MIN_INTERVAL_MS`]），至多重试 2 次。
const PDSYNC_RATE_LIMIT_RETRY_DELAY_MS: u64 = 1_200;
const PDSYNC_RATE_LIMIT_MAX_RETRIES: u32 = 2;

/// dm 应答是否为限流拒绝（应答侧错误帧 `{"ok": false, "reason": "rate-limited"}`）。
fn dm_response_is_rate_limited(response: &Option<Value>) -> bool {
    response
        .as_ref()
        .and_then(|v| v.get("reason"))
        .and_then(Value::as_str)
        == Some("rate-limited")
}

impl KernelDmHandler {
    /// 自动接受/重确认的回发：取本机节点信息装配 friend-accept 信封
    /// （设备配对 from==to==我；重确认 to=请求方 rootId），经节点命令通道
    /// 尽力投递。
    pub(super) fn spawn_auto_accept(&self, my_root_id: &str, nickname: &str, auto_accept: AutoAccept) {
        let node = self
            .node_shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let signing_key = self
            .signing_key_shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let (Some(node), Some(signing_key)) = (node, signing_key) else {
            return;
        };
        let from = my_root_id.to_string();
        let to = auto_accept.to_root_id.clone();
        let nickname = nickname.to_string();
        // 头像共享格（空串=无头像，body 省略 avatar 字段）
        let avatar = {
            let shared = self
                .avatar_shared
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            (!shared.trim().is_empty()).then_some(shared)
        };
        tokio::spawn(async move {
            let local = node.local_node_info().await.ok();
            let mut body = serde_json::json!({
                "requestId": auto_accept.request_id,
                "nickname": nickname,
            });
            if let Some(avatar) = avatar {
                body["avatar"] = serde_json::Value::from(avatar);
            }
            if let Some(info) = local {
                body["nodeInfo"] = serde_json::json!({
                    "peerId": info.peer_id,
                    "addresses": info.addresses,
                });
            }
            let envelope = dm_envelope::build_envelope(
                KIND_FRIEND_ACCEPT,
                &from,
                &to,
                system_now_ms(),
                body,
                &signing_key,
            );
            let _ = node.dm_direct(&auto_accept.target, envelope).await;
        });
    }

    /// profile-sync 握手回发：读身份文件的全量资料快照装配 profile-sync 信封
    /// 单点回投（本机资料较对端新时由 `apply_self_profile` 裁决触发；spawn
    /// 模式同 `spawn_device_sync_reply`，失败静默）。
    pub(super) fn spawn_profile_sync_reply(&self, my_root_id: &str, target: PeerNodeInfo) {
        let node = self
            .node_shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let signing_key = self
            .signing_key_shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let (Some(node), Some(signing_key)) = (node, signing_key) else {
            return;
        };
        let path = self
            .data_dir
            .join("identities")
            .join(format!("{my_root_id}.json"));
        let Ok(raw) = std::fs::read_to_string(&path) else {
            return;
        };
        let Ok(file) = crate::identity::IdentityFile::from_json(&raw) else {
            return;
        };
        // 昵称为空时回退 rootId 前 8 位（与出站口径一致）
        let nickname = file.nickname.clone().unwrap_or_default();
        let nickname = if nickname.trim().is_empty() {
            my_root_id.chars().take(8).collect::<String>()
        } else {
            nickname
        };
        let body = serde_json::json!({
            "nickname": nickname,
            "avatar": file.avatar,
            "gender": file.gender,
            "region": file.region,
            "signature": file.signature,
            "updatedAt": file.updated_at,
        });
        let to = my_root_id.to_string();
        tokio::spawn(async move {
            let envelope = dm_envelope::build_envelope(
                KIND_PROFILE_SYNC,
                &to,
                &to,
                system_now_ms(),
                body,
                &signing_key,
            );
            let _ = node.dm_direct(&target, envelope).await;
        });
    }

    /// contact-sync 配对回发：自动接受自设备配对的 friend-request 后，把
    /// 本机通讯录全量快照（朋友/申请/标签/分组/拉黑）回投给请求方设备——
    /// QR 恢复的新设备立即拿到通讯录，不必等断→连跳变或本地下一次变更
    /// （spawn 模式同 `spawn_auto_accept`，失败静默；对端 LWW 幂等合入）。
    pub(super) fn spawn_contact_sync_reply(&self, my_root_id: &str, target: PeerNodeInfo) {
        let node = self
            .node_shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let signing_key = self
            .signing_key_shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let (Some(node), Some(signing_key)) = (node, signing_key) else {
            return;
        };
        let Ok(body) = crate::contact::build_contact_sync_snapshot(&self.storage, my_root_id)
        else {
            return;
        };
        let to = my_root_id.to_string();
        tokio::spawn(async move {
            let envelope = dm_envelope::build_envelope(
                KIND_CONTACT_SYNC,
                &to,
                &to,
                system_now_ms(),
                body,
                &signing_key,
            );
            let _ = node.dm_direct(&target, envelope).await;
        });
    }

    /// conv-sync 配对回发：自动接受自设备配对的 friend-request 后，把
    /// 本机会话元数据快照（direct 会话外壳 + 置顶/免打扰/草稿）回投给
    /// 请求方设备——新设备立即拿到会话列表，不必等断→连跳变或本地下一次
    /// 变更（spawn 模式同 `spawn_contact_sync_reply`，失败静默）。
    pub(super) fn spawn_conv_sync_reply(&self, my_root_id: &str, target: PeerNodeInfo) {
        let node = self
            .node_shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let signing_key = self
            .signing_key_shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let (Some(node), Some(signing_key)) = (node, signing_key) else {
            return;
        };
        let Ok(body) = crate::message::build_conv_sync_snapshot(&self.storage) else {
            return;
        };
        let to = my_root_id.to_string();
        tokio::spawn(async move {
            let envelope = dm_envelope::build_envelope(
                KIND_CONV_SYNC,
                &to,
                &to,
                system_now_ms(),
                body,
                &signing_key,
            );
            let _ = node.dm_direct(&target, envelope).await;
        });
    }

    /// device-sync 握手回发：取本机设备记录（sled 设备清单的本机条目）装配
    /// device-sync 信封尽力回投——对端上线推送其记录时本机回推，双方设备
    /// 清单双向齐全（spawn 模式同 `spawn_auto_accept`，失败静默）。
    pub(super) fn spawn_device_sync_reply(&self, my_root_id: &str, target: PeerNodeInfo) {
        let node = self
            .node_shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let signing_key = self
            .signing_key_shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let (Some(node), Some(signing_key)) = (node, signing_key) else {
            return;
        };
        let storage = self.storage.clone();
        let to = my_root_id.to_string();
        tokio::spawn(async move {
            let Ok(info) = node.local_node_info().await else {
                return;
            };
            let Some(peer_id) = info.peer_id else {
                return;
            };
            let Ok(Some(record)) = crate::device::DeviceService::get(&storage, &peer_id) else {
                return;
            };
            let Ok(body) = serde_json::to_value(&record) else {
                return;
            };
            let envelope = dm_envelope::build_envelope(
                super::super::dm_envelope::KIND_DEVICE_SYNC,
                &to,
                &to,
                system_now_ms(),
                body,
                &signing_key,
            );
            let _ = node.dm_direct(&target, envelope).await;
        });
    }

    /// pdsync 出站投递：把纯逻辑层构建好的 hello/need/data body 装配成完整
    /// pdsync-* 信封，逐个 `dm_direct` 回投连接层对端（spawn 模式同
    /// `spawn_device_sync_reply`，失败静默）。
    ///
    /// 墓碑 ACK 重发：本批携带 dseq（墓碑记录）时，发完后等对端回执
    /// （其下轮 need/hello 的 dlogAck 推进本机水位），5s/15s 水位未覆盖
    /// 则整批重发——data 帧可能落在对端进程重启/连接中断窗口丢失（无
    /// 重发则退化到分钟级反熵）；落库侧 vv 幂等，重发无害。两轮后仍无
    /// 回执则放弃，交反熵兜底。
    pub(super) fn spawn_pdsync_reply(
        &self,
        my_root_id: &str,
        target: PeerNodeInfo,
        outputs: Vec<crate::kernel::inbound_dm::PdsyncOut>,
    ) {
        let node = self
            .node_shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let signing_key = self
            .signing_key_shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let (Some(node), Some(signing_key)) = (node, signing_key) else {
            return;
        };
        let to = my_root_id.to_string();
        // 本批推送的墓碑最大 dseq（ACK 重发判定依据；无墓碑则不等回执）
        let pushed_max_dseq: Option<u64> = outputs
            .iter()
            .filter_map(|out| match out {
                crate::kernel::inbound_dm::PdsyncOut::Push { body }
                | crate::kernel::inbound_dm::PdsyncOut::Data { body } => body_max_dseq(body),
                crate::kernel::inbound_dm::PdsyncOut::Need { .. }
                | crate::kernel::inbound_dm::PdsyncOut::AttachReq { .. }
                | crate::kernel::inbound_dm::PdsyncOut::AttachResp { .. } => None,
            })
            .max();
        let wm_peer_id = target.peer_id.clone();
        let storage = self.storage.clone();
        tokio::spawn(async move {
            send_pdsync_outputs(&node, &signing_key, &to, &target, &outputs).await;
            let (Some(max_dseq), Some(peer_id)) = (pushed_max_dseq, wm_peer_id) else {
                return;
            };
            for delay_ms in [5_000u64, 15_000] {
                tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                let watermark =
                    crate::sync::dlog::get_watermark(&storage, &peer_id).unwrap_or(0);
                if watermark >= max_dseq {
                    break; // 回执已到（对端 need/hello 的 dlogAck 推进了水位）
                }
                log::info!(
                    "[CT_SYNC] dlog retry | peer={} watermark={} pushed={}",
                    peer_id,
                    watermark,
                    max_dseq
                );
                send_pdsync_outputs(&node, &signing_key, &to, &target, &outputs).await;
            }
        });
    }
}

/// 出站 body 中墓碑记录的最大 dseq（无墓碑 → None）。
fn body_max_dseq(body: &Value) -> Option<u64> {
    body.get("records")?
        .as_array()?
        .iter()
        .filter_map(|r| r.get("dseq").and_then(Value::as_u64))
        .max()
}

/// 逐个装配并投递 pdsync 出站信封（rate-limited 有限重试；其余失败静默）。
async fn send_pdsync_outputs(
    node: &std::sync::Arc<crate::p2p::node::P2pNode>,
    signing_key: &ed25519_dalek::SigningKey,
    to: &str,
    target: &PeerNodeInfo,
    outputs: &[crate::kernel::inbound_dm::PdsyncOut],
) {
    for output in outputs {
        let kind = match output {
            crate::kernel::inbound_dm::PdsyncOut::Push { .. } => {
                crate::kernel::dm_envelope::KIND_PDSYNC_DATA
            }
            crate::kernel::inbound_dm::PdsyncOut::Need { .. } => {
                crate::kernel::dm_envelope::KIND_PDSYNC_NEED
            }
            crate::kernel::inbound_dm::PdsyncOut::Data { .. } => {
                crate::kernel::dm_envelope::KIND_PDSYNC_DATA
            }
            crate::kernel::inbound_dm::PdsyncOut::AttachReq { .. } => {
                crate::kernel::dm_envelope::KIND_PDSYNC_ATTACHMENT_REQ
            }
            crate::kernel::inbound_dm::PdsyncOut::AttachResp { .. } => {
                crate::kernel::dm_envelope::KIND_PDSYNC_ATTACHMENT_RESP
            }
        };
        let envelope = dm_envelope::build_envelope(
            kind,
            to,
            to,
            system_now_ms(),
            output.body().clone(),
            signing_key,
        );
        // rate-limited 有限重试：pdsync-* 已入应答侧限流豁免名单，
        // 但对端可能是未升级的旧版本（仍按 1s 窗口限流连发信封）——
        // 隔 1.2s（略大于限流窗口）重试至多 2 次；其余失败维持静默
        // （反熵是周期性的，本轮丢失下一轮补齐，不无限放大）。
        let mut retries = 0;
        loop {
            let response = node.dm_direct(target, envelope.clone()).await;
            let rate_limited = matches!(&response, Ok(v) if dm_response_is_rate_limited(v));
            if !rate_limited || retries >= PDSYNC_RATE_LIMIT_MAX_RETRIES {
                break;
            }
            retries += 1;
            tokio::time::sleep(std::time::Duration::from_millis(
                PDSYNC_RATE_LIMIT_RETRY_DELAY_MS,
            ))
            .await;
        }
    }
}

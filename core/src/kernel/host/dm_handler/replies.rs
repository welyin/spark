//! 入站结果的回发信封 spawn：把 `handle_dm` 产出的回发指令（friend-accept
//! 自动接受、profile/contact/conv/device 快照回发、pdsync 出站）装配成
//! 完整信封，经节点命令通道 `dm_direct` 尽力投递。全部方法在阻塞线程池
//! 线程内运行（不能 `block_on`）——`tokio::spawn` 到同一 runtime 驱动；
//! 节点未回填或身份已锁（无签名私钥）时静默跳过，不影响已完成的本地落库。

use serde_json::Value;

use crate::p2p::node::system_now_ms;
use crate::p2p::peer_targets::PeerNodeInfo;

use super::KernelDmHandler;
use crate::kernel::dm_delivery::list_self_device_peer_infos;
use crate::kernel::dm_envelope::{
    self, KIND_CONTACT_SYNC, KIND_CONV_SYNC, KIND_DEVICE_NOTICE, KIND_FRIEND_ACCEPT,
    KIND_PROFILE_SYNC,
};
use crate::kernel::inbound_dm::AutoAccept;
use crate::p2p::constants::P2P_DEVICE_NOTICE_SENT_PREFIX;
use crate::storage::StorageBackend;

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

    /// 设备加入通知广播（M1）：向全部已配对自设备广播本机 `device_joined`
    /// 通知。body 形状：`{kind:"device_joined", deviceId, deviceName, ts}`。
    /// 排除本机与已 `noticeSent` 者；成功后写幂等标记。
    pub(super) fn spawn_device_notice_broadcast(&self, my_root_id: &str) {
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
        let mut storage = self.storage.clone();
        let from = my_root_id.to_string();
        let to = my_root_id.to_string();
        tokio::spawn(async move {
            let Ok(info) = node.local_node_info().await else {
                return;
            };
            let Some(local_peer_id) = info.peer_id else {
                return;
            };
            let device_name = crate::device::DeviceService::get(&storage, &local_peer_id)
                .ok()
                .flatten()
                .map(|r| r.device_name)
                .unwrap_or_else(|| crate::device::collect_local_device_info().device_name);
            let body = serde_json::json!({
                "kind": "device_joined",
                "deviceId": local_peer_id.clone(),
                "deviceName": device_name,
                "ts": system_now_ms(),
            });
            let friends = crate::contact::ContactService::overview(&storage, "personal")
                .ok()
                .map(|v| v.friends)
                .unwrap_or_default();
            let devices = crate::device::DeviceService::list(&storage).unwrap_or_default();
            let targets = list_self_device_peer_infos(friends, devices, &to, Some(&local_peer_id));
            for target in targets {
                let Some(target_peer_id) = target.peer_id.as_deref() else {
                    continue;
                };
                if target_peer_id == local_peer_id {
                    continue;
                }
                let sent_key = format!("{P2P_DEVICE_NOTICE_SENT_PREFIX}{target_peer_id}");
                if storage.get(&sent_key).ok().flatten().is_some() {
                    continue;
                }
                let envelope = dm_envelope::build_envelope(
                    KIND_DEVICE_NOTICE,
                    &from,
                    &to,
                    system_now_ms(),
                    body.clone(),
                    &signing_key,
                );
                if node.dm_direct(&target, envelope).await.is_ok() {
                    let _ = storage.put(&sent_key, "1");
                }
            }
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
        let node_id = self.sync_node_id();
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
        let mut storage = self.storage.clone();
        tokio::spawn(async move {
            // L4（mobile-leaf-mode §6）：确认不可达的 pdsync-data 入
            // `dm:pending:`（个人空间，to=自 rootId），连接恢复时经
            // `on_peer_app_ready` flush 重发；对端 vv 幂等合入
            let failed = send_pdsync_outputs(&node, &signing_key, &to, &target, &outputs).await;
            enqueue_pdsync_failures(&mut storage, &to, &failed, &node_id);
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
                let failed =
                    send_pdsync_outputs(&node, &signing_key, &to, &target, &outputs).await;
                enqueue_pdsync_failures(&mut storage, &to, &failed, &node_id);
            }
        });
    }

    /// orgsync 出站投递：把纯逻辑层构建好的 hello/need/data body 装配成完整
    /// orgsync-* 信封（kind=KIND_ORGSYNC_*、from=本机 rootId、
    /// to=对端成员 rootId，后者由每个 OrgsyncOut 携带——B1），逐个
    /// `dm_direct` 回投（spawn 模式同 `spawn_pdsync_reply`，失败静默）。
    ///
    /// 墓碑 ACK 重发（F2）对齐 pdsync replies.rs:309-313：本批携带 dseq 时，
    /// 5s/15s 水位未覆盖（对端已确认序号 ≥ 本批最大 dseq）则整批重发；
    /// 水位已覆盖 → break 不再重发。水位键用 org 域 `dlog:org:{orgId}:{name}@v{version}:wm:{rootId}:{peerId}`
    /// （B6 按 (rootId, peerId) 设备粒度）。
    pub(super) fn spawn_orgsync_reply(
        &self,
        my_root_id: &str,
        target: PeerNodeInfo,
        outputs: Vec<crate::kernel::inbound_dm::OrgsyncOut>,
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
        // orgsync 出站：from=本机 rootId, to=各 output 携带的目标成员 rootId
        let from = my_root_id.to_string();
        // 本批所有 output 的目标成员 rootId（B1：入站 handler 从信封 from 透传，
        // 全部指向同一对端成员）
        let to_member_root_id = outputs
            .first()
            .map(|o| o.to_root_id().to_string())
            .unwrap_or_else(|| my_root_id.to_string());
        let pushed_max_dseq: Option<u64> = outputs
            .iter()
            .filter_map(|out| match out {
                crate::kernel::inbound_dm::OrgsyncOut::Data { body, .. } => body_max_dseq(body),
                crate::kernel::inbound_dm::OrgsyncOut::Need { .. }
                | crate::kernel::inbound_dm::OrgsyncOut::OrgqReq { .. }
                | crate::kernel::inbound_dm::OrgsyncOut::OrgqResp { .. } => None,
            })
            .max();
        let wm_peer_id = target.peer_id.clone();
        let storage = self.storage.clone();
        // 对端成员 rootId（GC/水位查询的 root 键段）+ 本批 (org_id, name, version)
        // 从 Data body 解析（需要重发判定）。
        let ack_ctx = outputs
            .iter()
            .filter_map(|out| match out {
                crate::kernel::inbound_dm::OrgsyncOut::Data { body, .. } => {
                    body_org_scope(body).map(|s| (to_member_root_id.clone(), s))
                }
                _ => None,
            })
            .next();
        let retry_first_ms = crate::sync::orgsync::ORGSYNC_DLOG_ACK_RETRY_FIRST_MS as u64;
        let retry_second_ms = crate::sync::orgsync::ORGSYNC_DLOG_ACK_RETRY_SECOND_MS as u64;
        let node_id = self.sync_node_id();
        let mut storage = storage;
        tokio::spawn(async move {
            // L4（mobile-leaf-mode §6）：确认不可达的 orgsync-data 入
            // `org:dm:pending:`（to=成员 rootId），成员设备 app-ready 时经
            // `on_peer_app_ready` flush 重发；对端 dlog/vv 幂等合入
            let failed = send_orgsync_outputs(
                &node,
                &signing_key,
                &from,
                &target,
                &outputs,
            )
            .await;
            enqueue_orgsync_failures(&mut storage, &failed, &node_id);
            let (Some(max_dseq), Some(peer_id), Some((root_id, (org_id, name, version)))) =
                (pushed_max_dseq, wm_peer_id, ack_ctx)
            else {
                return;
            };
            // F2：org 域 dlog 水位重发，5s/15s 水位未覆盖则重发、已覆盖则 break
            for delay_ms in [retry_first_ms, retry_second_ms] {
                tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                let wm = crate::sync::orgsync::org_dlog_get_watermark(
                    &storage, &org_id, &name, &version, &root_id, &peer_id,
                )
                .unwrap_or(0);
                if wm >= max_dseq {
                    break; // 回执已到（对端 need/hello 的 dlogAck 推进了水位）
                }
                log::info!(
                    "[ORGSYNC] dlog retry | root={} watermark={} pushed={}",
                    root_id,
                    wm,
                    max_dseq
                );
                let failed = send_orgsync_outputs(
                    &node,
                    &signing_key,
                    &from,
                    &target,
                    &outputs,
                )
                .await;
                enqueue_orgsync_failures(&mut storage, &failed, &node_id);
            }
        });
    }

    /// 回发 feed-blob 出站信封（feed-blob-req/resp，跨联系人分块传输通道）。
    /// 单条输出（`InboundDmResult::feed_blob_out`），from=本机 rootId、
    /// to=连接层对端 rootId（feed 原作者/拉取方）。spawn 到 runtime 投递，
    /// 失败静默（分块传输由请求方下轮调和重试兜底）。
    pub(super) fn spawn_feed_blob_reply(
        &self,
        my_root_id: &str,
        target: PeerNodeInfo,
        output: crate::kernel::inbound_dm::FeedBlobOut,
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
        let from = my_root_id.to_string();
        // feed-blob 信封 to = 连接层对端 rootId（feed 原作者或拉取方）
        let to = target.peer_id.clone().unwrap_or_else(|| from.clone());
        let envelope = dm_envelope::build_envelope(
            output.kind(),
            &from,
            &to,
            system_now_ms(),
            output.body().clone(),
            &signing_key,
        );
        tokio::spawn(async move {
            let _ = node.dm_direct(&target, envelope).await;
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

/// 从 orgsync-data body 解析 (orgId, name, version)（org dlog 水位查询用）。
fn body_org_scope(body: &Value) -> Option<(String, String, String)> {
    let org_id = body.get("orgId")?.as_str()?.to_string();
    let col_full = body.get("collection")?.as_str()?;
    let at = col_full.rfind("@v")?;
    let name = col_full[..at].to_string();
    let version = col_full[at + 2..].to_string();
    Some((org_id, name, version))
}

/// 逐个装配并投递 pdsync 出站信封（rate-limited 有限重试；其余失败静默）。
///
/// 返回**确认不可达**（`Ok(None)` 拨号失败/超时/传输错误，rate-limited 重试
/// 耗尽不算——对端在线只是限流）的 `pdsync-data` 失败项 `(body, envelope)`，
/// 供调用方入 `dm:pending:` 离线队列（mobile-leaf-mode §6 L4）；need/附件
/// 类不收集（请求与分块由反熵/下轮调和重发，非集合数据本体）。
async fn send_pdsync_outputs(
    node: &std::sync::Arc<crate::p2p::node::P2pNode>,
    signing_key: &ed25519_dalek::SigningKey,
    to: &str,
    target: &PeerNodeInfo,
    outputs: &[crate::kernel::inbound_dm::PdsyncOut],
) -> Vec<(Value, Value)> {
    let mut failed: Vec<(Value, Value)> = Vec::new();
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
        let response = loop {
            let response = node.dm_direct(target, envelope.clone()).await;
            let rate_limited = matches!(&response, Ok(v) if dm_response_is_rate_limited(v));
            if !rate_limited || retries >= PDSYNC_RATE_LIMIT_MAX_RETRIES {
                break response;
            }
            retries += 1;
            tokio::time::sleep(std::time::Duration::from_millis(
                PDSYNC_RATE_LIMIT_RETRY_DELAY_MS,
            ))
            .await;
        };
        // L4 离线暂存：确认不可达的集合数据（pdsync-data）收集返回，由调用方
        // 入 pending；`Ok(Some(_))`（含限流重试耗尽，对端在线）不算失败。
        if !matches!(&response, Ok(Some(_)))
            && kind == crate::kernel::dm_envelope::KIND_PDSYNC_DATA
        {
            failed.push((output.body().clone(), envelope));
        }
    }
    failed
}

/// 检查 device_joined 通知补发窗口是否仍然有效。
pub(super) fn device_notice_window_open(storage: &crate::storage::SledStorage, now_ms: i64) -> bool {
    use crate::p2p::constants::P2P_DEVICE_NOTICE_SELF_UNTIL;
    storage
        .get(P2P_DEVICE_NOTICE_SELF_UNTIL)
        .ok()
        .flatten()
        .and_then(|v| v.parse::<i64>().ok())
        .is_some_and(|until| now_ms <= until)
}

/// 检查是否已向指定 peer 发送过 device_joined 通知（幂等键）。
pub(super) fn device_notice_sent(storage: &crate::storage::SledStorage, peer_id: &str) -> bool {
    storage
        .get(&format!("{P2P_DEVICE_NOTICE_SENT_PREFIX}{peer_id}"))
        .ok()
        .flatten()
        .is_some()
}

/// 逐个装配并投递 orgsync 出站信封（rate-limited 有限重试；其余失败静默）。
/// kind 为 KIND_ORGSYNC_*、from=本机 rootId、to=各 output 携带的目标成员
/// rootId（B1）。
///
/// 返回**确认不可达**的 `orgsync-data` 失败项 `(toRootId, body, envelope)`
/// （判定同 [`send_pdsync_outputs`]），供调用方入 `org:dm:pending:` 离线
/// 队列（mobile-leaf-mode §6 L4）；need/orgq 问答类不收集（重查即补）。
async fn send_orgsync_outputs(
    node: &std::sync::Arc<crate::p2p::node::P2pNode>,
    signing_key: &ed25519_dalek::SigningKey,
    from: &str,
    target: &PeerNodeInfo,
    outputs: &[crate::kernel::inbound_dm::OrgsyncOut],
) -> Vec<(String, Value, Value)> {
    let mut failed: Vec<(String, Value, Value)> = Vec::new();
    for output in outputs {
        let kind = match output {
            crate::kernel::inbound_dm::OrgsyncOut::Need { .. } => {
                crate::kernel::dm_envelope::KIND_ORGSYNC_NEED
            }
            crate::kernel::inbound_dm::OrgsyncOut::Data { .. } => {
                crate::kernel::dm_envelope::KIND_ORGSYNC_DATA
            }
            crate::kernel::inbound_dm::OrgsyncOut::OrgqReq { .. } => {
                crate::kernel::dm_envelope::KIND_ORGQ_REQ
            }
            crate::kernel::inbound_dm::OrgsyncOut::OrgqResp { .. } => {
                crate::kernel::dm_envelope::KIND_ORGQ_RESP
            }
        };
        // B1：每个 output 携带目标成员 rootId（信封 to），而非统一的入站 from
        let to = output.to_root_id();
        let envelope = dm_envelope::build_envelope(
            kind,
            from,
            to,
            system_now_ms(),
            output.body().clone(),
            signing_key,
        );
        // rate-limited 有限重试（与 pdsync 同口径）
        let mut retries = 0;
        let response = loop {
            let response = node.dm_direct(target, envelope.clone()).await;
            let rate_limited = matches!(&response, Ok(v) if dm_response_is_rate_limited(v));
            if !rate_limited || retries >= PDSYNC_RATE_LIMIT_MAX_RETRIES {
                break response;
            }
            retries += 1;
            tokio::time::sleep(std::time::Duration::from_millis(
                PDSYNC_RATE_LIMIT_RETRY_DELAY_MS,
            ))
            .await;
        };
        // L4 离线暂存：确认不可达的集合数据（orgsync-data）收集返回，由调用方
        // 入 pending；`Ok(Some(_))`（含限流重试耗尽，对端在线）不算失败。
        if !matches!(&response, Ok(Some(_)))
            && kind == crate::kernel::dm_envelope::KIND_ORGSYNC_DATA
        {
            failed.push((to.to_string(), output.body().clone(), envelope));
        }
    }
    failed
}

/// L4 入队帮助（mobile-leaf-mode §6）：pdsync 确认不可达的 data 失败项入
/// 个人空间 `dm:pending:`（to=自 rootId；自设备场景 from==to，flush 由
/// `on_peer_app_ready` 朋友自记录路径触发，天然兼容）。
fn enqueue_pdsync_failures(
    storage: &mut crate::storage::SledStorage,
    to: &str,
    failed: &[(Value, Value)],
    node_id: &str,
) {
    for (body, envelope) in failed {
        crate::kernel::dm_delivery::enqueue_sync_pending(
            storage,
            crate::dm_offline::PendingSpace::Personal,
            to,
            crate::kernel::dm_envelope::KIND_PDSYNC_DATA,
            body,
            envelope,
            node_id,
        );
    }
}

/// L4 入队帮助（§6）：orgsync 确认不可达的 data 失败项入组织空间
/// `org:dm:pending:`；orgId 从 data body 解析（`body_org_scope` 既有口径），
/// 解析不出（形状异常）跳过——反熵兜底语义不变。
fn enqueue_orgsync_failures(
    storage: &mut crate::storage::SledStorage,
    failed: &[(String, Value, Value)],
    node_id: &str,
) {
    for (to, body, envelope) in failed {
        let Some((org_id, _, _)) = body_org_scope(body) else {
            continue;
        };
        crate::kernel::dm_delivery::enqueue_sync_pending(
            storage,
            crate::dm_offline::PendingSpace::Org(&org_id),
            to,
            crate::kernel::dm_envelope::KIND_ORGSYNC_DATA,
            body,
            envelope,
            node_id,
        );
    }
}

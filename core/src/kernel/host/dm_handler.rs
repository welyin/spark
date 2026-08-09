//! kernel 的 dm 入站处理器（[`DmHandler`] 实现）：把 dm 直连接收委托给
//! `inbound_dm` 纯逻辑编排（验签/落库/事件），并把需要 host/kernel 侧资源
//! （节点句柄/签名私钥/会话口令）的回发指令装配成信封 spawn 到 runtime 投递。
//!
//! 从 `host` 拆出的子模块（文件长度约束）：[`KernelDmHandler`] 字段全部为
//! `Arc`/`SledStorage` 克隆（`Send + Sync`），由事件循环 spawn 到阻塞线程池执行
//! （验签/落库等重 IO 不占事件循环线程）；本模块只做装配与错误映射，业务规则
//! 全在 `super::super::inbound_dm`。

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::p2p::host::DmHandler;
use crate::p2p::node::system_now_ms;
use crate::p2p::peer_targets::PeerNodeInfo;
use crate::p2p::P2pNode;
use crate::storage::{StorageBackend, SledStorage};

use super::super::dm_envelope::{
    self, KIND_CONTACT_SYNC, KIND_CONV_SYNC, KIND_FRIEND_ACCEPT, KIND_PROFILE_SYNC,
};
use super::super::inbound_dm::AutoAccept;

/// kernel 的 dm 入站处理器：字段全部为 `Arc`/`SledStorage` 克隆，`Send + Sync`，
/// 由事件循环 spawn 到阻塞线程池执行（验签/落库等重 IO 不占事件循环线程）。
pub(crate) struct KernelDmHandler {
    pub(crate) storage: SledStorage,
    pub(crate) current_root_id: Arc<Mutex<Option<String>>>,
    pub(crate) nickname_shared: Arc<Mutex<String>>,
    pub(crate) avatar_shared: Arc<Mutex<String>>,
    pub(crate) event_tx: tokio::sync::broadcast::Sender<crate::p2p::P2pEvent>,
    pub(crate) node_shared: Arc<Mutex<Option<Arc<P2pNode>>>>,
    pub(crate) signing_key_shared: Arc<Mutex<Option<ed25519_dalek::SigningKey>>>,
    pub(crate) password_shared: Arc<Mutex<Option<String>>>,
    pub(crate) data_dir: std::path::PathBuf,
    pub(crate) io_lock: Arc<Mutex<()>>,
    /// 已证明支持 pdsync 的自设备 peerId 集合（收尾能力探测，§7.1，按设备粒度）。
    pub(crate) pdsync_capable_self_devices: Arc<Mutex<std::collections::HashSet<String>>>,
}

impl KernelDmHandler {
    /// 收尾（§7.1）：标记某自设备（连接层 peerId）已证明支持 pdsync。保活据此
    /// 停止向其回退发旧快照。幂等（集合内重复无影响）。
    fn kernel_pdsync_capable_mark(&self, peer_id: &str) {
        self.pdsync_capable_self_devices
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(peer_id.to_string());
    }

    /// 本地写入节点 id：p2p 运行中为 peerId；否则回退持久化 p2p 身份派生的
    /// 稳定 id（见 [`super::super::doc_ops::persisted_sync_node_id`]，避免多台离线
    /// 设备共用 `local-node` 被向量比较判为同源）。
    fn sync_node_id(&self) -> String {
        self.node_shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|node| node.peer_id().to_string())
            .unwrap_or_else(|| super::super::doc_ops::persisted_sync_node_id(&self.storage))
    }

    /// 收尾（§7.1）能力标记判定：仅当 pdsync-* 信封**验签通过**且
    /// `from == 本机 rootId`（验签已把 from 绑定根公钥，确为自设备）时，
    /// 返回应标记的连接层 peerId。
    ///
    /// 两处防误标：
    /// - 验签前不可按 payload kind 字符串标记——伪造信封会欺骗能力探测；
    /// - 按连接层 peerId（每设备唯一）而非 rootId 键控——同身份所有设备共
    ///   享 rootId，按 rootId 标记会让一台新设备停掉所有自设备的旧快照
    ///   回退（旧版本设备从此收不到数据，§7.1 回退被破坏）。
    pub(crate) fn pdsync_capability_mark(
        payload: &Value,
        my_root_id: &str,
        remote_peer_id: &str,
    ) -> Option<String> {
        let kind = payload.get("kind").and_then(Value::as_str)?;
        if !matches!(
            kind,
            super::super::dm_envelope::KIND_PDSYNC_HELLO
                | super::super::dm_envelope::KIND_PDSYNC_NEED
                | super::super::dm_envelope::KIND_PDSYNC_DATA
        ) {
            return None;
        }
        let verified =
            dm_envelope::verify_envelope(payload, my_root_id, system_now_ms()).ok()?;
        (verified.from == my_root_id).then(|| remote_peer_id.to_string())
    }

    /// 自动接受/重确认的回发：取本机节点信息装配 friend-accept 信封
    /// （设备配对 from==to==我；重确认 to=请求方 rootId），经节点命令通道
    /// 尽力投递。
    ///
    /// 本方法在阻塞线程池线程内运行（不能 `block_on`）——`tokio::spawn`
    /// 到同一 runtime 驱动（事件循环空闲时处理 DmDirect 命令）；节点未回填或
    /// 身份已锁（无签名私钥）时静默跳过，不影响已完成的本地落库。
    fn spawn_auto_accept(&self, my_root_id: &str, nickname: &str, auto_accept: AutoAccept) {
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

    /// 自设备 profile-sync 全量快照应用：以会话口令重封身份文件，完成
    /// 「我的资料」跨设备同步（wiki/design/sync-and-evidence.md「个人空间
    /// 同步口径」：个人资料在个人设备间全量同步；identity.md §5「恢复后
    /// 头像经 profile-sync 找回」）。
    ///
    /// LWW 三向裁决（向量时钟为后续专项）：
    /// - 对端较新（`updatedAt` 严格大于本地）：应用快照，刷新共享槽并通知
    ///   前端（SelfProfileSynced）——防离线设备上线后以旧快照回灌；
    /// - 本机较新（严格小于）：返回 true，调用方据此向对端回发本机全量
    ///   快照（握手式交换——对端较旧/残缺时补齐，如 QR 恢复的新设备
    ///   updatedAt=0，其残缺快照不会覆盖本机资料，本机回发使其收敛）；
    /// - 相等：收敛态，不动（也不回发，无 ping-pong）。
    ///
    /// 身份已锁（无口令）/文件缺失/校验失败时静默跳过（返回 false），
    /// 不影响朋友记录已完成的更新。
    fn apply_self_profile(&self, root_id: &str, body: &Value) -> bool {
        let password = self
            .password_shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let Some(password) = password else {
            return false;
        };
        let Some(updated_at) = body.get("updatedAt").and_then(Value::as_i64) else {
            return false;
        };
        let path = self
            .data_dir
            .join("identities")
            .join(format!("{root_id}.json"));
        let Ok(raw) = std::fs::read_to_string(&path) else {
            return false;
        };
        let Ok(mut file) = crate::identity::IdentityFile::from_json(&raw) else {
            return false;
        };
        let has_session = self
            .password_shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some();
        log::info!(
            "[PROFILE_CHAIN] apply_self_profile | incoming.updatedAt={} file.updated_at={} has_password={} sig={:?}",
            updated_at,
            file.updated_at,
            has_session,
            body.get("signature"),
        );
        if updated_at < file.updated_at as i64 {
            // 本机资料较新：提示调用方回发本机快照补齐对端
            return true;
        }
        if updated_at == file.updated_at as i64 {
            return false;
        }
        // 线形三态 → update_profile 参数三态：字符串=设置，显式 null=清除，缺省=不变
        let nickname = body.get("nickname").and_then(Value::as_str);
        let tri_state = |key: &str| -> Option<Option<&str>> {
            match body.get(key) {
                Some(Value::Null) => Some(None),
                Some(Value::String(s)) => Some(Some(s.as_str())),
                _ => None,
            }
        };
        let avatar = tri_state("avatar");
        // gender/region/signature 的内核清除语义是 Some("")（空串=清除）
        let extra = |key: &str| -> Option<&str> {
            match body.get(key) {
                Some(Value::Null) => Some(""),
                Some(Value::String(s)) => Some(s.as_str()),
                _ => None,
            }
        };
        if crate::identity::update_profile(
            &mut file,
            &password,
            nickname,
            avatar,
            extra("gender"),
            extra("region"),
            extra("signature"),
        )
        .is_err()
        {
            return false;
        }
        let Ok(text) = serde_json::to_string_pretty(&file) else {
            return false;
        };
        if super::super::identity::write_identity_file_atomic(&path, &text).is_err() {
            return false;
        }
        // 共享格刷新（dm 应答/出站口径）+ 前端通知
        *self.nickname_shared.lock().unwrap_or_else(|e| e.into_inner()) =
            file.nickname.clone().unwrap_or_default();
        *self.avatar_shared.lock().unwrap_or_else(|e| e.into_inner()) =
            file.avatar.clone().unwrap_or_default();
        let mut data = serde_json::json!({
            "nickname": file.nickname.clone().unwrap_or_default(),
        });
        if let Some(a) = &file.avatar {
            data["avatar"] = Value::from(a.clone());
        }
        let _ = self.event_tx.send(crate::p2p::P2pEvent::SelfProfileSynced(data));
        false
    }

    /// pdsync 合入 `profile:self` 后回写身份文件（P2）。
    ///
    /// sled 镜像已由 `handle_pdsync_data` LWW 落地；这里把 sled 的最新资料
    /// 同步回身份文件（保证两处一致）。仅解锁态（有口令）可重封身份文件；
    /// 锁定态跳过——下次 unlock 时以 sled 覆盖（见 login）。写失败静默。
    fn apply_profile_from_sled(&self, root_id: &str) {
        let Some(password) = self
            .password_shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        else {
            return;
        };
        // 读 sled profile:self
        let Some(raw) = self
            .storage
            .get(super::super::identity::PROFILE_SELF_KEY)
            .ok()
            .flatten()
        else {
            return;
        };
        let Ok(profile) = serde_json::from_str::<super::super::identity::SyncableProfile>(&raw) else {
            return;
        };
        let info = profile.to_profile_info();
        let path = self
            .data_dir
            .join("identities")
            .join(format!("{root_id}.json"));
        let Ok(raw_file) = std::fs::read_to_string(&path) else {
            return;
        };
        let Ok(mut file) = crate::identity::IdentityFile::from_json(&raw_file) else {
            return;
        };
        let file_ts_before = file.updated_at;
        let sled_ts = crate::sync::personal::get_personal_meta(
            &self.storage,
            super::super::identity::PROFILE_SELF_KEY,
        )
        .ok()
        .flatten()
        .map(|m| m.ts);
        // 以 sled 为源（pdsync 已 LWW 裁决，sled profile:self 是本次合入胜者），
        // 回写身份文件资料字段。
        if crate::identity::update_profile(
            &mut file,
            &password,
            info.nickname.as_deref(),
            match info.avatar.as_deref() {
                Some(a) if !a.is_empty() => Some(Some(a)),
                _ => Some(None),
            },
            Some(info.gender.as_deref().unwrap_or("")),
            Some(info.region.as_deref().unwrap_or("")),
            Some(info.signature.as_deref().unwrap_or("")),
        )
        .is_err()
        {
            return;
        }
        log::info!(
            "[PROFILE_CHAIN] apply_profile_from_sled | file.updated_at {} -> {} | sled pmeta.ts={:?} | sig={:?}",
            file_ts_before,
            file.updated_at,
            sled_ts,
            info.signature,
        );
        let Ok(text) = serde_json::to_string_pretty(&file) else {
            return;
        };
        if super::super::identity::write_identity_file_atomic(&path, &text).is_err() {
            return;
        }
        *self.nickname_shared.lock().unwrap_or_else(|e| e.into_inner()) =
            file.nickname.clone().unwrap_or_default();
        *self.avatar_shared.lock().unwrap_or_else(|e| e.into_inner()) =
            file.avatar.clone().unwrap_or_default();
        // 前端通知（与 apply_self_profile 同口径）：我的资料已被自设备同步更新
        let mut data = serde_json::json!({
            "nickname": file.nickname.clone().unwrap_or_default(),
        });
        if let Some(a) = &file.avatar {
            data["avatar"] = Value::from(a.clone());
        }
        let _ = self.event_tx.send(crate::p2p::P2pEvent::SelfProfileSynced(data));
    }

    /// profile-sync 握手回发：读身份文件的全量资料快照装配 profile-sync 信封
    /// 单点回投（本机资料较对端新时由 `apply_self_profile` 裁决触发；spawn
    /// 模式同 `spawn_device_sync_reply`，失败静默）。
    fn spawn_profile_sync_reply(&self, my_root_id: &str, target: PeerNodeInfo) {
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
    fn spawn_contact_sync_reply(&self, my_root_id: &str, target: PeerNodeInfo) {
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
    fn spawn_conv_sync_reply(&self, my_root_id: &str, target: PeerNodeInfo) {
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
    fn spawn_device_sync_reply(&self, my_root_id: &str, target: PeerNodeInfo) {
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
    fn spawn_pdsync_reply(
        &self,
        my_root_id: &str,
        target: PeerNodeInfo,
        outputs: Vec<super::super::inbound_dm::PdsyncOut>,
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
        tokio::spawn(async move {
            for output in outputs {
                let kind = match &output {
                    super::super::inbound_dm::PdsyncOut::Push { .. } => {
                        super::super::dm_envelope::KIND_PDSYNC_DATA
                    }
                    super::super::inbound_dm::PdsyncOut::Need { .. } => {
                        super::super::dm_envelope::KIND_PDSYNC_NEED
                    }
                    super::super::inbound_dm::PdsyncOut::Data { .. } => {
                        super::super::dm_envelope::KIND_PDSYNC_DATA
                    }
                };
                let body = match &output {
                    super::super::inbound_dm::PdsyncOut::Push { body }
                    | super::super::inbound_dm::PdsyncOut::Need { body }
                    | super::super::inbound_dm::PdsyncOut::Data { body } => body.clone(),
                };
                let envelope = dm_envelope::build_envelope(
                    kind,
                    &to,
                    &to,
                    system_now_ms(),
                    body,
                    &signing_key,
                );
                // rate-limited 有限重试：pdsync-* 已入应答侧限流豁免名单，
                // 但对端可能是未升级的旧版本（仍按 1s 窗口限流连发信封）——
                // 隔 1.2s（略大于限流窗口）重试至多 2 次；其余失败维持静默
                // （反熵是周期性的，本轮丢失下一轮补齐，不无限放大）。
                let mut retries = 0;
                loop {
                    let response = node.dm_direct(&target, envelope.clone()).await;
                    let rate_limited =
                        matches!(&response, Ok(v) if dm_response_is_rate_limited(v));
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
        });
    }
}

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

impl DmHandler for KernelDmHandler {
    /// dm 直连接收：委托 kernel 入站编排（验签/落库/事件），应答帧回传
    /// 发送方；产出的事件逐个 emit 到壳层广播通道。
    fn handle_dm(
        &self,
        payload: Value,
        remote_peer_id: &str,
        online_peers: &HashSet<String>,
    ) -> std::result::Result<Value, String> {
        let kind = payload.get("kind").and_then(|v| v.as_str()).unwrap_or("?");
        log::info!(
            "[HOST] handle_dm ENTRY | kind={} remote_peer={}",
            kind,
            &remote_peer_id[..std::cmp::min(16, remote_peer_id.len())]
        );
        let root_id = self
            .current_root_id
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .ok_or_else(|| "no active identity".to_string())?;
        // 昵称为空时回退 rootId 前 8 位（与 kernel my_nickname 的出站口径一致）
        let nickname = {
            let shared = self
                .nickname_shared
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            if shared.trim().is_empty() {
                root_id.chars().take(8).collect()
            } else {
                shared
            }
        };
        let mut storage = self.storage.clone();
        // 收尾（§7.1）能力标记：验签通过且 from==本机 rootId 的 pdsync-* 信封
        // 才证明对端（该连接层 peerId 对应的设备）支持 pdsync——保活据此停止
        // 向该设备回退发旧快照。判定内部做完整验签，先于入站合入执行不影响
        // 正确性（验签不过不标记）。
        if let Some(peer) = Self::pdsync_capability_mark(&payload, &root_id, remote_peer_id) {
            self.kernel_pdsync_capable_mark(&peer);
        }
        // 入站落库整体在 io_lock 内执行（与 Tauri 命令线程的变更互斥）
        let result = {
            let _io = self.io_lock.lock().unwrap_or_else(|e| e.into_inner());
            let node_id = self.sync_node_id();
            super::super::inbound_dm::handle_inbound_dm(
                &mut storage,
                &root_id,
                &nickname,
                payload,
                remote_peer_id,
                online_peers,
                system_now_ms(),
                &node_id,
            )
            .map_err(|e| e.to_string())?
        };
        for event in result.events {
            // 无订阅者时忽略发送失败
            let _ = self.event_tx.send(event);
        }
        if let Some(auto_accept) = result.auto_accept {
            // 配对回发通讯录 + 会话元数据快照：新设备（QR 恢复）立即拿到
            // 联系人和会话外壳/置顶/免打扰/草稿
            self.spawn_contact_sync_reply(&root_id, auto_accept.target.clone());
            self.spawn_conv_sync_reply(&root_id, auto_accept.target.clone());
            self.spawn_auto_accept(&root_id, &nickname, auto_accept);
        }
        // 自设备 profile-sync 回发分两种：
        // 1. 配对握手（handle_self_friend_request，unconditional=true）：
        //    无条件回发——P2P 启动时的一次性广播可能早于自记录 peer 填入，
        //    配对是首次可靠的回发时机；
        // 2. LWW 裁决（handle_profile_sync，unconditional=false）：
        //    仅本机较新时回发（对端快照较旧/残缺时补齐，收敛后相等不再
        //    互发，无 ping-pong）。
        if let Some(reply) = result.profile_sync_reply {
            if reply.unconditional {
                self.spawn_profile_sync_reply(&root_id, reply.target);
            } else if let Some(self_profile) = result.self_profile {
                let local_is_newer = self.apply_self_profile(&root_id, &self_profile);
                if local_is_newer {
                    self.spawn_profile_sync_reply(&root_id, reply.target);
                }
            }
        } else if let Some(self_profile) = result.self_profile {
            // 无回发指令但有快照：仅做 LWW 应用（对端较新时更新本机身份文件）
            self.apply_self_profile(&root_id, &self_profile);
        }
        // pdsync 合入 profile:self：回写身份文件（仅解锁态）
        if result.profile_applied {
            self.apply_profile_from_sled(&root_id);
        }
        // 自设备 device-sync 握手：回发本机设备记录
        if let Some(target) = result.device_sync_reply {
            self.spawn_device_sync_reply(&root_id, target);
        }
        // pdsync 出站：把纯逻辑层构建好的 body 装配成完整信封回投连接层对端
        if !result.pdsync_out.is_empty() {
            let target = PeerNodeInfo {
                peer_id: Some(remote_peer_id.to_string()),
                addresses: Vec::new(),
            };
            self.spawn_pdsync_reply(&root_id, target, result.pdsync_out);
        }
        Ok(result.response)
    }
}

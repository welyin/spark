//! keepalive 组织保活周期任务（p2p-node.ts `maintainOrganizationNetwork`）：
//! 分阶段预算执行（org-sync-stall-fix §3.2，F6：S0 网关/地址发布 →
//! S1 自设备链路 → S3 orgsync-hello 恒执行，各阶段独立超时）。
//! （覆盖网维护已在 p2p 事件循环内完成；覆盖网成员不周期拨号，connection-policy
//! M5。M6：tick 内主动外联（网关拨号 / 失联恢复查询 / 管理员补副本）全删，
//! 彻底事件驱动。阶段四A P3：S2 legacy org-pull 反熵对账随出站停发删除——
//! 组织对账由 orgsync 反熵承接。）

use std::collections::HashSet;

use super::{
    ORG_ADDRESS_REPUBLISH_INTERVAL_MS, OrgSyncContext, SELF_HELLO_IMMEDIATE_MIN_INTERVAL_MS,
    SelfHelloState,
};
use crate::contact::ContactService;
use crate::org::OrganizationService;
use crate::org::gateway::{OrgMemberHint, org_members_dht_key};
use crate::p2p::constants::OVERLAY_TOPIC;
use crate::p2p::envelope::build_org_body;
use crate::p2p::node::LocalP2PNodeInfo;
use crate::p2p::peer_targets::PeerNodeInfo;
use crate::storage::{ScanOptions, StorageBackend};

impl OrgSyncContext {
    /// 本机个人域写入（含删除墓碑）后的即时 hello：不等 keepalive tick
    /// （最坏 ~60s），直接向当前已连接的自设备补发 pdsync-hello，对端回
    /// need 即拉走墓碑——删除传播从"分钟级"降到"秒级"。
    ///
    /// 防抖：最短间隔 1s；窗口内的再次触发登记一次尾随补发（覆盖批量
    /// 删除的尾巴）。
    ///
    /// M4 懒拨号接入：无已连接自设备时，**数据写入即拨一次**（resolve 自
    /// 设备 peer → `connect_peer` → 拨通后补发 hello）——替代被删除的
    /// keepalive tick 周期补拨。失败即沉默，下次写入再拨（connection-policy
    /// §5.2 / §9）。拨号是网络操作，不触碰 io_lock（无需存储 RMW 串行）。
    pub(crate) fn self_hello_now(&self) {
        enum Act {
            Send(String, Vec<String>),
            Dial(String, PeerNodeInfo),
            Schedule(i64),
            Skip,
        }
        let now = self.now();
        let act = {
            let mut st = self
                .self_hello_immediate
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if now - st.last_sent_ms >= SELF_HELLO_IMMEDIATE_MIN_INTERVAL_MS {
                st.last_sent_ms = now;
                st.trailing_pending = false;
                // 多设备连接集（事件泵断连剔除 + tick 连接侧维护）：逐台发
                let peers: Vec<String> = self
                    .self_device_links
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .iter()
                    .cloned()
                    .collect();
                match (self.root_id(), peers.is_empty()) {
                    // 有已连接自设备 → 直发 hello
                    (Some(root_id), false) => Act::Send(root_id, peers),
                    // 无已连接自设备 → 懒拨号一次（写入即重连时机）
                    (Some(root_id), true) => match self.resolve_self_device_peer() {
                        Some(peer) => Act::Dial(root_id, peer),
                        None => Act::Skip,
                    },
                    _ => Act::Skip,
                }
            } else if !st.trailing_pending {
                st.trailing_pending = true;
                Act::Schedule(st.last_sent_ms + SELF_HELLO_IMMEDIATE_MIN_INTERVAL_MS - now)
            } else {
                Act::Skip
            }
        };
        match act {
            Act::Send(root_id, peers) => {
                log::info!(
                    "[CT_SYNC] immediate hello after local write | devices={:?}",
                    peers
                );
                // 同步函数驱动 async 发送：spawn 到 runtime（丢弃 future
                // 不会执行——必须 spawn）
                let ctx = self.clone();
                tokio::spawn(async move {
                    for peer_id in peers {
                        ctx.send_pdsync_hello(&root_id, &peer_id, now).await;
                    }
                });
            }
            Act::Dial(root_id, peer) => {
                log::info!(
                    "[CT_SYNC] lazy dial self device after local write | peer={:?}",
                    peer.peer_id
                );
                let ctx = self.clone();
                tokio::spawn(async move {
                    // leaf 模式 §4 寻址序固化：清单缓存地址为空（DeviceRecord 仅
                    // peerId）时补一次性 kad 存在记录查询（client 模式），与登录
                    // M2 路径（p2p_ops::bootstrap_login_dials）同口径；未命中即
                    // 空地址，connect_peer 失败沉默。非 leaf 行为不变（桌面有
                    // overlay 邻居池/announce 沉淀兜底，不多发查询）。
                    let peer = if ctx.node.leaf_mode()
                        && peer.addresses.is_empty()
                        && let Some(pid) = peer.peer_id.as_deref()
                    {
                        let fresh = query_node_presence_addrs(&ctx.node, pid).await;
                        if fresh.is_empty() {
                            peer
                        } else {
                            PeerNodeInfo {
                                peer_id: Some(pid.to_string()),
                                addresses: fresh,
                            }
                        }
                    } else {
                        peer
                    };
                    // 拨通后补发 hello（connect_peer 返回即连接已建立，可直发）
                    if ctx.node.connect_peer(&peer).await.is_ok()
                        && let Some(pid) = peer.peer_id
                    {
                        ctx.send_pdsync_hello(&root_id, &pid, now).await;
                    }
                });
            }
            Act::Schedule(delay_ms) => {
                let ctx = self.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(delay_ms.max(0) as u64))
                        .await;
                    ctx.self_hello_now();
                });
            }
            Act::Skip => {}
        }
    }

    /// 解析自设备 peer（含地址，供懒拨号）。优先 FriendRecord.peers（配对
    /// 握手回填，带地址，多设备遍历择优），DeviceRecord 兜底（仅 peerId）。与
    /// [`Self::maintain_self_device_link`] 头部同一解析口径；本机 peerId 排除。
    fn resolve_self_device_peer(&self) -> Option<PeerNodeInfo> {
        let mut storage = self.storage.clone();
        let my_peer = self.node.peer_id().to_string();
        // 自 FriendRecord（rootId==自己 且带寻址）优先
        if let Some(p) = crate::contact::ContactService::get_friend(&mut storage, &self.root_id()?)
            .ok()
            .flatten()
            .and_then(|f| f.peers.into_iter().find(|p| !p.peer_id.is_empty()))
            && p.peer_id != my_peer
        {
            return Some(PeerNodeInfo {
                peer_id: Some(p.peer_id),
                addresses: p.addresses,
            });
        }
        // DeviceRecord 兜底（仅 peerId，已连接时 dm_direct 短路）
        crate::device::DeviceService::list(&storage)
            .ok()?
            .into_iter()
            .find(|r| !r.peer_id.trim().is_empty() && r.peer_id != my_peer)
            .map(|r| PeerNodeInfo {
                peer_id: Some(r.peer_id),
                addresses: Vec::new(),
            })
    }

    // ------------------------------------------------------------------
    // keepalive 组织保活（p2p-node.ts:379-445 `maintainOrganizationNetwork`）
    // ------------------------------------------------------------------

    /// 单个 keepalive tick 的组织层保活（p2p-node.ts:379-445
    /// `maintainOrganizationNetwork`），分阶段预算执行（org-sync-stall-fix
    /// §3.2，F6）：
    ///
    /// - **S0** 网关/地址发布（[`Self::refresh_gateway_providing`] +
    ///   [`Self::refresh_org_address_publishing`]，10s）；
    /// - **S1** 自设备链路状态机（断→连跳变 Resync、稳态 steady hello，10s）。
    ///   不做周期补拨（M4 懒拨号：写入触发 `self_hello_now` 才拨一次）——两端
    ///   错峰上线靠任一方写入触发懒拨号会合，或对端主动拨过来；
    /// - **S3** orgsync-hello（向已连接复制组成员发摘要，O2a §20.3，10s）——
    ///   tick 的出口语义，**恒执行**：前序阶段超时放弃不得跳过。
    ///
    /// 阶段四A P3：S2（legacy org-pull 反熵对账）随出站停发删除——组织对账
    /// 由 orgsync 反熵（S3 + 写入触发的即时 hello）承接；`reconcile` 预算
    /// 字段同删。
    ///
    /// 各阶段包 `tokio::time::timeout`，超时即放弃本阶段进入下一阶段（阶段间
    /// 无依赖）；总预算 ≤30s（外加 S1 前一次 5s 超时兜底的
    /// `local_node_info` 读取），约小于生产 keepalive 间隔（60s）。
    pub(crate) async fn maintain_org_tick(&self) {
        let Some(root_id) = self.root_id() else {
            return;
        };
        let budgets = self.tick_budgets;

        // S0) 网关/地址发布：本机是某组织活跃网关 → 私有 DHT key 提供成员提示
        //     （§15）；公开组织持钥节点新签/重发地址记录（§16）。均幂等、失败
        //     静默（dht off 时下轮重试）。
        self.run_tick_stage("S0", budgets.gateway_publish, async {
            self.refresh_gateway_providing(&root_id).await;
            self.refresh_org_address_publishing(&root_id).await;
        })
        .await;

        // 本机节点信息一次取用：连接快照供 S3 使用（API 层 5s 超时兜底，
        // org-sync-stall-fix §3.3）
        let local_info = self.node.local_node_info().await.ok();

        // S1) 自设备链路状态机
        self.run_tick_stage(
            "S1",
            budgets.self_device_link,
            self.maintain_self_device_link(&root_id, local_info.as_ref()),
        )
        .await;

        let connected: HashSet<String> = local_info
            .map(|info| info.connected_peers.into_iter().collect())
            .unwrap_or_default();

        // S3) orgsync-hello 触发：本机作为复制组成员，向已连接的复制组成员
        //     发送 orgsync-hello 摘要（O2a §20.3）。tick 出口语义，恒执行。
        //     TODO: 完整接线——从 VersionedStorage 变更信号（last_local_write_ms）
        //     驱动即时 hello + 1s 防抖；当前最小闭环：每 tick 遍历组织与集合，
        //     向已连接复制组成员发送 hello。后续需接入 orgd 写变更 watcher。
        //     （P2 起组织写入路径已挂即时 hello，本 tick 为周期兜底。）
        self.run_tick_stage(
            "S3",
            budgets.orgsync_hello,
            self.maybe_send_orgsync_hello(&root_id, &connected, None),
        )
        .await;

        // S4) affairsync-hello 触发（affair-sync §7）：本机作为关注者，向各
        //     已关注事务目录中已连接的关注者发送摘要。周期兜底——连接建立
        //     后的收敛由本阶段覆盖（新连上的关注者下轮 tick 即被 hello 命中）；
        //     关注/本地事务写入的即时触发走 AffairHello 请求（affair_ops）。
        self.run_tick_stage(
            "S4",
            budgets.affairsync_hello,
            self.maybe_send_affairsync_hello(&root_id, &connected, None),
        )
        .await;

        // 阶段四E（设计 §2.6）：orgsync hello 节奏后拉一轮跨组织邮箱
        //（复用既有 tick 触发点，不新增周期任务；未解锁/未启动时函数内
        // 静默无操作）。
        // 评审修正：spawn 分离而非内联 await——拉取是 组织×网关×端点×两轮
        // 的出站网络链（每请求 15s 超时），内联会无界拖长 tick、把 PushOrg/
        // SelfHelloNow 事件语义请求压在串行 worker 队列后（F6 纪律：事件
        // 不合并，长链不入队尾）。
        let seed = self
            .seed_shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(seed) = seed {
            let storage = self.storage.clone();
            let node = std::sync::Arc::clone(&self.node);
            let root_id = root_id.clone();
            tokio::spawn(async move {
                let _ =
                    crate::kernel::org_mail_ops::org_mail_fetch_async(storage, node, seed, root_id)
                        .await;
            });
        }
    }

    /// tick 单阶段执行器（org-sync-stall-fix §3.2/§3.4）：包 `timeout`，超时
    /// 即放弃本阶段返回（阶段间无依赖，后续阶段照常）；进入/完成 DEBUG，完成
    /// 耗时 >5s 升 WARN，超时 WARN 明示放弃——与「静默缓慢」区分。
    async fn run_tick_stage(
        &self,
        stage: &'static str,
        budget: std::time::Duration,
        fut: impl std::future::Future<Output = ()>,
    ) {
        log::debug!("[ORG_SYNC] tick stage enter | stage={stage}");
        let started = std::time::Instant::now();
        if tokio::time::timeout(budget, fut).await.is_err() {
            log::warn!(
                "[ORG_SYNC] tick stage budget exceeded, skipped | stage={stage} budget={}ms",
                budget.as_millis()
            );
            return;
        }
        let elapsed = started.elapsed();
        if elapsed > std::time::Duration::from_secs(5) {
            log::warn!(
                "[ORG_SYNC] tick stage slow | stage={stage} elapsed={}ms",
                elapsed.as_millis()
            );
        } else {
            log::debug!(
                "[ORG_SYNC] tick stage done | stage={stage} elapsed={}ms",
                elapsed.as_millis()
            );
        }
    }

    /// 自设备链路维护（connection-policy M4 懒拨号后）：**不再周期补拨**。
    /// 断→连跳变时向对端重发 device-sync + profile-sync 快照；稳态由写入
    /// digest 驱动增量 hello。
    ///
    /// 重连时机全部改为**数据写入触发**：本机个人域写入 → `self_hello_now`
    /// 检查自设备连接，无则懒拨号一次（connect_peer）→ 拨通后补发 hello
    /// （§5.2 / §2.3 改造点）。此处 tick 只维护链路状态机（断→连跳变
    /// Resync、稳态 steady hello），不做周期拨号——两端错峰上线靠任一方
    /// 写入数据触发懒拨号会合，或对端主动拨过来。
    async fn maintain_self_device_link(
        &self,
        root_id: &str,
        local_info: Option<&LocalP2PNodeInfo>,
    ) {
        let mut storage = self.storage.clone();
        // 双来源解析配对设备：FriendRecord.peers 优先（带地址，多设备遍历），DeviceRecord
        // 兜底（仅 peerId）。懒拨号地址由写入触发路径 `resolve_self_device_peer`
        // 另行解析（带地址）；此处只需 peerId 判定连接状态。
        let mut peer_id = {
            let from_friend = ContactService::get_friend(&mut storage, root_id)
                .ok()
                .flatten()
                .and_then(|f| f.peers.into_iter().find(|p| !p.peer_id.is_empty()));
            match from_friend {
                Some(p) => p.peer_id,
                None => {
                    let my_peer = local_info.and_then(|i| i.peer_id.as_deref());
                    let device = crate::device::DeviceService::list(&storage)
                        .unwrap_or_default()
                        .into_iter()
                        .find(|r| Some(r.peer_id.as_str()) != my_peer);
                    match device {
                        Some(d) => d.peer_id,
                        None => return,
                    }
                }
            }
        };
        if peer_id.is_empty() {
            return;
        }
        // 自指拦截：自记录 peer 被污染指向本机（历史 pdsync 互灌残留）时先
        // 尝试自愈——按设备清单把记录改写回对端设备（成功则继续走保活/投递，
        // 该键对称排除于 pdsync 折叠/增量，自愈写不传播）；找不到对端设备
        // 记录时保持拦截——不自拨：DialError::LocalPeerId，resync 亦无意义
        if let Some(local) = local_info.and_then(|i| i.peer_id.as_deref())
            && local == peer_id.as_str()
        {
            let Some(healed) = crate::kernel::dm_delivery::heal_self_pointing_friend_record(
                &mut storage,
                root_id,
                local,
                local,
                self.now(),
            ) else {
                return;
            };
            if let Some(healed_peer) = healed.peer_id {
                peer_id = healed_peer;
            }
        }
        let connected = local_info
            .map(|info| info.connected_peers.iter().any(|p| p == &peer_id))
            .unwrap_or(false);

        // 状态机决策在独立作用域内完成（MutexGuard 不可跨 await）
        enum Action {
            StayConnected,
            Resync,
            // 未连接：不再周期补拨（M4 懒拨号）。等数据写入触发
            // `self_hello_now` 懒拨一次，或对端主动拨过来。此处仅维护
            // 连接集状态，供写入触发路径判断是否需拨。
            Idle,
        }
        let action = {
            let mut last = self
                .self_device_link
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if connected {
                if last.as_deref() == Some(peer_id.as_str()) {
                    Action::StayConnected
                } else {
                    // 断→连跳变（含启动后首次观察到连接）
                    *last = Some(peer_id.clone());
                    Action::Resync
                }
            } else {
                *last = None;
                Action::Idle
            }
        };
        // 维护共享的多设备连接集（即时 hello 等触发路径消费；事件泵在
        // Disconnected 时已剔除，这里是连接侧的补充/兜底）
        {
            let mut links = self
                .self_device_links
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if connected {
                links.insert(peer_id.clone());
            } else {
                links.remove(peer_id.as_str());
            }
        }
        match action {
            // 稳态：本机个人域写入 digest 变化（变更即触发）或到达周期兜底
            // 间隔时发 pdsync-hello——pdsync 此前只有断→连跳变才发 hello，
            // 稳态无增量同步机制（对端收 hello 按 diff 回 need/data 收敛）
            Action::StayConnected => self.maybe_send_steady_hello(root_id, &peer_id).await,
            // 重发快照补齐错过的变更。幂等（LWW 裁决），启动一次性广播可能
            // 造成的少量重复投递可接受。
            Action::Resync => {
                // 本次 Resync 已发 hello：重置稳态触发状态，下个 StayConnected
                // tick 以当前 digest 重建基线（避免连接跳变前的累积变更误触发）
                *self
                    .self_hello_state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = SelfHelloState::default();
                self.send_self_snapshots(root_id, &peer_id).await;
            }
            // 未连接：M4 懒拨号——周期补拨已删除，写入触发（self_hello_now）
            // 会懒拨一次；此处静默等待，不骚扰死设备。
            Action::Idle => {}
        }
    }

    /// 稳态（StayConnected）自设备 hello 触发：对本机各类目 folded vv 算
    /// 本机写入 digest，经 [`SelfHelloState::observe`] 判定——digest 变化
    /// （本机个人域写入，≤1 tick 传播）或到达周期兜底间隔时发 pdsync-hello。
    /// 远端合入不动本机 vv 分量，digest 不变，天然不触发（防回声）。
    async fn maybe_send_steady_hello(&self, root_id: &str, peer_id: &str) {
        let local_node_id = self.node.peer_id().to_string();
        let exclude = crate::sync::pdsync::self_friend_key(root_id);
        let digest = local_personal_write_digest(&self.storage, &local_node_id, Some(&exclude));
        let now = self.now();
        let send = self
            .self_hello_state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .observe(digest, now);
        if send {
            self.send_pdsync_hello(root_id, peer_id, now).await;
        }
    }

    /// 向已连接的自设备发 `pdsync-hello` 摘要（断→连跳变的 Resync 与稳态
    /// 增量/周期触发共用；失败静默）。自 FriendRecord 键对称排除（peer 为
    /// 设备相对值，不可互灌——双设备同账号排除键相同，folded vv 保持一致）。
    async fn send_pdsync_hello(&self, root_id: &str, peer_id: &str, now: i64) {
        let signing_key = self
            .signing_key
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let Some(signing_key) = signing_key else {
            return;
        };
        let last_sync = crate::sync::pdsync::get_last_msg_sync_at(&self.storage, root_id);
        let Ok(mut hello) = crate::sync::pdsync::build_hello(
            &self.storage,
            2_592_000_000,
            500,
            "eager",
            Some(&crate::sync::pdsync::self_friend_key(root_id)),
            last_sync,
        ) else {
            return;
        };
        // 删除日志回执：我已收讫对端日志的序号（对端据此停推/GC；
        // 按目标设备取——同身份多设备各自维护日志，ack 不可混用）
        let dlog_ack = crate::sync::dlog::get_seen(&self.storage, peer_id).unwrap_or(0);
        hello["dlogAck"] = serde_json::json!(dlog_ack);
        let target = PeerNodeInfo {
            peer_id: Some(peer_id.to_string()),
            addresses: Vec::new(), // 已连接：dm_direct 短路直发
        };
        let envelope = crate::kernel::dm_envelope::build_envelope(
            crate::kernel::dm_envelope::KIND_PDSYNC_HELLO,
            root_id,
            root_id,
            now,
            hello,
            &signing_key,
        );
        let _ = self.node.dm_direct(&target, envelope).await;
    }

    /// 向已会合的自设备重发本机数据（断→连跳变触发；失败静默）。
    ///
    /// 收尾（§7.1）：能力探测驱动。始终发 `pdsync-hello`；仅当对端设备
    /// （连接层 peerId）尚未证明支持 pdsync（从未回 need/data）时，才回退
    /// 补发旧 device/profile/contact/conv 快照。已证明支持的设备只走 pdsync
    /// 反熵，不再双发旧快照。入站旧 kind 处理保留（跨版本兼容，见 §7.1）。
    /// 能力集合按 peerId 键控：同身份多台设备共享 rootId，按 rootId 判定会
    /// 让一台新设备停掉所有自设备的旧快照回退。
    async fn send_self_snapshots(&self, root_id: &str, peer_id: &str) {
        let signing_key = self
            .signing_key
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let Some(signing_key) = signing_key else {
            return;
        };
        let my_peer_id = self.node.peer_id().to_string();
        let target = PeerNodeInfo {
            peer_id: Some(peer_id.to_string()),
            addresses: Vec::new(), // 已连接：dm_direct 短路直发
        };
        let now = self.now();
        // 对端设备是否已证明支持 pdsync（host `handle_dm` 验签通过后按其
        // 连接层 peerId 置位）
        let pdsync_capable = self
            .pdsync_capable_self_devices
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(peer_id);
        // 0) pdsync-hello：摘要交换（与稳态触发共用同一发送路径）。对端支持
        //    则回 need/data 触发收敛；不支持则静默（由下方旧快照回退兜底）。
        self.send_pdsync_hello(root_id, peer_id, now).await;
        // 收尾（§7.1）：对端未证明支持 pdsync 时才回退补发旧快照；已支持
        // 的设备只走 pdsync 反熵（不再双发旧通道，避免冗余）。
        if !pdsync_capable {
            // 1) device-sync：本机设备记录（读取既有记录，不 upsert——避免每
            //    tick 刷新 updatedAt 推高 LWW 水位）
            if let Ok(Some(record)) = crate::device::DeviceService::get(&self.storage, &my_peer_id)
            {
                if let Ok(body) = serde_json::to_value(&record) {
                    let envelope = crate::kernel::dm_envelope::build_envelope(
                        crate::kernel::dm_envelope::KIND_DEVICE_SYNC,
                        root_id,
                        root_id,
                        now,
                        body,
                        &signing_key,
                    );
                    let _ = self.node.dm_direct(&target, envelope).await;
                }
            }
            // 2) profile-sync：身份文件全量资料快照（昵称为空回退 rootId 前 8 位）
            let path = self
                .data_dir
                .join("identities")
                .join(format!("{root_id}.json"));
            if let Ok(raw) = std::fs::read_to_string(&path) {
                if let Ok(file) = crate::identity::IdentityFile::from_json(&raw) {
                    let nickname = file.nickname.clone().unwrap_or_default();
                    let nickname = if nickname.trim().is_empty() {
                        root_id.chars().take(8).collect::<String>()
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
                    let envelope = crate::kernel::dm_envelope::build_envelope(
                        crate::kernel::dm_envelope::KIND_PROFILE_SYNC,
                        root_id,
                        root_id,
                        now,
                        body,
                        &signing_key,
                    );
                    let _ = self.node.dm_direct(&target, envelope).await;
                }
            }
            // 3) contact-sync：通讯录全量快照（朋友/申请/标签/分组/拉黑；
            //    LWW 幂等，对端按时间戳裁决，重复投递无害）
            if let Ok(body) = crate::contact::build_contact_sync_snapshot(&self.storage, root_id) {
                let envelope = crate::kernel::dm_envelope::build_envelope(
                    crate::kernel::dm_envelope::KIND_CONTACT_SYNC,
                    root_id,
                    root_id,
                    now,
                    body,
                    &signing_key,
                );
                let _ = self.node.dm_direct(&target, envelope).await;
            }
            // 4) conv-sync：会话元数据快照（direct 会话外壳 + 置顶/免打扰/草稿）
            if let Ok(body) = crate::message::build_conv_sync_snapshot(&self.storage) {
                let envelope = crate::kernel::dm_envelope::build_envelope(
                    crate::kernel::dm_envelope::KIND_CONV_SYNC,
                    root_id,
                    root_id,
                    now,
                    body,
                    &signing_key,
                );
                let _ = self.node.dm_direct(&target, envelope).await;
            }
        }
    }

    /// 网关职责检测（org.md §14 + O1 账号角色模型 + network §4.2）：本机账号
    /// 是**活跃网关**（全员候选计分推导活跃集，见
    /// [`crate::org::roles::is_gateway_active`]）且持有 orgSecret → 在
    /// `org_members_dht_key` 上 start_providing + 发布成员提示记录。
    /// 节点侧幂等去重，每 tick 调用一次即可；周期重发由节点挂 tick 计数完成。
    async fn refresh_gateway_providing(&self, root_id: &str) {
        let records =
            OrganizationService::read_all_organizations(&self.storage).unwrap_or_default();
        let now = self.now();
        let keys: Vec<String> = records
            .iter()
            .filter(|record| crate::org::roles::is_gateway_active(&self.storage, record, root_id, now))
            .filter_map(|record| record.org_secret().map(org_members_dht_key))
            .collect();
        if keys.is_empty() {
            return;
        }
        let Ok(info) = self.node.local_node_info().await else {
            return;
        };
        let Some(peer_id) = info.peer_id else {
            return;
        };
        if info.addresses.is_empty() {
            return;
        }
        let value = OrgMemberHint {
            peer_id,
            addresses: info.addresses,
        }
        .to_record_value();
        for key in keys {
            // 相同 (key, value) 节点侧幂等空操作；dht off 等失败静默（下轮重试）
            let _ = self
                .node
                .dht_provide_record(key.as_bytes(), value.clone())
                .await;
        }
    }

    /// 公开组织的地址记录发布（org.md §16 + p2p-messages.md §16），与
    /// [`Self::refresh_gateway_providing`] 同落点：
    ///
    /// - **持钥节点**（本机为成员且 extra 中有可解密的 `orgRootSecret`）：无缓存
    ///   记录 / 展示名或履职集变更 / 到达重发间隔时，以 `seq+1` 新签记录 →
    ///   `dht_put_record`（key = sha256(orgPublicKey) 字节，TTL 8h）→
    ///   gossip 扩散（spark-overlay 信封 `type='org-address'`）→ 沉淀本地缓存
    /// - **非持钥网关**（本机在履职集但不持根私钥）：按同一间隔重发
    ///   缓存中仍有效的记录（不重签、不 gossip）
    /// - 展示名取 `orgDisplayName` 覆盖，缺省用组织名；全部失败静默（下轮重试）
    async fn refresh_org_address_publishing(&self, root_id: &str) {
        use crate::org::org_address as oa;

        let records =
            OrganizationService::read_all_organizations(&self.storage).unwrap_or_default();
        let now = self.now();
        for record in records {
            if !record.is_public || record.find_member(root_id).is_none() {
                continue;
            }
            let Some(org_address) = record.org_address.clone() else {
                continue;
            };
            let signing = oa::org_root_signing_key(&record);
            // O1 + A9：活跃网关判定（全员候选计分推导活跃集）
            let is_gateway = crate::org::roles::is_gateway_active(&self.storage, &record, root_id, now);
            if signing.is_none() && !is_gateway {
                continue;
            }
            let Some(dht_key) = oa::org_address_dht_key(&org_address) else {
                continue;
            };
            let display_name = record
                .display_name_override()
                .map(str::to_string)
                .or_else(|| Some(record.name.clone()))
                .filter(|name| !name.trim().is_empty());
            let last = self
                .org_address_publish
                .lock()
                .unwrap()
                .get(&org_address)
                .copied()
                .unwrap_or(0);
            let due = now - last >= ORG_ADDRESS_REPUBLISH_INTERVAL_MS;
            let cached = oa::read_cached_org_address_record(&self.storage, &org_address);

            if let Some(signing_key) = signing {
                // A9（network §4.2）：地址记录 gateways = 当前计分推导履职集
                // （线形不变、语义从「指定名单」转为「履职集快照」）——发送方
                // 据地址记录解析的是目标组织当下实际履职的网关
                let active_gateways =
                    crate::org::roles::gateway_active_set(&self.storage, &record, Some(root_id), now);
                let changed = cached.as_ref().is_none_or(|c| {
                    c.gateways != active_gateways || c.display_name != display_name
                });
                if cached.is_none() || changed || due {
                    let seq = cached.as_ref().map(|c| c.seq).unwrap_or(0) + 1;
                    let signed = oa::sign_org_address_record(
                        &signing_key,
                        &record.org_id,
                        display_name,
                        active_gateways,
                        seq,
                        now,
                        oa::ORG_ADDRESS_RECORD_DEFAULT_TTL_MS,
                    );
                    // DHT put（dht off 静默失败；失败不写时间戳，下轮重试）
                    let published = self
                        .node
                        .dht_put_record(&dht_key, signed.to_record_value())
                        .await
                        .is_ok();
                    // gossip 扩散（仅新签记录，天然低频）
                    if let Ok(value) = serde_json::to_value(&signed) {
                        let body = build_org_body(oa::ORG_ADDRESS_GOSSIP_TYPE, value);
                        let _ = self.node.broadcast(OVERLAY_TOPIC, body).await;
                    }
                    let mut storage = self.storage.clone();
                    if let Err(e) = oa::cache_org_address_record(&mut storage, &signed) {
                        self.warn(format!("org address cache save failed: {e}"));
                    }
                    if published {
                        self.org_address_publish
                            .lock()
                            .unwrap()
                            .insert(org_address, now);
                    }
                }
            } else if due {
                // 非持钥网关：重发缓存中仍有效的记录（记录自身 ttl 内的副本才值得重发）
                if let Some(cached) = cached.filter(|c| !oa::org_address_record_expired(c, now)) {
                    // 与持钥分支同口径：发布失败不写时间戳，下轮重试
                    let published = self
                        .node
                        .dht_put_record(&dht_key, cached.to_record_value())
                        .await
                        .is_ok();
                    if published {
                        self.org_address_publish
                            .lock()
                            .unwrap()
                            .insert(org_address, now);
                    }
                }
            }
        }
    }
}

/// 本机个人域写入 digest：各类目全部记录 pmeta 中本机 nodeId 分量**逐记录
/// 求和**。
///
/// 不能用 `collect_category_vv` 的折叠值——折叠按分量取 max，同类目多条
/// 记录的本机分量互相遮蔽（写一条新记录时 folded 本机分量不变，变更漏检）。
/// 逐记录求和后，本机写入（`put_personal` / `bump_personal_meta` /
/// `delete_personal` 家族）恒使被写记录的本机分量 +1，总和严格递增——任何
/// 本机个人域写入都被察觉。远端合入（`apply_personal_remote` 家族）落的是
/// 远端 pmeta：本机分量只有本机写入能推高，远端携带的本机分量不会高于本机
/// 已有值（并发 LWW 远端胜出的极端情形可能替换为更旧值，至多带来一次多余
/// hello——对端 diff 为 Equal 即静默收敛，无回声循环）。
/// 单类目扫描失败按 0 计（下次 tick 重判）。
fn local_personal_write_digest<S: StorageBackend>(
    storage: &S,
    local_node_id: &str,
    exclude_key: Option<&str>,
) -> i64 {
    let mut sum = 0i64;
    for category in crate::sync::pdsync::CATEGORIES {
        for prefix in category.prefixes {
            let meta_prefix = crate::sync::personal::personal_meta_key(prefix);
            let Ok(rows) = storage.scan(&ScanOptions::prefix(&meta_prefix)) else {
                continue;
            };
            for (meta_key, raw) in rows {
                // 与 collect_category_vv 同口径：剥离 pmeta 前缀后必须仍命中
                // category 前缀；排除键（自记录）不参与
                let Some(record_key) = meta_key.strip_prefix(crate::sync::personal::PMETA_PREFIX)
                else {
                    continue;
                };
                if !category.prefixes.iter().any(|p| record_key.starts_with(p)) {
                    continue;
                }
                if exclude_key == Some(record_key) {
                    continue;
                }
                if let Ok(meta) = serde_json::from_str::<crate::sync::meta::DocMeta>(&raw) {
                    sum = sum.wrapping_add(meta.vv.get(local_node_id).copied().unwrap_or(0));
                }
            }
        }
    }
    sum
}

/// 一次性 kad 节点存在记录查询（leaf 模式 §4 寻址序的 DHT 腿；与
/// `kernel/p2p_ops.rs::query_node_presence_async` 同口径）：验签通过且 peerId
/// 匹配返回新鲜地址，未命中/验签失败返回空（调用方按原候选继续，失败沉默）。
async fn query_node_presence_addrs(node: &crate::p2p::P2pNode, peer_id: &str) -> Vec<String> {
    let key = crate::p2p::announce::node_presence_record_key(peer_id);
    let Ok(Some(raw)) = node.dht_get_record(key.as_bytes()).await else {
        return Vec::new();
    };
    let Ok(text) = String::from_utf8(raw) else {
        return Vec::new();
    };
    let Some(announce) = crate::p2p::announce::verify_announce_text(&text) else {
        return Vec::new();
    };
    if announce.peer_id != peer_id {
        return Vec::new();
    }
    announce.addresses
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::MemoryStorage;
    use crate::sync::meta::DocMeta;
    use crate::sync::personal::{apply_personal_remote, put_personal};

    fn remote_meta(node: &str, counter: i64, ts: i64) -> DocMeta {
        DocMeta {
            vv: [(node.to_string(), counter)].into_iter().collect(),
            ts,
            node_id: Some(node.to_string()),
            ..Default::default()
        }
    }

    /// 变更即触发 + 防回声：本机写入（put_personal 家族）使 digest 递增；
    /// 远端合入（apply_personal_remote）只写远端 vv 分量，digest 不变——
    /// 远端合入的写入不会触发稳态 hello，无回声循环。
    #[test]
    fn digest_tracks_local_writes_only() {
        let mut s = MemoryStorage::new();
        let d0 = local_personal_write_digest(&s, "node-a", None);
        assert_eq!(d0, 0);

        // 本机写入 → digest 变化
        put_personal(&mut s, "node-a", "ct:friend:x", "\"v1\"", 1000).unwrap();
        let d1 = local_personal_write_digest(&s, "node-a", None);
        assert!(d1 > d0, "本机写入必须反映到 digest");

        // 远端合入（新 key，直接采纳）→ digest 不变
        apply_personal_remote(
            &mut s,
            "ct:friend:y",
            "\"v2\"",
            &remote_meta("node-b", 1, 2000),
        )
        .unwrap();
        assert_eq!(
            local_personal_write_digest(&s, "node-a", None),
            d1,
            "远端合入不得触发"
        );

        // 远端合入覆盖本机已有 key（远端 vv 领先）→ digest 仍不变
        let mut meta = remote_meta("node-b", 2, 3000);
        meta.vv.insert("node-a".to_string(), 1);
        apply_personal_remote(&mut s, "ct:friend:x", "\"v3\"", &meta).unwrap();
        assert_eq!(
            local_personal_write_digest(&s, "node-a", None),
            d1,
            "远端胜出覆盖也不计入本机分量"
        );

        // 本机再写（含折叠排除键语义：自记录排除后不参与 digest）→ digest 递增
        put_personal(&mut s, "node-a", "ct:friend:z", "\"v4\"", 4000).unwrap();
        let d2 = local_personal_write_digest(&s, "node-a", None);
        assert!(d2 > d1);
        let excluded = local_personal_write_digest(&s, "node-a", Some("ct:friend:z"));
        assert_eq!(excluded, d1, "排除键的记录不参与 digest");
    }

    /// 快照合入对端记账 → 本机 digest 不变（风暴抑制）：旧通道（contact-sync /
    /// conv-sync / device-sync）合入时记账到对端设备（remote_peer_id）而非本机，
    /// 本机分量不被推进 → steady hello 不触发 → 消除双向回环风暴。
    /// 对照：若误用本机 node 记账（旧 bug），本机 digest 被推进 → 触发风暴。
    #[test]
    fn snapshot_remote_accounting_does_not_bump_local_digest() {
        let mut s = MemoryStorage::new();
        let d0 = local_personal_write_digest(&s, "node-a", None);
        assert_eq!(d0, 0);

        // 对端记账（remote_peer_id 记账）→ 本机 node-a digest 不变
        put_personal(&mut s, "node-b", "ct:friend:x", "\"v1\"", 1000).unwrap();
        put_personal(&mut s, "node-b", "ct:tag:t1", "\"v2\"", 1001).unwrap();
        assert_eq!(
            local_personal_write_digest(&s, "node-a", None),
            d0,
            "对端记账不得推进本机分量（防回声）"
        );

        // 数据仍可见：记录 pmeta 带对端分量，category vv 折叠含对端 → 对端补推收敛
        let meta = crate::sync::personal::get_personal_meta(&s, "ct:friend:x")
            .unwrap()
            .expect("对端记账的合入仍应落 pmeta");
        assert_eq!(meta.vv.get("node-b"), Some(&1), "账记到对端设备分量");
        assert_eq!(meta.vv.get("node-a"), None, "本机分量不被推进");

        // 对照（旧 bug 行为）：误用本机记账 → digest 被推进 → 风暴前兆
        put_personal(&mut s, "node-a", "ct:friend:z", "\"v3\"", 1002).unwrap();
        assert!(
            local_personal_write_digest(&s, "node-a", None) > d0,
            "误用本机记账会推进 digest（触发回环，须避免）"
        );
    }

    /// 稳态 hello 判定：首次观察只建基线（不发）；本机写入 digest 变化即
    /// 触发；未变未到点不发；到达周期兜底间隔发；发送后重新计时。
    #[test]
    fn steady_hello_observe_decisions() {
        let mut st = SelfHelloState::default();
        assert!(!st.observe(7, 1_000), "首次观察只建基线，不发");
        assert!(!st.observe(7, 2_000), "digest 未变且未到点，不发");
        assert!(st.observe(8, 3_000), "本机写入 digest 变化，变更即触发");
        assert!(!st.observe(8, 4_000), "发送后未再变，不发");
        assert!(
            st.observe(8, 4_000 + super::super::SELF_DEVICE_HELLO_INTERVAL_MS),
            "到达周期兜底间隔，发"
        );
        assert!(
            !st.observe(8, 4_000 + super::super::SELF_DEVICE_HELLO_INTERVAL_MS + 1),
            "发送后重新计时，不连发"
        );
    }
}

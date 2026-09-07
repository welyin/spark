//! affairsync-hello 出站触发（affair-sync §7）：本机作为关注者，向事务关注者
//! 目录（`affairsync:dir:` 覆盖网线索）中**已连接**的关注者发送
//! affairsync-hello 摘要（折叠 vv + DAG 头集合）。
//!
//! 与 orgsync_hello 的对照（affair-sync §8）：
//! - 复制组 = 动态关注者集合：收件人从关注者目录取（复制流量中学的线索，
//!   dir.rs），而非成员表角色推导；
//! - 无 dlogAck/roles/degraded：affair 域纯 append-only，无墓碑面与数据
//!   账号角色面；
//! - 触发点：即时（关注/本地事务写入，[`OrgSyncRequest::AffairHello`]）+
//!   tick S4 周期兜底（连接建立后的收敛由 tick 覆盖，对齐 orgsync S3 的
//!   「M8：hello 不拨号」口径——未连接关注者一律跳过，等其上线或对方
//!   主动 hello）。

use serde_json::Value;

use super::OrgSyncContext;
use crate::p2p::peer_targets::PeerNodeInfo;
use crate::storage::StorageBackend;

impl OrgSyncContext {
    /// affair 域即时 hello（AffairHello 请求的处理体）：取连接快照后向该事务
    /// （或全部已关注事务）的已连接关注者发 affairsync-hello。
    pub(crate) async fn affairsync_hello_now(&self, affair_id: Option<&str>) {
        let Some(root_id) = self.root_id() else {
            return;
        };
        let connected: std::collections::HashSet<String> = self
            .node
            .local_node_info()
            .await
            .ok()
            .map(|info| info.connected_peers.into_iter().collect())
            .unwrap_or_default();
        if connected.is_empty() {
            return;
        }
        self.maybe_send_affairsync_hello(&root_id, &connected, affair_id)
            .await;
    }

    /// affairsync-hello 触发：遍历本机已关注事务（`only_affair` 收窄），向
    /// 关注者目录中已连接的关注者逐个发送 hello。hello 是幂等摘要交换：
    /// 对端按 diff 回 need / 推 data（inbound_dm/affairsync.rs），重复发送
    /// 收敛到 Equal 静默。
    pub(crate) async fn maybe_send_affairsync_hello(
        &self,
        root_id: &str,
        connected: &std::collections::HashSet<String>,
        only_affair: Option<&str>,
    ) {
        let signing_key = self
            .signing_key
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let Some(signing_key) = signing_key else {
            return;
        };
        let now = self.now();
        let device_class = crate::sync::pdsync::local_device_class();
        let mut affairs =
            crate::sync::affairsync::list_followed_affairs(&self.storage).unwrap_or_default();
        if let Some(only) = only_affair {
            affairs.retain(|a| a == only);
        }
        for affair_id in affairs {
            let Ok(vv) = crate::sync::affairsync::collect_affair_vv(&self.storage, &affair_id)
            else {
                continue;
            };
            let heads = read_affair_heads(&self.storage, &affair_id);
            let Ok(hints) = crate::sync::affairsync::follower_hints(&self.storage, &affair_id)
            else {
                continue;
            };
            for hint in hints {
                // 目录条目是线索不是名册：跳过自条目与无寻址线索的条目
                if hint.root_id == root_id {
                    continue;
                }
                let Some(peer_id) = hint.peer_id else {
                    continue;
                };
                // M8 同口径：hello 不拨号，只发已连接关注者；未连接者等其
                // 上线后的 tick / 对方主动 hello 收敛
                if !connected.contains(peer_id.as_str()) {
                    continue;
                }
                let body = crate::sync::affairsync::build_affairsync_hello(
                    &affair_id,
                    &vv,
                    &heads,
                    &device_class,
                );
                let target = PeerNodeInfo {
                    peer_id: Some(peer_id.clone()),
                    addresses: Vec::new(), // 已连接：dm_direct 短路直发
                };
                let envelope = crate::kernel::dm_envelope::build_envelope(
                    crate::kernel::dm_envelope::KIND_AFFAIRSYNC_HELLO,
                    root_id,
                    &hint.root_id,
                    now,
                    body,
                    &signing_key,
                );
                let delivered = self.node.dm_direct(&target, envelope).await.is_ok();
                log::info!(
                    "[AFFAIRSYNC] hello sent | affair={} to={} delivered={}",
                    &affair_id[..std::cmp::min(16, affair_id.len())],
                    peer_id,
                    delivered
                );
            }
        }
    }
}

/// 读事务的 DAG 头集合（`affair:head:{affairId}`；缺失/损坏按空集）。
fn read_affair_heads<S: StorageBackend>(storage: &S, affair_id: &str) -> Vec<String> {
    storage
        .get(&crate::affair::affair_head_key(affair_id))
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .and_then(|value| {
            value.get("heads").and_then(Value::as_array).map(|arr| {
                arr.iter()
                    .filter_map(Value::as_str)
                    .map(String::from)
                    .collect::<Vec<_>>()
            })
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::MemoryStorage;

    /// DAG 头读取：缺失 → 空集；正常落库形态 → 集合内容；损坏记录 → 空集。
    #[test]
    fn read_heads_tolerates_missing_and_corrupt() {
        let mut storage = MemoryStorage::default();
        let affair_id = "ab".repeat(32);
        assert!(read_affair_heads(&storage, &affair_id).is_empty());

        storage
            .put(
                &crate::affair::affair_head_key(&affair_id),
                &serde_json::json!({ "heads": ["h1", "h2"] }).to_string(),
            )
            .unwrap();
        assert_eq!(read_affair_heads(&storage, &affair_id), vec!["h1", "h2"]);

        storage
            .put(&crate::affair::affair_head_key(&affair_id), "not-json")
            .unwrap();
        assert!(read_affair_heads(&storage, &affair_id).is_empty());
    }
}

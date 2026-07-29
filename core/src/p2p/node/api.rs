//! 命令通道：`Command` 枚举与 `P2pNode` 的对外异步命令方法。
//!
//! 每个公开方法把请求包装成 `Command` 发给事件循环，经 oneshot 等待结果；
//! 事件循环退出时 send/recv 失败统一映射为 `P2pError::NotStarted`。

use std::time::Duration;

use serde_json::{Map, Value};
use tokio::sync::oneshot;

use crate::p2p::constants::DM_READ_TIMEOUT_MS;
use crate::p2p::peer_targets::PeerNodeInfo;
use crate::p2p::{P2pError, Result};

use super::{KeepaliveStats, LocalP2PNodeInfo, P2pNode};

pub(super) enum Command {
    Broadcast {
        topic: String,
        body: Map<String, Value>,
        tx: oneshot::Sender<Result<()>>,
    },
    AnnounceNow {
        tx: oneshot::Sender<Result<bool>>,
    },
    ConnectPeer {
        node_info: PeerNodeInfo,
        tx: oneshot::Sender<Result<()>>,
    },
    ExchangeWithPeer {
        peer_id: String,
        tx: oneshot::Sender<Result<usize>>,
    },
    QueryRecovery {
        token: String,
        neighbors: Vec<String>,
        want: usize,
        tx: oneshot::Sender<Result<Vec<PeerNodeInfo>>>,
    },
    OrgShareDirect {
        node_info: PeerNodeInfo,
        payload: Value,
        tx: oneshot::Sender<Result<bool>>,
    },
    OrgPullRequest {
        node_info: PeerNodeInfo,
        request_json: String,
        tx: oneshot::Sender<Result<Option<Value>>>,
    },
    DmDirect {
        node_info: PeerNodeInfo,
        payload: Value,
        tx: oneshot::Sender<Result<Option<Value>>>,
    },
    LocalNodeInfo {
        tx: oneshot::Sender<LocalP2PNodeInfo>,
    },
    /// 向公共 DHT 发布记录（原始 key/value 字节）。
    DhtPutRecord {
        key: Vec<u8>,
        value: Vec<u8>,
        tx: oneshot::Sender<Result<()>>,
    },
    /// 从公共 DHT 查询记录；未命中 Ok(None)。
    DhtGetRecord {
        key: Vec<u8>,
        tx: oneshot::Sender<Result<Option<Vec<u8>>>>,
    },
    /// 在 key 上声明为 provider 并发布一条记录（组织私有 DHT 网关职责；
    /// 随后挂 keepalive tick 周期重发）。
    DhtProvide {
        key: Vec<u8>,
        value: Vec<u8>,
        tx: oneshot::Sender<Result<()>>,
    },
    /// 查询某 key 的 provider 集合（peerId 字符串列表）。
    DhtGetProviders {
        key: Vec<u8>,
        tx: oneshot::Sender<Result<Vec<String>>>,
    },
    /// 向已连接对端发起 node-challenge 身份确认。
    ChallengePeer {
        peer_id: String,
        tx: oneshot::Sender<Result<bool>>,
    },
    Tick {
        tx: oneshot::Sender<KeepaliveStats>,
    },
    Shutdown,
}

impl P2pNode {
    pub(super) fn send_cmd(&self, cmd: Command) -> Result<()> {
        self.cmd_tx.send(cmd).map_err(|_| P2pError::NotStarted)
    }

    /// 广播业务消息：自动填充 version/evidenceHeadHash/timestamp 并签名。
    pub async fn broadcast(&self, topic: &str, body: Map<String, Value>) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.send_cmd(Command::Broadcast {
            topic: topic.to_string(),
            body,
            tx,
        })?;
        rx.await.map_err(|_| P2pError::NotStarted)?
    }

    /// 立即发布一次 node-announce（地址变化补发之外的主动触发）。
    pub async fn announce_now(&self) -> Result<bool> {
        let (tx, rx) = oneshot::channel();
        self.send_cmd(Command::AnnounceNow { tx })?;
        rx.await.map_err(|_| P2pError::NotStarted)?
    }

    /// 按候选地址列表拨号连接目标成员（10s 超时）。
    pub async fn connect_peer(&self, node_info: &PeerNodeInfo) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.send_cmd(Command::ConnectPeer {
            node_info: node_info.clone(),
            tx,
        })?;
        tokio::time::timeout(Duration::from_secs(10), rx)
            .await
            .map_err(|_| P2pError::Dial("connect timeout".to_string()))?
            .map_err(|_| P2pError::NotStarted)?
    }

    /// 向一个已连接邻居发起 peer-exchange，返回合并条目数。
    pub async fn exchange_with_peer(&self, peer_id: &str) -> Result<usize> {
        let (tx, rx) = oneshot::channel();
        self.send_cmd(Command::ExchangeWithPeer {
            peer_id: peer_id.to_string(),
            tx,
        })?;
        tokio::time::timeout(Duration::from_secs(10), rx)
            .await
            .map_err(|_| P2pError::Protocol("exchange timeout".to_string()))?
            .map_err(|_| P2pError::NotStarted)?
    }

    /// 向一组已连接邻居发出 org-recovery 查询（ttl=2、去重合并、截断 16）。
    pub async fn query_recovery(
        &self,
        token: &str,
        neighbors: Vec<String>,
        want: usize,
    ) -> Result<Vec<PeerNodeInfo>> {
        let (tx, rx) = oneshot::channel();
        self.send_cmd(Command::QueryRecovery {
            token: token.to_string(),
            neighbors,
            want,
            tx,
        })?;
        tokio::time::timeout(Duration::from_secs(10), rx)
            .await
            .map_err(|_| P2pError::Protocol("recovery query timeout".to_string()))?
            .map_err(|_| P2pError::NotStarted)?
    }

    /// 直连 org-share 推送（逐地址尝试，ok && syncId 匹配即送达）。
    pub async fn org_share_direct(&self, node_info: &PeerNodeInfo, payload: Value) -> Result<bool> {
        let (tx, rx) = oneshot::channel();
        self.send_cmd(Command::OrgShareDirect {
            node_info: node_info.clone(),
            payload,
            tx,
        })?;
        tokio::time::timeout(Duration::from_secs(15), rx)
            .await
            .map_err(|_| P2pError::Protocol("org-share timeout".to_string()))?
            .map_err(|_| P2pError::NotStarted)?
    }

    /// 直连 org-pull 请求（org-pull-list / org-pull-org 帧文本），返回首个可解析响应。
    pub async fn org_pull_request(
        &self,
        node_info: &PeerNodeInfo,
        request_json: &str,
    ) -> Result<Option<Value>> {
        let (tx, rx) = oneshot::channel();
        self.send_cmd(Command::OrgPullRequest {
            node_info: node_info.clone(),
            request_json: request_json.to_string(),
            tx,
        })?;
        tokio::time::timeout(Duration::from_secs(15), rx)
            .await
            .map_err(|_| P2pError::Protocol("org-pull timeout".to_string()))?
            .map_err(|_| P2pError::NotStarted)?
    }

    /// dm 直连投递（`/spark/dm/1.0.0`，1:1 聊天消息与好友请求）：
    /// payload 为 dm 信封 JSON（透明搬运，不验签）；返回对方应用层应答 JSON，
    /// 无应答/不可解析/投递失败返回 `Ok(None)`。
    ///
    /// 外层超时 15s（略大于一次完整逐地址尝试量级：协议单请求 10s + 拨号余量）。
    /// 注意：超时/None 不代表对端未收到——调用方放弃后 attempt 仍可能送达；
    /// 重发可能产生重复，接收侧应按消息 id 去重兜底（kernel 层职责）。
    pub async fn dm_direct(
        &self,
        node_info: &PeerNodeInfo,
        payload: Value,
    ) -> Result<Option<Value>> {
        let (tx, rx) = oneshot::channel();
        self.send_cmd(Command::DmDirect {
            node_info: node_info.clone(),
            payload,
            tx,
        })?;
        tokio::time::timeout(Duration::from_millis(DM_READ_TIMEOUT_MS + 5_000), rx)
            .await
            .map_err(|_| P2pError::Protocol("dm timeout".to_string()))?
            .map_err(|_| P2pError::NotStarted)?
    }

    /// 节点状态快照（UI 诊断）。
    pub async fn local_node_info(&self) -> Result<LocalP2PNodeInfo> {
        let (tx, rx) = oneshot::channel();
        self.send_cmd(Command::LocalNodeInfo { tx })?;
        rx.await.map_err(|_| P2pError::NotStarted)
    }

    /// 向公共 DHT 发布一条记录（原始 key/value 字节，TTL 8h 本地周期重发；
    /// dht_mode = Off 时报错）。供 Phase 2/3 的组织记录等场景复用。
    pub async fn dht_put_record(&self, key: &[u8], value: Vec<u8>) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.send_cmd(Command::DhtPutRecord {
            key: key.to_vec(),
            value,
            tx,
        })?;
        tokio::time::timeout(Duration::from_secs(15), rx)
            .await
            .map_err(|_| P2pError::Protocol("dht put timeout".to_string()))?
            .map_err(|_| P2pError::NotStarted)?
    }

    /// 从公共 DHT 查询一条记录；未命中返回 `Ok(None)`。
    pub async fn dht_get_record(&self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        let (tx, rx) = oneshot::channel();
        self.send_cmd(Command::DhtGetRecord {
            key: key.to_vec(),
            tx,
        })?;
        tokio::time::timeout(Duration::from_secs(15), rx)
            .await
            .map_err(|_| P2pError::Protocol("dht get timeout".to_string()))?
            .map_err(|_| P2pError::NotStarted)?
    }

    /// 在 key 上声明为 provider 并发布一条记录（组织私有 DHT 网关职责，
    /// p2p-messages.md §15：start_providing + 周期重发，挂 keepalive tick）。
    ///
    /// 幂等：相同 (key, value) 重复调用为空操作；value 变化（如地址轮换）
    /// 会重新发布。失败后调用方可重试（未注册的 key 不算占用）。
    pub async fn dht_provide_record(&self, key: &[u8], value: Vec<u8>) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.send_cmd(Command::DhtProvide {
            key: key.to_vec(),
            value,
            tx,
        })?;
        tokio::time::timeout(Duration::from_secs(15), rx)
            .await
            .map_err(|_| P2pError::Protocol("dht provide timeout".to_string()))?
            .map_err(|_| P2pError::NotStarted)?
    }

    /// 查询某 key 的 provider 集合（peerId 字符串列表；空列表 = 无 provider）。
    pub async fn dht_get_providers(&self, key: &[u8]) -> Result<Vec<String>> {
        let (tx, rx) = oneshot::channel();
        self.send_cmd(Command::DhtGetProviders {
            key: key.to_vec(),
            tx,
        })?;
        tokio::time::timeout(Duration::from_secs(15), rx)
            .await
            .map_err(|_| P2pError::Protocol("dht get providers timeout".to_string()))?
            .map_err(|_| P2pError::NotStarted)?
    }

    /// 向已连接对端发起 node-challenge 身份确认（三层确认第③层）；
    /// 未连接或回执验签失败返回 Ok(false)。
    pub async fn challenge_peer(&self, peer_id: &str) -> Result<bool> {
        let (tx, rx) = oneshot::channel();
        self.send_cmd(Command::ChallengePeer {
            peer_id: peer_id.to_string(),
            tx,
        })?;
        tokio::time::timeout(Duration::from_secs(10), rx)
            .await
            .map_err(|_| P2pError::Protocol("challenge timeout".to_string()))?
            .map_err(|_| P2pError::NotStarted)?
    }

    /// 手动触发一次 keepalive tick（测试用；周期 tick 由循环内 interval 驱动）。
    pub async fn maintain_tick(&self) -> Result<KeepaliveStats> {
        let (tx, rx) = oneshot::channel();
        self.send_cmd(Command::Tick { tx })?;
        tokio::time::timeout(Duration::from_secs(10), rx)
            .await
            .map_err(|_| P2pError::Protocol("tick timeout".to_string()))?
            .map_err(|_| P2pError::NotStarted)
    }
}

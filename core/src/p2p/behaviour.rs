//! libp2p 装配（core/spec/p2p-messages.md §1.1）：
//! TCP（noise + yamux）+ WebSocket 双栈 + relay client；mDNS、identify、ping、
//! relay server、AutoNAT、UPnP、gossipsub（flood_publish）与六个直连协议。
//!
//! 与 TS 的差异（有意决策）：
//! - 多路复用器 yamux（rust-libp2p 已弃 mplex；TS 侧已按迁移桥接追加 yamux，
//!   TS↔Rust 协商 `/yamux/1.0.0`，见 core/spec/p2p-messages.md §1.1 互通验证记录）；
//! - `/spark/version/1.0.0` 用专用 [`VersionFrameCodec`]：TS 语义是"响应方开流即写、
//!   请求方不写内容"，与 request-response 框架形状冲突，见 spec §6 互通验证记录；
//! - gossipsub 无 `allowPublishToZeroTopicPeers` 等价物：rust 侧 `flood_publish(true)`
//!   下发布不依赖 mesh，零订阅者时的 `NoPeersSubscribedToTopic` 错误由发布方容忍。

use std::time::Duration;

use libp2p::swarm::NetworkBehaviour;
use libp2p::swarm::behaviour::toggle::Toggle;
use libp2p::{
    StreamProtocol, autonat, gossipsub, identify, kad, mdns, ping, relay, request_response, upnp,
};

use super::constants::{
    AFFAIR_META_READ_TIMEOUT_MS, AFFAIR_META_RR_PROTOCOL, BLOB_FETCH_READ_TIMEOUT_MS,
    DHT_RECORD_TTL_SECS, DIRECT_BLOB_FETCH_PROTOCOL, DIRECT_DM_PROTOCOL, DIRECT_ORG_MAIL_PROTOCOL,
    DIRECT_ORG_RECOVERY_PROTOCOL, DIRECT_PEER_EXCHANGE_PROTOCOL, DIRECT_VERSION_PROTOCOL,
    DM_READ_TIMEOUT_MS, KAD_PROTOCOL_NAME, NODE_CHALLENGE_PROTOCOL, NODE_CHALLENGE_READ_TIMEOUT_MS,
    ORG_MAIL_READ_TIMEOUT_MS, ORG_RECOVERY_READ_TIMEOUT_MS, PEER_EXCHANGE_READ_RESPONSE_TIMEOUT_MS,
    RELAY_DEFAULT_DATA_LIMIT_BYTES, RELAY_DEFAULT_DURATION_LIMIT_SECS, RELAY_MAX_RESERVATIONS,
    VERSION_PROTOCOL_READ_TIMEOUT_MS,
};

/// 直连协议单帧上限（1 MiB，防畸形放大；正常帧远小于此）。
const MAX_FRAME_LEN: u64 = 1024 * 1024;

/// blob-fetch 协议单帧上限：响应帧携带本体 base64（内容上限
/// [`crate::content::CONTENT_BLOB_MAX_BYTES`] = 10 MiB ≈ 14 MB 线形），
/// 取 16 MiB 留 JSON 包装余量；其余直连协议仍用 1 MiB。
const BLOB_FETCH_FRAME_MAX_LEN: u64 = 16 * 1024 * 1024;

/// 直连协议帧编解码：整段 UTF-8 JSON 作为单帧写入（**不在 codec 内关流**——
/// request-response handler 在写完请求/响应后自行 `stream.close()`，对端以 EOF
/// 为帧边界；codec 内重复 close 会让 yamux 丢弃已写数据）。
#[derive(Clone, Debug)]
pub struct JsonFrameCodec {
    max_len: u64,
}

/// 注意：`request_response::Behaviour::new` 走 `TCodec::default()`，
/// 派生 Default 会把 max_len 置 0（take(0) 把所有帧截成空串），必须手写。
impl Default for JsonFrameCodec {
    fn default() -> Self {
        Self::new()
    }
}

impl JsonFrameCodec {
    pub fn new() -> Self {
        Self {
            max_len: MAX_FRAME_LEN,
        }
    }

    /// 自定义单帧上限（blob-fetch 等大帧协议用；`Behaviour::with_codec` 装配）。
    pub fn with_max_len(max_len: u64) -> Self {
        Self { max_len }
    }
}

#[async_trait::async_trait]
impl request_response::Codec for JsonFrameCodec {
    type Protocol = StreamProtocol;
    type Request = String;
    type Response = String;

    async fn read_request<T>(
        &mut self,
        _protocol: &Self::Protocol,
        io: &mut T,
    ) -> std::io::Result<Self::Request>
    where
        T: libp2p::futures::AsyncRead + Unpin + Send,
    {
        read_frame(io, self.max_len).await
    }

    async fn read_response<T>(
        &mut self,
        _protocol: &Self::Protocol,
        io: &mut T,
    ) -> std::io::Result<Self::Response>
    where
        T: libp2p::futures::AsyncRead + Unpin + Send,
    {
        read_frame(io, self.max_len).await
    }

    async fn write_request<T>(
        &mut self,
        _protocol: &Self::Protocol,
        io: &mut T,
        req: Self::Request,
    ) -> std::io::Result<()>
    where
        T: libp2p::futures::AsyncWrite + Unpin + Send,
    {
        write_frame(io, &req).await
    }

    async fn write_response<T>(
        &mut self,
        _protocol: &Self::Protocol,
        io: &mut T,
        res: Self::Response,
    ) -> std::io::Result<()>
    where
        T: libp2p::futures::AsyncWrite + Unpin + Send,
    {
        write_frame(io, &res).await
    }
}

async fn read_frame<T>(io: &mut T, max_len: u64) -> std::io::Result<String>
where
    T: libp2p::futures::AsyncRead + Unpin + Send,
{
    use libp2p::futures::AsyncReadExt;
    let mut limited = io.take(max_len);
    let mut buf = Vec::new();
    limited.read_to_end(&mut buf).await?;
    String::from_utf8(buf).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

async fn write_frame<T>(io: &mut T, text: &str) -> std::io::Result<()>
where
    T: libp2p::futures::AsyncWrite + Unpin + Send,
{
    use libp2p::futures::AsyncWriteExt;
    io.write_all(text.as_bytes()).await?;
    io.flush().await
}

/// version 协议编解码（`/spark/version/1.0.0`）。
///
/// TS 口径是"响应方连接打开即写、请求方**不写任何内容**"（core/spec/p2p-messages.md §6），
/// 与 request-response 框架"先读请求再回响应"的形状冲突。互通验证（阶段② TS↔Rust lab）
/// 发现：沿用 JsonFrameCodec 时 read_request 等 EOF，TS 请求方永不写字也不半关闭，
/// 入站升级 2500ms 超时重置子流，TS→Rust 版本探测必败。
///
/// 收口：`read_request` **立即返回空串、不读字节**——响应侧在子流打开后立即写版本帧，
/// 对齐 TS 语义；Rust 请求方仍写空帧（0 字节），TS 响应方本就不读请求，两方向兼容。
#[derive(Clone, Debug, Default)]
pub struct VersionFrameCodec;

#[async_trait::async_trait]
impl request_response::Codec for VersionFrameCodec {
    type Protocol = StreamProtocol;
    type Request = String;
    type Response = String;

    async fn read_request<T>(
        &mut self,
        _protocol: &Self::Protocol,
        _io: &mut T,
    ) -> std::io::Result<Self::Request>
    where
        T: libp2p::futures::AsyncRead + Unpin + Send,
    {
        // 不读任何字节：请求方按协议不写内容，读到 EOF 的等法会让 TS 探测超时
        Ok(String::new())
    }

    async fn read_response<T>(
        &mut self,
        _protocol: &Self::Protocol,
        io: &mut T,
    ) -> std::io::Result<Self::Response>
    where
        T: libp2p::futures::AsyncRead + Unpin + Send,
    {
        read_frame(io, MAX_FRAME_LEN).await
    }

    async fn write_request<T>(
        &mut self,
        _protocol: &Self::Protocol,
        io: &mut T,
        req: Self::Request,
    ) -> std::io::Result<()>
    where
        T: libp2p::futures::AsyncWrite + Unpin + Send,
    {
        write_frame(io, &req).await
    }

    async fn write_response<T>(
        &mut self,
        _protocol: &Self::Protocol,
        io: &mut T,
        res: Self::Response,
    ) -> std::io::Result<()>
    where
        T: libp2p::futures::AsyncWrite + Unpin + Send,
    {
        write_frame(io, &res).await
    }
}

/// DHT（Kad）运行模式。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DhtMode {
    /// 完全私有：不挂载 Kad 行为（隐私开关）。
    Off,
    /// 仅客户端：参与查询但不提供路由/存储服务（移动端预留）。
    Client,
    /// 全量节点：路由 + 记录存储（默认）。
    #[default]
    Server,
}

impl DhtMode {
    /// 存储/命令线形字符串（off/client/server）。
    pub fn as_str(&self) -> &'static str {
        match self {
            DhtMode::Off => "off",
            DhtMode::Client => "client",
            DhtMode::Server => "server",
        }
    }

    /// 从线形字符串解析；非法值返回 None。
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "off" => Some(DhtMode::Off),
            "client" => Some(DhtMode::Client),
            "server" => Some(DhtMode::Server),
            _ => None,
        }
    }
}

/// relay server 配额配置（relay-strategy §1「有配额」）：预约限时限量
/// （默认 2h / 256MiB）+ 预约名额上限 + 转发限量，全部对齐网络常量。
/// `max_reservations` 为测试/运维覆盖口（None = 常量 15）。
pub(crate) fn relay_server_config(max_reservations: Option<usize>) -> relay::Config {
    relay::Config {
        max_reservations: max_reservations.unwrap_or(RELAY_MAX_RESERVATIONS),
        reservation_duration: Duration::from_secs(RELAY_DEFAULT_DURATION_LIMIT_SECS),
        max_circuit_bytes: RELAY_DEFAULT_DATA_LIMIT_BYTES,
        ..Default::default()
    }
}

/// relay server（hop）行为构造：初始挂载（build_behaviour）与 R1 AutoNAT
/// Public 重新挂载共用同一配置。
pub(crate) fn build_relay_server(
    local_peer_id: libp2p::PeerId,
    max_reservations: Option<usize>,
) -> relay::Behaviour {
    relay::Behaviour::new(local_peer_id, relay_server_config(max_reservations))
}

/// Spark 组合行为。
#[derive(NetworkBehaviour)]
pub struct SparkBehaviour {
    pub gossipsub: gossipsub::Behaviour,
    pub mdns: Toggle<mdns::tokio::Behaviour>,
    pub identify: identify::Behaviour,
    pub ping: ping::Behaviour,
    pub relay_server: Toggle<relay::Behaviour>,
    pub relay_client: relay::client::Behaviour,
    // dcutr 打洞（电路升直连，dcutr-hole-punch §2.1）：leaf/桌面同挂——
    // 客户端行为，与是否提供 relay server 无关；依赖 identify + relay client
    //（均已挂载）。Toggle 挂载：测试/运维可关（旧端模拟、故障排查）。
    pub dcutr: Toggle<libp2p::dcutr::Behaviour>,
    pub autonat: autonat::Behaviour,
    pub upnp: Toggle<upnp::tokio::Behaviour>,
    pub kad: Toggle<kad::Behaviour<kad::store::MemoryStore>>,
    pub version_rr: request_response::Behaviour<VersionFrameCodec>,
    pub exchange_rr: request_response::Behaviour<JsonFrameCodec>,
    pub recovery_rr: request_response::Behaviour<JsonFrameCodec>,
    /// org-mail（跨组织网关邮箱）直连（阶段四E，p2p-org-mail §21）。
    pub org_mail_rr: request_response::Behaviour<JsonFrameCodec>,
    pub node_challenge_rr: request_response::Behaviour<JsonFrameCodec>,
    pub dm_rr: request_response::Behaviour<JsonFrameCodec>,
    /// indexer 查询（affair-metadata §8；C10）。
    pub affair_meta_rr: request_response::Behaviour<JsonFrameCodec>,
    /// 内容面 blob 拉取（public-topics §七「持有即做种」的传输协议；
    /// 大帧 codec——响应携带本体 base64）。
    pub blob_fetch_rr: request_response::Behaviour<JsonFrameCodec>,
}

/// 装配开关（测试可关闭 mDNS/UPnP）。
#[derive(Clone, Debug)]
pub struct BehaviourOptions {
    pub enable_mdns: bool,
    pub enable_upnp: bool,
    /// 是否挂载 relay server（接受他人预约）。桌面默认 true；移动端 false
    /// （peer-rediscovery §7.2：移动端只作 relay client）。
    pub enable_relay_server: bool,
    /// relay server 预约名额覆盖（配额，测试/运维调小模拟满载；
    /// None = 网络常量 [`RELAY_MAX_RESERVATIONS`]）。
    pub relay_max_reservations: Option<usize>,
    /// dcutr 打洞（电路升直连）挂载开关：默认 true；关 = 旧端形态
    /// （identify 协议清单不含 /libp2p/dcutr，对端不发起升级）。
    pub enable_dcutr: bool,
    pub dht_mode: DhtMode,
    /// 叶子模式（mobile-leaf-mode §3）：gossipsub 业务 mesh 订阅关闭（只消费
    /// 不服务，不为他人中继）。
    pub leaf_mode: bool,
}

impl Default for BehaviourOptions {
    fn default() -> Self {
        Self {
            enable_mdns: true,
            enable_upnp: true,
            enable_relay_server: true,
            relay_max_reservations: None,
            enable_dcutr: true,
            dht_mode: DhtMode::default(),
            leaf_mode: false,
        }
    }
}

/// 构造组合行为。
pub fn build_behaviour(
    keypair: &libp2p::identity::Keypair,
    relay_client: relay::client::Behaviour,
    options: &BehaviourOptions,
) -> std::result::Result<SparkBehaviour, Box<dyn std::error::Error + Send + Sync>> {
    let local_peer_id = keypair.public().to_peer_id();

    let gossipsub_config = gossipsub::ConfigBuilder::default()
        .flood_publish(true)
        // validate_messages：消息先入 mcache 等待应用层显式上报 Accept/Ignore/Reject
        // 再决定转发（plugin-announce relay 资历制依赖此机制；overlay/sync 在
        // 分发处无条件回报 Accept，语义与开启前一致）
        .validate_messages()
        .validation_mode(gossipsub::ValidationMode::Strict)
        .build()?;
    let mut gossipsub_behaviour = gossipsub::Behaviour::new(
        gossipsub::MessageAuthenticity::Signed(keypair.clone()),
        gossipsub_config,
    )
    .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    // leaf 模式 §3：gossipsub 订阅全砍（SYNC/OVERLAY/PLUGIN_ANNOUNCE/AFFAIR_META）——leaf
    // 不为他人中继（订阅一断 publish 自然空操作，与 gossip.rs 发布守卫双保险）。
    // PLUGIN_ANNOUNCE 亦砍：市场索引改经 pdsync `mkt:ann` 类目同步分发
    // （mobile-leaf-mode；plugin-dist §8 公告自含签名，PC 桥接收 gossip 后
    // 受管落库同步给叶子），leaf 不订阅、发布守卫（gossip.rs）保留。
    // AFFAIR_META 同砍：轻客户端/叶子不订阅元数据面，只向 indexer 查询
    // （affair-metadata §2「订阅与否由角色决定」）。
    if !options.leaf_mode {
        gossipsub_behaviour.subscribe(&gossipsub::IdentTopic::new(super::constants::SYNC_TOPIC))?;
        gossipsub_behaviour
            .subscribe(&gossipsub::IdentTopic::new(super::constants::OVERLAY_TOPIC))?;
        // 插件市场广播索引（plugin-dist §8；启动即订阅，relay 校验链在 gossip 层）
        gossipsub_behaviour.subscribe(&gossipsub::IdentTopic::new(
            super::constants::PLUGIN_ANNOUNCE_TOPIC,
        ))?;
        // 议题元数据公告（affair-metadata §2；不强制签名，暂存区归 C10）
        gossipsub_behaviour.subscribe(&gossipsub::IdentTopic::new(
            super::constants::AFFAIR_META_TOPIC,
        ))?;
    }

    let mdns_behaviour = if options.enable_mdns {
        Toggle::from(Some(mdns::tokio::Behaviour::new(
            mdns::Config::default(),
            local_peer_id,
        )?))
    } else {
        Toggle::from(None)
    };

    // identify 协议串对齐 JS 默认（protocolPrefix 'ipfs'）
    let identify_behaviour = identify::Behaviour::new(identify::Config::new(
        "/ipfs/id/1.0.0".to_string(),
        keypair.public(),
    ));

    // relay server 按 enable_relay_server 条件挂载：桌面默认开启（接受他人预约），
    // 移动端关闭（只作 relay client，peer-rediscovery §7.2）。R1（relay-implementation
    // §2）：初始挂载为现状兼容；AutoNAT Private 时由事件循环摘牌（Toggle 换 None）。
    let relay_server = if options.enable_relay_server {
        Toggle::from(Some(build_relay_server(
            local_peer_id,
            options.relay_max_reservations,
        )))
    } else {
        Toggle::from(None)
    };

    let upnp_behaviour = if options.enable_upnp {
        Toggle::from(Some(upnp::tokio::Behaviour::default()))
    } else {
        Toggle::from(None)
    };

    let version_rr = request_response::Behaviour::new(
        [(
            StreamProtocol::new(DIRECT_VERSION_PROTOCOL),
            request_response::ProtocolSupport::Full,
        )],
        request_response::Config::default()
            .with_request_timeout(Duration::from_millis(VERSION_PROTOCOL_READ_TIMEOUT_MS)),
    );
    let exchange_rr = request_response::Behaviour::new(
        [(
            StreamProtocol::new(DIRECT_PEER_EXCHANGE_PROTOCOL),
            request_response::ProtocolSupport::Full,
        )],
        request_response::Config::default().with_request_timeout(Duration::from_millis(
            PEER_EXCHANGE_READ_RESPONSE_TIMEOUT_MS,
        )),
    );
    let recovery_rr = request_response::Behaviour::new(
        [(
            StreamProtocol::new(DIRECT_ORG_RECOVERY_PROTOCOL),
            request_response::ProtocolSupport::Full,
        )],
        request_response::Config::default()
            .with_request_timeout(Duration::from_millis(ORG_RECOVERY_READ_TIMEOUT_MS)),
    );
    let node_challenge_rr = request_response::Behaviour::new(
        [(
            StreamProtocol::new(NODE_CHALLENGE_PROTOCOL),
            request_response::ProtocolSupport::Full,
        )],
        request_response::Config::default()
            .with_request_timeout(Duration::from_millis(NODE_CHALLENGE_READ_TIMEOUT_MS)),
    );
    // 阶段四E：org-mail 直连协议（JsonFrameCodec 模式同族）
    let org_mail_rr = request_response::Behaviour::new(
        [(
            StreamProtocol::new(DIRECT_ORG_MAIL_PROTOCOL),
            request_response::ProtocolSupport::Full,
        )],
        request_response::Config::default()
            .with_request_timeout(Duration::from_millis(ORG_MAIL_READ_TIMEOUT_MS)),
    );
    let dm_rr = request_response::Behaviour::new(
        [(
            StreamProtocol::new(DIRECT_DM_PROTOCOL),
            request_response::ProtocolSupport::Full,
        )],
        request_response::Config::default()
            .with_request_timeout(Duration::from_millis(DM_READ_TIMEOUT_MS)),
    );
    // C10：indexer 查询协议（affair-metadata §8；轻客户端 → 启用角色节点）
    let affair_meta_rr = request_response::Behaviour::new(
        [(
            StreamProtocol::new(AFFAIR_META_RR_PROTOCOL),
            request_response::ProtocolSupport::Full,
        )],
        request_response::Config::default()
            .with_request_timeout(Duration::from_millis(AFFAIR_META_READ_TIMEOUT_MS)),
    );
    // 内容面 blob 拉取（public-topics §七）：大帧 codec（响应携带本体 base64），
    // 超时按 10 MiB 本体在慢链路上的传输余量取 30s
    let blob_fetch_rr = request_response::Behaviour::with_codec(
        JsonFrameCodec::with_max_len(BLOB_FETCH_FRAME_MAX_LEN),
        [(
            StreamProtocol::new(DIRECT_BLOB_FETCH_PROTOCOL),
            request_response::ProtocolSupport::Full,
        )],
        request_response::Config::default()
            .with_request_timeout(Duration::from_millis(BLOB_FETCH_READ_TIMEOUT_MS)),
    );

    // Kad：Off 不挂载；Client 挂但只查不服务；Server 全量。
    // MemoryStore + 本地周期重发即可（记录都带 TTL，不接 sled）。
    // 注意：kad 默认 mode 是 Client（仅在有外部地址时自动升 Server），
    // Client 模式下入站子流是 DeniedUpgrade，必须按 dht_mode 显式 set_mode。
    let kad_behaviour = match options.dht_mode {
        DhtMode::Off => Toggle::from(None),
        mode => {
            let store = kad::store::MemoryStore::new(local_peer_id);
            let mut kad_config = kad::Config::new(StreamProtocol::new(KAD_PROTOCOL_NAME));
            kad_config.set_record_ttl(Some(Duration::from_secs(DHT_RECORD_TTL_SECS)));
            let mut behaviour = kad::Behaviour::with_config(local_peer_id, store, kad_config);
            behaviour.set_mode(Some(match mode {
                DhtMode::Client => kad::Mode::Client,
                DhtMode::Server => kad::Mode::Server,
                DhtMode::Off => unreachable!("Off handled above"),
            }));
            Toggle::from(Some(behaviour))
        }
    };

    Ok(SparkBehaviour {
        gossipsub: gossipsub_behaviour,
        mdns: mdns_behaviour,
        identify: identify_behaviour,
        ping: ping::Behaviour::new(ping::Config::new()),
        relay_server,
        relay_client,
        dcutr: Toggle::from(
            options
                .enable_dcutr
                .then(|| libp2p::dcutr::Behaviour::new(local_peer_id)),
        ),
        autonat: autonat::Behaviour::new(local_peer_id, autonat::Config::default()),
        upnp: upnp_behaviour,
        kad: kad_behaviour,
        version_rr,
        exchange_rr,
        recovery_rr,
        org_mail_rr,
        node_challenge_rr,
        dm_rr,
        affair_meta_rr,
        blob_fetch_rr,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// relay 配额接线（relay-strategy §1「预约限时限量（默认 2h / 256MiB
    /// 续期），转发限量」）：配置全部取自网络常量；覆盖口只调预约名额
    /// （测试模拟满载）；时限/字节量/名额的执行在 libp2p relay server 内部
    /// （既有能力，零自研），超限拒绝的行为面见 tests/p2p_relay_quota.rs。
    #[test]
    fn relay_server_config_wires_quota_constants() {
        let cfg = relay_server_config(None);
        assert_eq!(cfg.max_reservations, RELAY_MAX_RESERVATIONS);
        assert_eq!(
            cfg.reservation_duration,
            Duration::from_secs(RELAY_DEFAULT_DURATION_LIMIT_SECS),
            "预约限时 2h"
        );
        assert_eq!(
            cfg.max_circuit_bytes, RELAY_DEFAULT_DATA_LIMIT_BYTES,
            "转发限量 256MiB"
        );
        let overridden = relay_server_config(Some(1));
        assert_eq!(overridden.max_reservations, 1, "覆盖口只调预约名额");
        assert_eq!(
            overridden.reservation_duration,
            Duration::from_secs(RELAY_DEFAULT_DURATION_LIMIT_SECS),
            "覆盖不影响时限/字节量"
        );
        assert_eq!(overridden.max_circuit_bytes, RELAY_DEFAULT_DATA_LIMIT_BYTES);
    }
}

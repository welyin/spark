//! libp2p 装配（core/spec/p2p-messages.md §1.1）：
//! TCP（noise + yamux）+ WebSocket 双栈 + relay client；mDNS、identify、ping、
//! relay server、AutoNAT、UPnP、gossipsub（flood_publish）与四个直连协议。
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
use libp2p::{StreamProtocol, autonat, gossipsub, identify, mdns, ping, relay, request_response, upnp};

use super::constants::{
    DIRECT_ORG_RECOVERY_PROTOCOL, DIRECT_ORG_SHARE_PROTOCOL, DIRECT_PEER_EXCHANGE_PROTOCOL,
    DIRECT_VERSION_PROTOCOL, ORG_RECOVERY_READ_TIMEOUT_MS, ORG_SHARE_READ_TIMEOUT_MS,
    PEER_EXCHANGE_READ_RESPONSE_TIMEOUT_MS, RELAY_DEFAULT_DATA_LIMIT_BYTES,
    RELAY_DEFAULT_DURATION_LIMIT_SECS, RELAY_MAX_RESERVATIONS, VERSION_PROTOCOL_READ_TIMEOUT_MS,
};

/// 直连协议单帧上限（1 MiB，防畸形放大；正常帧远小于此）。
const MAX_FRAME_LEN: u64 = 1024 * 1024;

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

/// Spark 组合行为。
#[derive(NetworkBehaviour)]
pub struct SparkBehaviour {
    pub gossipsub: gossipsub::Behaviour,
    pub mdns: Toggle<mdns::tokio::Behaviour>,
    pub identify: identify::Behaviour,
    pub ping: ping::Behaviour,
    pub relay_server: relay::Behaviour,
    pub relay_client: relay::client::Behaviour,
    pub autonat: autonat::Behaviour,
    pub upnp: Toggle<upnp::tokio::Behaviour>,
    pub version_rr: request_response::Behaviour<VersionFrameCodec>,
    pub exchange_rr: request_response::Behaviour<JsonFrameCodec>,
    pub recovery_rr: request_response::Behaviour<JsonFrameCodec>,
    pub org_share_rr: request_response::Behaviour<JsonFrameCodec>,
}

/// 装配开关（测试可关闭 mDNS/UPnP）。
#[derive(Clone, Debug)]
pub struct BehaviourOptions {
    pub enable_mdns: bool,
    pub enable_upnp: bool,
}

impl Default for BehaviourOptions {
    fn default() -> Self {
        Self {
            enable_mdns: true,
            enable_upnp: true,
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
        .validation_mode(gossipsub::ValidationMode::Strict)
        .build()?;
    let mut gossipsub_behaviour = gossipsub::Behaviour::new(
        gossipsub::MessageAuthenticity::Signed(keypair.clone()),
        gossipsub_config,
    )
    .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    gossipsub_behaviour.subscribe(&gossipsub::IdentTopic::new(super::constants::SYNC_TOPIC))?;
    gossipsub_behaviour.subscribe(&gossipsub::IdentTopic::new(super::constants::OVERLAY_TOPIC))?;

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

    let relay_config = relay::Config {
        max_reservations: RELAY_MAX_RESERVATIONS,
        reservation_duration: Duration::from_secs(RELAY_DEFAULT_DURATION_LIMIT_SECS),
        max_circuit_bytes: RELAY_DEFAULT_DATA_LIMIT_BYTES,
        ..Default::default()
    };
    let relay_server = relay::Behaviour::new(local_peer_id, relay_config);

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
        request_response::Config::default()
            .with_request_timeout(Duration::from_millis(PEER_EXCHANGE_READ_RESPONSE_TIMEOUT_MS)),
    );
    let recovery_rr = request_response::Behaviour::new(
        [(
            StreamProtocol::new(DIRECT_ORG_RECOVERY_PROTOCOL),
            request_response::ProtocolSupport::Full,
        )],
        request_response::Config::default()
            .with_request_timeout(Duration::from_millis(ORG_RECOVERY_READ_TIMEOUT_MS)),
    );
    let org_share_rr = request_response::Behaviour::new(
        [(
            StreamProtocol::new(DIRECT_ORG_SHARE_PROTOCOL),
            request_response::ProtocolSupport::Full,
        )],
        request_response::Config::default()
            .with_request_timeout(Duration::from_millis(ORG_SHARE_READ_TIMEOUT_MS)),
    );

    Ok(SparkBehaviour {
        gossipsub: gossipsub_behaviour,
        mdns: mdns_behaviour,
        identify: identify_behaviour,
        ping: ping::Behaviour::new(ping::Config::new()),
        relay_server,
        relay_client,
        autonat: autonat::Behaviour::new(local_peer_id, autonat::Config::default()),
        upnp: upnp_behaviour,
        version_rr,
        exchange_rr,
        recovery_rr,
        org_share_rr,
    })
}

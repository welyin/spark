//! p2p 模块：Spark 内核网络层。
//!
//! 精确规格见 `core/spec/p2p-messages.md`（协议帧、三套签名体系、邻居打分、
//! keepalive 流程与"已知不一致与坑"），组织业务语义见 `core/spec/org.md`。
//!
//! 分层：本模块负责传输、协议帧、签名与邻居层；组织/同步的业务状态由宿主经
//! [`host::P2pHost`] trait 注入（类似 sync 的 PurgeWatermark 模式），本模块不做
//! 任何 db 直接业务操作。所有时间经 `now_fn` 注入（对齐 now_ms 模式）。

pub mod announce;
pub mod behaviour;
pub mod constants;
pub mod direct;
pub mod envelope;
pub mod host;
pub mod identity_store;
pub mod keepalive;
pub mod listen_port;
pub mod node;
pub mod overlay_store;
pub mod peer_activity;
pub mod peer_targets;

pub use announce::{
    AnnounceReject, NodeAnnounce, NodeAnnounceValidator, announce_to_json,
    build_node_announce_payload, prepare_publish_addresses, public_key_from_peer_id_str,
    sign_node_announce,
};
pub use envelope::{
    ENVELOPE_VERSION, Envelope, EnvelopeSigner, VerifiedEnvelope, build_delete_body,
    build_org_body, build_update_body, decode_envelope_public_key, is_signature_mandatory_type,
    parse_and_verify_envelope, spki_der_base64, spki_der_from_raw, spki_der_pem,
};
pub use host::{OrgShareAck, P2pHost};
pub use node::{KeepaliveStats, LocalP2PNodeInfo, P2pConfig, P2pEvent, P2pNode};
pub use overlay_store::{OverlayPeerRecord, OverlayPeerSource, OverlayPeerStore};
pub use peer_activity::{
    NO_RECORD_PRIORITY, NodeObservation, PeerActivityRecord, PeerActivityStore, compute_priority,
};
pub use peer_targets::{PeerNodeInfo, build_dial_targets, extract_peer_id};

/// p2p 模块统一错误。
#[derive(Debug, thiserror::Error)]
pub enum P2pError {
    /// 节点未启动。
    #[error("p2p node not started")]
    NotStarted,

    /// 信封/消息形状非法。
    #[error("malformed message: {0}")]
    Malformed(String),

    /// 签名无效。
    #[error("signature invalid")]
    SignatureInvalid,

    /// 拨号失败。
    #[error("dial failed: {0}")]
    Dial(String),

    /// 协议读写失败或超时。
    #[error("protocol error: {0}")]
    Protocol(String),

    /// libp2p 装配错误。
    #[error("swarm error: {0}")]
    Swarm(String),

    /// 存储后端错误。
    #[error(transparent)]
    Storage(#[from] crate::storage::StorageError),

    /// JSON 序列化/反序列化错误。
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    /// 宿主回调错误。
    #[error("host callback error: {0}")]
    Host(String),
}

/// p2p 模块 Result 别名。
pub type Result<T> = std::result::Result<T, P2pError>;

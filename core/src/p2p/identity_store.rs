//! libp2p 节点身份持久化（对齐 identity-store.ts 与 core/spec/p2p-messages.md §1.3）。
//!
//! Ed25519 keypair → `to_protobuf_encoding()` → base64 存 `p2p:identity:privateKey`；
//! 读取失败即重新生成并写回。PeerId 由公钥派生，重启稳定。
//! protobuf 线形与 TS `@libp2p/crypto privateKeyToProtobuf` 一致（Ed25519 Data = 64B）。

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use libp2p::identity::Keypair;

use crate::storage::StorageBackend;

use super::constants::P2P_IDENTITY_PRIVATE_KEY;
use super::{P2pError, Result};

/// 仅读取持久化 libp2p 私钥并返回对应 peerId（只读，无则返回 None）。
/// 用于 p2p 尚未启动但需要本机 peerId 的场景，避免在查询路径产生写副作用。
pub fn load_peer_id(storage: &dyn StorageBackend) -> Option<String> {
    let encoded = storage.get(P2P_IDENTITY_PRIVATE_KEY).ok().flatten()?;
    let bytes = B64.decode(encoded.trim()).ok()?;
    let keypair = Keypair::from_protobuf_encoding(&bytes).ok()?;
    Some(libp2p::identity::PeerId::from_public_key(&keypair.public()).to_base58())
}

/// 从持久化 libp2p 私钥编码中读取 Ed25519 公钥（32B 原始字节 base64）。
/// p2p 未启动、但需要 devicePubKey 的兜底采集路径使用。
pub fn load_libp2p_pub_key(storage: &dyn StorageBackend) -> Option<String> {
    let encoded = storage.get(P2P_IDENTITY_PRIVATE_KEY).ok().flatten()?;
    let bytes = B64.decode(encoded.trim()).ok()?;
    let keypair = Keypair::from_protobuf_encoding(&bytes).ok()?;
    let ed25519 = keypair.try_into_ed25519().ok()?;
    Some(B64.encode(ed25519.public().to_bytes()))
}

/// 从持久化 libp2p 私钥派生 X25519 私钥标量（M3 epoch 包裹用）。
/// 不返回 Ed25519 seed，避免在业务层扩散原始签名密钥。
pub fn load_x25519_private_key(storage: &dyn StorageBackend) -> Option<[u8; 32]> {
    let encoded = storage.get(P2P_IDENTITY_PRIVATE_KEY).ok().flatten()?;
    let bytes = B64.decode(encoded.trim()).ok()?;
    let keypair = Keypair::from_protobuf_encoding(&bytes).ok()?;
    let ed25519 = keypair.try_into_ed25519().ok()?;
    // libp2p ed25519 Keypair::to_bytes 为 96B（扩展私钥 64B + 公钥 32B），
    // 前 32B 即 Ed25519 seed，正是 ed_sk_to_x25519 所需输入。
    let bytes = ed25519.to_bytes();
    let seed = bytes[..32].try_into().ok()?;
    Some(crate::sync::orgsync::ed_sk_to_x25519(&seed))
}

/// 读取或创建 libp2p 私钥（同设备 PeerId 稳定）。
pub fn get_or_create_libp2p_keypair(storage: &mut dyn StorageBackend) -> Result<Keypair> {
    let persisted = storage.get(P2P_IDENTITY_PRIVATE_KEY)?;
    if let Some(ref encoded) = persisted
        && let Ok(bytes) = B64.decode(encoded.trim())
        && let Ok(keypair) = Keypair::from_protobuf_encoding(&bytes)
    {
        log::info!(
            "[P2P_IDENTITY] loaded persisted keypair | peerId={}",
            libp2p::identity::PeerId::from_public_key(&keypair.public()).to_base58()
        );
        return Ok(keypair);
    }

    let keypair = Keypair::generate_ed25519();
    let raw = keypair
        .to_protobuf_encoding()
        .map_err(|e| P2pError::Swarm(format!("keypair encode failed: {e}")))?;
    storage.put(P2P_IDENTITY_PRIVATE_KEY, &B64.encode(raw))?;
    log::info!(
        "[P2P_IDENTITY] generated NEW keypair (persisted={}) | peerId={}",
        persisted.is_some(),
        libp2p::identity::PeerId::from_public_key(&keypair.public()).to_base58()
    );
    Ok(keypair)
}

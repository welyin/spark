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

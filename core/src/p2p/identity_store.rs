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
    if let Some(encoded) = storage.get(P2P_IDENTITY_PRIVATE_KEY)?
        && let Ok(bytes) = B64.decode(encoded.trim())
        && let Ok(keypair) = Keypair::from_protobuf_encoding(&bytes)
    {
        return Ok(keypair);
    }

    let keypair = Keypair::generate_ed25519();
    let raw = keypair
        .to_protobuf_encoding()
        .map_err(|e| P2pError::Swarm(format!("keypair encode failed: {e}")))?;
    storage.put(P2P_IDENTITY_PRIVATE_KEY, &B64.encode(raw))?;
    Ok(keypair)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::MemoryStorage;

    #[test]
    fn identity_persists_across_loads() {
        let mut storage = MemoryStorage::new();
        let first = get_or_create_libp2p_keypair(&mut storage).unwrap();
        let second = get_or_create_libp2p_keypair(&mut storage).unwrap();
        assert_eq!(first.public(), second.public());
    }

    #[test]
    fn corrupt_entry_regenerates() {
        let mut storage = MemoryStorage::new();
        storage
            .put(P2P_IDENTITY_PRIVATE_KEY, "not-valid-base64!!!")
            .unwrap();
        let keypair = get_or_create_libp2p_keypair(&mut storage).unwrap();
        // 写回后可正常读
        let reloaded = get_or_create_libp2p_keypair(&mut storage).unwrap();
        assert_eq!(keypair.public(), reloaded.public());
    }

    #[test]
    fn protobuf_roundtrip_is_ts_compatible_shape() {
        // TS privateKeyToProtobuf：{Type: Ed25519(1), Data: 64B(seed||pubkey)}
        let keypair = Keypair::generate_ed25519();
        let raw = keypair.to_protobuf_encoding().unwrap();
        // protobuf: field1 varint = 1 → 0x08 0x01；field2 bytes len=64 → 0x12 0x40
        assert_eq!(&raw[..4], &[0x08, 0x01, 0x12, 0x40]);
        assert_eq!(raw.len(), 4 + 64);
    }
}

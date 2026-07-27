//! libp2p 节点身份密钥对持久化单测。

use libp2p::identity::Keypair;
use spark_core::p2p::constants::P2P_IDENTITY_PRIVATE_KEY;
use spark_core::p2p::identity_store::*;
use spark_core::storage::{MemoryStorage, StorageBackend};

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

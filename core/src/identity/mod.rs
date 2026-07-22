//! 身份模块：BIP39 助记词、SLIP-0010 ed25519 派生、身份文件 v2/v1 加解密。
//!
//! 算法精确规格见 `core/spec/identity.md`，验收向量见
//! `core/spec/vectors/identity.json`。

pub mod crypto;
pub mod derive;
pub mod error;
pub mod file;
pub mod mnemonic;
pub mod slip10;

pub use derive::{
    Identity, ROOT_DERIVATION_PATH, derive_domain_identity, derive_identity_at_path,
    derive_root_identity, domain_indices, verify_ed25519_signature,
};
pub use error::{IdentityError, Result};
pub use file::{
    IdentityFile, IdentityPayload, create_identity, migrate_v1_to_v2, recover_identity,
    sanitize_profile, unlock_identity, update_profile, validate_avatar, validate_nickname,
};
pub use mnemonic::{Wordlist, find_invalid_mnemonic_words, generate_mnemonic, parse_mnemonic};
pub use slip10::{Slip10Node, format_derivation_path, parse_derivation_path};

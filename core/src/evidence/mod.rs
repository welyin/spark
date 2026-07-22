//! 存证模块：canonical JSON（normalizeObject）与哈希链。
//!
//! 算法精确规格见 `core/spec/sync-evidence.md` §1-§2，验收向量见
//! `core/spec/vectors/sync-evidence.json`。

pub mod canonical;
pub mod chain;

pub use canonical::{js_number_to_string, normalize_object, normalize_value};
pub use chain::{
    EVIDENCE_HEAD_KEY, EVIDENCE_PREFIX, EvidenceEntry, EvidenceError, EvidenceHead,
    EvidenceOp, NewEvidenceEntry, Result, append_evidence, build_evidence_data_hash,
    build_evidence_entry_hash, build_evidence_meta_hash, build_evidence_payload_hash,
    build_next_evidence_entry, evidence_batch_operations, evidence_key, get_evidence_entry,
    get_evidence_head, get_evidence_head_hash, get_evidence_height, sha256_hex,
    verify_evidence_chain, verify_evidence_hash_matches_remote,
};

//! 存证模块：canonical JSON（normalizeObject）与哈希链。
//!
//! 算法精确规格见 `core/spec/sync-evidence.md` §1-§2，验收向量见
//! `core/spec/vectors/sync-evidence.json`；锚定/导出（§6–§9）向量见
//! `core/spec/vectors/evidence-anchor.json`。

pub mod anchor;
pub mod canonical;
pub mod chain;
pub mod export;
pub mod report;

pub use anchor::{
    AnchorFork, AnchorRecord, EVIDENCE_ANCHOR_PREFIX, EVIDENCE_FORK_PREFIX, anchor_key,
    anchor_leaf, anchor_root, anchor_sign_payload, detect_anchor_fork, fork_archive_key,
    fork_archive_value, inclusion_proof, sign_anchor, verify_anchor, verify_inclusion,
};
pub use canonical::{js_number_to_string, normalize_object, normalize_value};
pub use chain::{
    EVIDENCE_HEAD_KEY, EVIDENCE_PREFIX, EvidenceEntry, EvidenceError, EvidenceHead, EvidenceOp,
    NewEvidenceEntry, Result, append_evidence, build_evidence_data_hash, build_evidence_entry_hash,
    build_evidence_meta_hash, build_evidence_payload_hash, build_next_evidence_entry,
    evidence_batch_operations, evidence_key, get_evidence_entry, get_evidence_head,
    get_evidence_head_hash, get_evidence_height, sha256_hex, verify_evidence_chain,
    verify_evidence_hash_matches_remote,
};
pub use export::{
    BUSINESS_LAYER_NOTE, EvidenceExportPackage, ExportScope, MembershipOutcome, ROSTER_ENTRY_COLLECTION,
    RosterAnchorRef, RosterMember, RosterSection, RosterSummary, SignerCheck, VerifyReport,
    build_export_package, collect_anchors, export_sign_payload, roster_commitment_payload,
    verify_export_package,
};
pub use report::{format_utc, render_printable_report};

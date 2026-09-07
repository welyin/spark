//! 节点锚记录 + 组织锚根（默克尔树）+ 分叉检测（sync-evidence §6–§8）。
//!
//! - 锚记录：每节点一条 LWW 自覆盖，声明本地链头承诺（`org:evi:anchor:
//!   {orgId}:{nodeId}`，orgsync `org:structure@v1` 键域）；
//! - 组织锚根：纯派生不落库，任一节点对「当前已知全部成员锚记录」算默克尔
//!   根 + inclusion proof（§7 域分隔/排序/奇数叶复制规则）；
//! - 分叉检测：同 nodeId 回退（headSeq 变小）或同 seq 异 hash → 可举证
//!   告警（§8；检测分歧、不做裁决）。
//!
//! 验签材料内嵌偏差（待协议守护会签补记 §6）：§6 字段表只有
//! `anchorV/orgId/nodeId/headSeq/headHash/ts/sig`，但 sig 的验签键是 root
//! 身份公钥，而 rootId = sha256hex(公钥) 不可逆推——锚记录不带公钥则任何
//! 消费方（含导出包独立核验方）都无法验签。按 `org/org_address` 地址记录
//! 既有模式内嵌 `rootId` + `publicKey`（b64），两字段**入签名载荷**（防
//! 公钥替换）。golden vectors 以此线形为验收权威。

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use ed25519_dalek::{Signer as _, Verifier as _};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::canonical::normalize_object;

/// 锚记录存储前缀（orgsync `org:structure@v1` 键域，all-members）。
pub const EVIDENCE_ANCHOR_PREFIX: &str = "org:evi:anchor:";
/// 分叉证据留档前缀（本地键，不进同步流量——§8 证据保全在覆盖前完成，
/// 留档本身无需传播）。
pub const EVIDENCE_FORK_PREFIX: &str = "org:evi:fork:";

/// 锚记录存储键 `org:evi:anchor:{orgId}:{nodeId}`。
pub fn anchor_key(org_id: &str, node_id: &str) -> String {
    format!("{EVIDENCE_ANCHOR_PREFIX}{org_id}:{node_id}")
}

/// 分叉证据留档键 `org:evi:fork:{orgId}:{nodeId}:{ts}`（同节点多次分叉各
/// 留一档）。
pub fn fork_archive_key(org_id: &str, node_id: &str, ts: i64) -> String {
    format!("{EVIDENCE_FORK_PREFIX}{org_id}:{node_id}:{ts}")
}

/// 节点锚记录（§6 线形 + 内嵌验签材料偏差，见模块头注）。
///
/// serde 字段名 camelCase；`sig` = root 身份 Ed25519 签名（b64 64B），载荷
/// = 其余全部字段的 canonical JSON（§1 normalizeObject 规则）UTF-8 字节。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnchorRecord {
    /// 版本字段（恒 1）。
    pub anchor_v: u32,
    /// `org_<16hex>`。
    pub org_id: String,
    /// 设备 peerId（与 vv 分量键同口径）。
    pub node_id: String,
    /// 签名方 root 身份 id（= sha256hex(publicKey)）。
    pub root_id: String,
    /// 签名方 root 身份公钥（b64 32B，内嵌验签材料）。
    pub public_key: String,
    /// 声明时刻本地链头 seq。
    pub head_seq: u64,
    /// 声明时刻本地链头 hash（sha256hex）。
    pub head_hash: String,
    /// 声明方本地 Unix 毫秒。
    pub ts: i64,
    /// root 身份签名（b64 64B）。
    pub sig: String,
}

/// 签名载荷对象（剔除 `sig` 的全部字段；normalize_object 内部排序，键序
/// 在此无关）。
pub fn anchor_sign_value(record: &AnchorRecord) -> Value {
    serde_json::json!({
        "anchorV": record.anchor_v,
        "orgId": record.org_id,
        "nodeId": record.node_id,
        "rootId": record.root_id,
        "publicKey": record.public_key,
        "headSeq": record.head_seq,
        "headHash": record.head_hash,
        "ts": record.ts,
    })
}

/// 签名载荷 = canonical（剔除 sig 的全部字段）的 UTF-8 字节串（§1 规则）。
pub fn anchor_sign_payload(record: &AnchorRecord) -> String {
    normalize_object(&anchor_sign_value(record))
}

/// 构造并签名锚记录（root 身份签名钥；rootId/publicKey 自签名钥推导）。
pub fn sign_anchor(
    root_signing_key: &ed25519_dalek::SigningKey,
    org_id: &str,
    node_id: &str,
    head_seq: u64,
    head_hash: &str,
    ts: i64,
) -> AnchorRecord {
    let pk_bytes = root_signing_key.verifying_key().to_bytes();
    let mut record = AnchorRecord {
        anchor_v: 1,
        org_id: org_id.to_string(),
        node_id: node_id.to_string(),
        root_id: hex::encode(Sha256::digest(pk_bytes)),
        public_key: B64.encode(pk_bytes),
        head_seq,
        head_hash: head_hash.to_string(),
        ts,
        sig: String::new(),
    };
    let payload = anchor_sign_payload(&record);
    record.sig = B64.encode(root_signing_key.sign(payload.as_bytes()).to_bytes());
    record
}

/// 锚记录验签：内嵌 publicKey 与 rootId 绑定（sha256hex）+ Ed25519 验签。
/// 任一字段篡改 → false。
pub fn verify_anchor(record: &AnchorRecord) -> bool {
    let Ok(pk_raw) = B64.decode(&record.public_key) else {
        return false;
    };
    let Ok(pk_arr) = <[u8; 32]>::try_from(pk_raw.as_slice()) else {
        return false;
    };
    if hex::encode(Sha256::digest(pk_arr)) != record.root_id {
        return false;
    }
    let Ok(pk) = ed25519_dalek::VerifyingKey::from_bytes(&pk_arr) else {
        return false;
    };
    let Ok(sig_raw) = B64.decode(&record.sig) else {
        return false;
    };
    let Ok(sig_arr) = <[u8; 64]>::try_from(sig_raw.as_slice()) else {
        return false;
    };
    pk.verify(
        anchor_sign_payload(record).as_bytes(),
        &ed25519_dalek::Signature::from_bytes(&sig_arr),
    )
    .is_ok()
}

// ---------------------------------------------------------------------------
// 组织锚根（§7 默克尔树）
// ---------------------------------------------------------------------------

/// 叶域分隔（§7）：`sha256(utf8("evi-anchor-leaf\x00") ‖ canonical(锚记录全文含 sig))`。
const LEAF_DOMAIN: &[u8] = b"evi-anchor-leaf\0";
/// 内部节点域分隔：`sha256(utf8("evi-anchor-node\x00") ‖ left ‖ right)`
/// （left/right 为 32B 原始摘要）。
const NODE_DOMAIN: &[u8] = b"evi-anchor-node\0";

fn sha256_raw(parts: &[&[u8]]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part);
    }
    hasher.finalize().into()
}

/// 锚记录叶哈希。
pub fn anchor_leaf(record: &AnchorRecord) -> [u8; 32] {
    let value = serde_json::to_value(record).unwrap_or(Value::Null);
    let canonical = normalize_object(&value);
    sha256_raw(&[LEAF_DOMAIN, canonical.as_bytes()])
}

/// 叶排序：按 nodeId **UTF-8 字节序**（Rust `str` 序即字节序）。
fn sorted_records(records: &[AnchorRecord]) -> Vec<&AnchorRecord> {
    let mut sorted: Vec<&AnchorRecord> = records.iter().collect();
    sorted.sort_by(|a, b| a.node_id.as_bytes().cmp(b.node_id.as_bytes()));
    sorted
}

/// 逐层构建（除根层外奇数层末叶复制补齐；返回 levels[0]=叶 … 末层=根）。
fn build_levels(leaves: &[[u8; 32]]) -> Vec<Vec<[u8; 32]>> {
    let mut levels = vec![leaves.to_vec()];
    while let Some(last) = levels.last()
        && last.len() > 1
    {
        let mut cur = last.clone();
        if cur.len() % 2 == 1 {
            cur.push(*cur.last().expect("non-empty level"));
        }
        let mut next = Vec::with_capacity(cur.len() / 2);
        for pair in cur.chunks_exact(2) {
            next.push(sha256_raw(&[NODE_DOMAIN, &pair[0], &pair[1]]));
        }
        levels.push(next);
    }
    levels
}

/// 组织锚根（hex 64 字符；空集合 → None）。单叶：root = 该叶。
pub fn anchor_root(records: &[AnchorRecord]) -> Option<String> {
    if records.is_empty() {
        return None;
    }
    let leaves: Vec<[u8; 32]> = sorted_records(records)
        .iter()
        .map(|r| anchor_leaf(r))
        .collect();
    let levels = build_levels(&leaves);
    let root = levels.last().and_then(|l| l.first())?;
    Some(hex::encode(root))
}

/// inclusion proof（§7）：nodeId → 兄弟哈希路径，**自叶向根**逐层 hex；
/// 左右位置不随证明携带（核验方持全部锚记录可推导 idx）。单叶 → 空证明。
pub fn inclusion_proof(records: &[AnchorRecord], node_id: &str) -> Option<Vec<String>> {
    let sorted = sorted_records(records);
    let mut idx = sorted.iter().position(|r| r.node_id == node_id)?;
    let leaves: Vec<[u8; 32]> = sorted.iter().map(|r| anchor_leaf(r)).collect();
    let levels = build_levels(&leaves);
    let mut proof = Vec::new();
    for level in &levels[..levels.len().saturating_sub(1)] {
        // 层内奇数复制已并入 levels：偶 idx 兄弟在右（idx+1，末叶复制即自身
        // 副本同 hash），奇 idx 兄弟在左（idx-1）
        let sibling = idx ^ 1;
        proof.push(hex::encode(level[sibling.min(level.len() - 1)]));
        idx /= 2;
    }
    Some(proof)
}

/// inclusion proof 验证（§7）：从叶重算至根与 `root_hex` 比对。
/// `records` = 核验方所持全部锚记录（用于推导叶序/idx）；`node_id` 对应的
/// 锚记录须在集合内。证明长度须与树深一致（防截断/加长）。
pub fn verify_inclusion(
    records: &[AnchorRecord],
    node_id: &str,
    proof: &[String],
    root_hex: &str,
) -> bool {
    let sorted = sorted_records(records);
    let Some(mut idx) = sorted.iter().position(|r| r.node_id == node_id) else {
        return false;
    };
    let leaves: Vec<[u8; 32]> = sorted.iter().map(|r| anchor_leaf(r)).collect();
    let levels = build_levels(&leaves);
    if proof.len() != levels.len() - 1 {
        return false;
    }
    let mut cur = anchor_leaf(sorted[idx]);
    for (level, sibling_hex) in levels[..levels.len() - 1].iter().zip(proof.iter()) {
        let Ok(sibling) = hex::decode(sibling_hex) else {
            return false;
        };
        let Ok(sib_arr) = <[u8; 32]>::try_from(sibling.as_slice()) else {
            return false;
        };
        // 层尺寸一致性（证明随树形重放；防错层嫁接）
        if idx >= level.len() {
            return false;
        }
        cur = if idx % 2 == 0 {
            sha256_raw(&[NODE_DOMAIN, &cur, &sib_arr])
        } else {
            sha256_raw(&[NODE_DOMAIN, &sib_arr, &cur])
        };
        idx /= 2;
    }
    hex::encode(cur) == root_hex
}

// ---------------------------------------------------------------------------
// 分叉检测（§8）
// ---------------------------------------------------------------------------

/// 分叉证据类别（同 nodeId 的先后两份签名锚）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AnchorFork {
    /// headSeq 回退（后锚 < 先锚）。
    SeqRegression,
    /// 同 headSeq 不同 headHash。
    SameSeqHashMismatch,
}

impl AnchorFork {
    /// 人读标签（告警/留档用）。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SeqRegression => "seq-regression",
            Self::SameSeqHashMismatch => "same-seq-hash-mismatch",
        }
    }
}

/// 分叉判定（§8 表）：仅同 orgId 同 nodeId 的两份锚可比较。
/// 锚旧于本地链头/锚 seq 大于本地链高不属本函数职责（那两态是「锚 vs
/// 本地链」判定，这里是「锚 vs 锚」判定）。
pub fn detect_anchor_fork(local: &AnchorRecord, incoming: &AnchorRecord) -> Option<AnchorFork> {
    if local.org_id != incoming.org_id || local.node_id != incoming.node_id {
        return None;
    }
    if incoming.head_seq < local.head_seq {
        return Some(AnchorFork::SeqRegression);
    }
    if incoming.head_seq == local.head_seq && incoming.head_hash != local.head_hash {
        return Some(AnchorFork::SameSeqHashMismatch);
    }
    None
}

/// 分叉证据留档负载（§8：告警负载携带双份签名锚全文）。
pub fn fork_archive_value(
    local: &AnchorRecord,
    incoming: &AnchorRecord,
    kind: AnchorFork,
    detected_ts: i64,
) -> Value {
    serde_json::json!({
        "kind": kind.as_str(),
        "detectedTs": detected_ts,
        "local": serde_json::to_value(local).unwrap_or(Value::Null),
        "incoming": serde_json::to_value(incoming).unwrap_or(Value::Null),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keypair(b: u8) -> ed25519_dalek::SigningKey {
        ed25519_dalek::SigningKey::from_bytes(&[b; 32])
    }

    fn anchor(b: u8, node: &str, seq: u64, hash: &str) -> AnchorRecord {
        sign_anchor(
            &keypair(b),
            "org_0000000000000001",
            node,
            seq,
            hash,
            1_720_000_000_000,
        )
    }

    #[test]
    fn sign_verify_roundtrip_and_tamper() {
        let rec = anchor(0x11, "nodeA", 7, &"ab".repeat(32));
        assert!(verify_anchor(&rec));
        for tampered in [
            AnchorRecord {
                head_seq: 8,
                ..rec.clone()
            },
            AnchorRecord {
                head_hash: "cd".repeat(32),
                ..rec.clone()
            },
            AnchorRecord {
                node_id: "nodeB".to_string(),
                ..rec.clone()
            },
            AnchorRecord {
                ts: rec.ts + 1,
                ..rec.clone()
            },
        ] {
            assert!(!verify_anchor(&tampered), "篡改字段验签必败");
        }
        // 公钥替换（含 rootId 同步替换为攻击者值——rootId/publicKey 在载荷内，
        // 换钥即签名失效）
        let evil = keypair(0x99);
        let mut swapped = rec.clone();
        swapped.public_key = B64.encode(evil.verifying_key().to_bytes());
        assert!(!verify_anchor(&swapped));
    }

    #[test]
    fn merkle_root_and_proofs() {
        let a = anchor(0x11, "nodeA", 7, &"ab".repeat(32));
        let b = anchor(0x22, "nodeB", 9, &"cd".repeat(32));
        let c = anchor(0x33, "nodeC", 3, &"ef".repeat(32));

        // 单叶：root = 叶，空证明
        let root1 = anchor_root(std::slice::from_ref(&a)).unwrap();
        assert_eq!(root1, hex::encode(anchor_leaf(&a)));
        let p1 = inclusion_proof(std::slice::from_ref(&a), "nodeA").unwrap();
        assert!(p1.is_empty());
        assert!(verify_inclusion(
            std::slice::from_ref(&a),
            "nodeA",
            &p1,
            &root1
        ));

        // 偶数叶
        let two = vec![a.clone(), b.clone()];
        let root2 = anchor_root(&two).unwrap();
        for node in ["nodeA", "nodeB"] {
            let p = inclusion_proof(&two, node).unwrap();
            assert_eq!(p.len(), 1);
            assert!(verify_inclusion(&two, node, &p, &root2), "{node} 证明通过");
        }

        // 奇数叶（末叶复制）
        let three = vec![a.clone(), b.clone(), c.clone()];
        let root3 = anchor_root(&three).unwrap();
        for node in ["nodeA", "nodeB", "nodeC"] {
            let p = inclusion_proof(&three, node).unwrap();
            assert_eq!(p.len(), 2);
            assert!(
                verify_inclusion(&three, node, &p, &root3),
                "{node} 证明通过"
            );
        }

        // 篡改 sibling / 换叶 / 错根 → 失败
        let p = inclusion_proof(&three, "nodeC").unwrap();
        let mut bad = p.clone();
        bad[0] = "00".repeat(32);
        assert!(!verify_inclusion(&three, "nodeC", &bad, &root3));
        assert!(!verify_inclusion(&three, "nodeC", &p, &root2), "错根必败");
        // 换叶索引：用 nodeA 的证明验 nodeC
        let pa = inclusion_proof(&three, "nodeA").unwrap();
        assert!(!verify_inclusion(&three, "nodeC", &pa, &root3));
        // 证明长度不符
        assert!(!verify_inclusion(&three, "nodeC", &p[..1], &root3));
    }

    #[test]
    fn fork_detection_table() {
        let base = anchor(0x11, "nodeA", 7, &"ab".repeat(32));
        // 回退
        let regress = anchor(0x11, "nodeA", 5, &"ab".repeat(32));
        assert_eq!(
            detect_anchor_fork(&base, &regress),
            Some(AnchorFork::SeqRegression)
        );
        // 同 seq 异 hash
        let conflict = anchor(0x11, "nodeA", 7, &"cd".repeat(32));
        assert_eq!(
            detect_anchor_fork(&base, &conflict),
            Some(AnchorFork::SameSeqHashMismatch)
        );
        // 正常前进 / 相同 / 不同节点
        let forward = anchor(0x11, "nodeA", 9, &"ab".repeat(32));
        assert_eq!(detect_anchor_fork(&base, &forward), None);
        assert_eq!(detect_anchor_fork(&base, &base.clone()), None);
        let other = anchor(0x22, "nodeB", 1, &"00".repeat(32));
        assert_eq!(detect_anchor_fork(&base, &other), None);
    }
}

//! 存证导出包与五步核验（sync-evidence §9；设计 §2/§3）。
//!
//! 导出包 = 自包含单文件 JSON：全量连续链（entries 不按 scope 过滤——链式
//! 校验以连续性为前提，scope 是声明性关注范围）+ 锚集 + 默克尔根 +
//! inclusion proofs + 导出者签名 + 规格指针。核验方零外部依赖。
//!
//! 验签材料内嵌偏差（与 anchor.rs 同源，待协议守护会签）：`exporter` 段
//! 在 §9 示例的 `{rootId, ts, sig}` 上增 `publicKey`（b64）——rootId =
//! sha256hex(公钥) 不可逆推，不内嵌公钥则导出者签名无法离线核验。该字段
//! 入签名载荷（防替换）。
//!
//! 红线 5：本文件与核验 CLI 共用同一份 `canonical`/`chain`/`anchor` 实现，
//! 禁止在工具侧重写序列化。

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use ed25519_dalek::{Signer as _, Verifier as _};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use super::anchor::{
    AnchorRecord, EVIDENCE_ANCHOR_PREFIX, anchor_root, inclusion_proof, verify_anchor,
    verify_inclusion,
};
use super::canonical::normalize_object;
use super::chain::{
    EvidenceEntry, build_evidence_entry_hash, get_evidence_entry, get_evidence_head,
};
use crate::storage::{ScanOptions, StorageBackend};

/// spec 自描述指针（§9 恒定值）。
pub const SPEC_REF: &str = "sync-evidence §1–§2、§6–§9";
/// canonicalJson 规则人读摘要（§9）。
pub const CANONICAL_JSON_SUMMARY: &str =
    "normalizeObject：key 排序（整数型 key 数值升序在前，其余 UTF-16 字典序），\
     嵌套对象递归 normalize 后作为 JSON 字符串值嵌入；数字按 JS Number::toString，\
     字符串按 JS JSON 转义";

/// 导出范围（§9：domain/collection/orgId 各可省，全省 = 全链导出；scope 是
/// 声明性关注范围，不是 entries 过滤条件）。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportScope {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub org_id: Option<String>,
}

/// 导出者段（`sig` = root 身份签名；`publicKey` 为内嵌验签材料偏差）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Exporter {
    pub root_id: String,
    pub public_key: String,
    pub ts: i64,
    pub sig: String,
}

/// 链头引用 `{seq, hash}`。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportHead {
    pub seq: u64,
    pub hash: String,
}

/// 规格自描述指针段。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpecRef {
    pub canonical_json: String,
    pub spec_ref: String,
}

/// 存证导出包（§9 线形；serde 字段序 = 文档列举序）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceExportPackage {
    pub format_version: u32,
    pub scope: ExportScope,
    pub exporter: Exporter,
    pub head: ExportHead,
    /// 全量连续链 `seq = 1..=head.seq`（§2 条目线形原样）。
    pub entries: Vec<EvidenceEntry>,
    /// 导出时刻已知的成员锚记录（scope.orgId 声明时收窄到该组织）。
    pub anchors: Vec<AnchorRecord>,
    /// 默克尔根（无锚 → null）。
    pub anchor_root: Option<String>,
    /// nodeId → 兄弟哈希路径（自叶向根）。
    pub anchor_proofs: Map<String, Value>,
    pub spec: SpecRef,
}

/// 签名材料：包全文 Value 剔除 `exporter.sig` 字段（§9）。
pub fn export_sign_value(package: &EvidenceExportPackage) -> Value {
    let mut value = serde_json::to_value(package).unwrap_or(Value::Null);
    if let Some(exporter) = value.get_mut("exporter")
        && let Some(obj) = exporter.as_object_mut()
    {
        obj.remove("sig");
    }
    value
}

/// 导出者签名载荷 = canonical（包全文剔除 exporter.sig）UTF-8 字节串。
pub fn export_sign_payload(package: &EvidenceExportPackage) -> String {
    normalize_object(&export_sign_value(package))
}

/// 收集锚记录：scope.orgId 声明时收窄到该组织，否则全部组织。
pub fn collect_anchors<S: StorageBackend>(
    storage: &S,
    org_id: Option<&str>,
) -> Result<Vec<AnchorRecord>, String> {
    let prefix = match org_id {
        Some(org) => format!("{EVIDENCE_ANCHOR_PREFIX}{org}:"),
        None => EVIDENCE_ANCHOR_PREFIX.to_string(),
    };
    let mut anchors = Vec::new();
    for (_, raw) in storage
        .scan(&ScanOptions::prefix(prefix))
        .map_err(|e| e.to_string())?
    {
        if let Ok(anchor) = serde_json::from_str::<AnchorRecord>(&raw) {
            anchors.push(anchor);
        }
    }
    anchors.sort_by(|a, b| a.node_id.cmp(&b.node_id));
    Ok(anchors)
}

/// 构建导出包（§9）：全量连续链 + 锚集 + 根 + proofs + 导出者签名。
/// 链缺失中间条目 / 空链 → Err。
pub fn build_export_package<S: StorageBackend>(
    storage: &S,
    scope: ExportScope,
    exporter_signing_key: &ed25519_dalek::SigningKey,
    ts: i64,
) -> Result<EvidenceExportPackage, String> {
    let head = get_evidence_head(storage)
        .map_err(|e| e.to_string())?
        .ok_or("空链无可导出存证")?;
    let mut entries = Vec::with_capacity(head.seq as usize);
    for seq in 1..=head.seq {
        let entry = get_evidence_entry(storage, seq)
            .map_err(|e| e.to_string())?
            .ok_or(format!("链不完整：缺 seq={seq}"))?;
        entries.push(entry);
    }
    let anchors = collect_anchors(storage, scope.org_id.as_deref())?;
    let root = anchor_root(&anchors);
    let mut proofs = Map::new();
    for anchor in &anchors {
        if let Some(proof) = inclusion_proof(&anchors, &anchor.node_id) {
            proofs.insert(
                anchor.node_id.clone(),
                Value::Array(proof.into_iter().map(Value::String).collect()),
            );
        }
    }
    let pk_bytes = exporter_signing_key.verifying_key().to_bytes();
    let mut package = EvidenceExportPackage {
        format_version: 1,
        scope,
        exporter: Exporter {
            root_id: hex::encode(Sha256::digest(pk_bytes)),
            public_key: B64.encode(pk_bytes),
            ts,
            sig: String::new(),
        },
        head: ExportHead {
            seq: head.seq,
            hash: head.hash,
        },
        entries,
        anchors,
        anchor_root: root,
        anchor_proofs: proofs,
        spec: SpecRef {
            canonical_json: CANONICAL_JSON_SUMMARY.to_string(),
            spec_ref: SPEC_REF.to_string(),
        },
    };
    let payload = export_sign_payload(&package);
    package.exporter.sig = B64.encode(
        exporter_signing_key
            .sign(payload.as_bytes())
            .to_bytes(),
    );
    Ok(package)
}

// ---------------------------------------------------------------------------
// 五步核验（设计 §3；CLI 与测试共用）
// ---------------------------------------------------------------------------

/// 核验报告（人读 + 机器判定两用）。
#[derive(Clone, Debug, Default)]
pub struct VerifyReport {
    /// 各步失败原因（step 标签前缀）；空 = 全部通过。
    pub failures: Vec<String>,
    /// 链高（核验通过语义下 = head.seq）。
    pub height: u64,
    /// 锚点数。
    pub anchor_count: usize,
    /// 各成员链头（nodeId → (headSeq, headHash)）。
    pub member_heads: Vec<(String, u64, String)>,
    /// 导出者 rootId。
    pub exporter_root_id: String,
}

impl VerifyReport {
    pub fn ok(&self) -> bool {
        self.failures.is_empty()
    }
}

fn parse_b64_32(raw: &str) -> Option<[u8; 32]> {
    let bytes = B64.decode(raw).ok()?;
    <[u8; 32]>::try_from(bytes.as_slice()).ok()
}

/// 五步核验（§3）：① 导出者签名 → ② 链式校验（连续 + prevHash + 重算
/// hash + head 吻合）→ ③ 锚验签 + 重算 anchorRoot + inclusion proofs →
/// ④ scope 覆盖性断言 → ⑤ 报告（调用方决定输出/退出码）。
///
/// 输入为包 JSON 文本；解析失败即单条失败报告。所有哈希经 `canonical`
/// 复算——禁止重写序列化（红线 5）。
pub fn verify_export_package(raw: &str) -> VerifyReport {
    let mut report = VerifyReport::default();
    let package: EvidenceExportPackage = match serde_json::from_str(raw) {
        Ok(p) => p,
        Err(e) => {
            report.failures.push(format!("parse: 包 JSON 解析失败: {e}"));
            return report;
        }
    };
    if package.format_version != 1 {
        report
            .failures
            .push(format!("format: 未知 formatVersion {}", package.format_version));
    }
    report.height = package.head.seq;
    report.anchor_count = package.anchors.len();
    report.exporter_root_id = package.exporter.root_id.clone();

    // ① 导出者签名（publicKey ⟹ rootId 绑定 + Ed25519 验签）
    let step1 = (|| -> Result<(), String> {
        let pk_arr = parse_b64_32(&package.exporter.public_key)
            .ok_or("exporter.publicKey 形状非法")?;
        if hex::encode(Sha256::digest(pk_arr)) != package.exporter.root_id {
            return Err("exporter.publicKey 与 rootId 不绑定".to_string());
        }
        let pk = ed25519_dalek::VerifyingKey::from_bytes(&pk_arr)
            .map_err(|e| format!("exporter 公钥非法: {e}"))?;
        let sig_raw = B64
            .decode(&package.exporter.sig)
            .map_err(|e| format!("exporter.sig 非 b64: {e}"))?;
        let sig_arr = <[u8; 64]>::try_from(sig_raw.as_slice())
            .map_err(|_| "exporter.sig 非 64B".to_string())?;
        let payload = export_sign_payload(&package);
        pk.verify(
            payload.as_bytes(),
            &ed25519_dalek::Signature::from_bytes(&sig_arr),
        )
        .map_err(|_| "exporter.sig 验签失败".to_string())
    })();
    if let Err(reason) = step1 {
        report.failures.push(format!("① 导出者签名: {reason}"));
    }

    // ② 链式校验：entries 恰为 seq 1..=head.seq 连续，prevHash 链接，逐条
    // 重算 hash，末条 = head
    let step2 = (|| -> Result<(), String> {
        if package.entries.len() as u64 != package.head.seq {
            return Err(format!(
                "entries 条数 {} ≠ head.seq {}",
                package.entries.len(),
                package.head.seq
            ));
        }
        let mut prev_hash: Option<String> = None;
        for (i, entry) in package.entries.iter().enumerate() {
            let expect_seq = (i + 1) as u64;
            if entry.seq != expect_seq {
                return Err(format!("断链：seq {} 处实为 {}", expect_seq, entry.seq));
            }
            if entry.prev_hash != prev_hash {
                return Err(format!("prevHash 断链：seq {}", entry.seq));
            }
            if entry.hash != build_evidence_entry_hash(entry) {
                return Err(format!("条目 hash 重算不符：seq {}", entry.seq));
            }
            prev_hash = Some(entry.hash.clone());
        }
        if prev_hash.as_deref() != Some(package.head.hash.as_str()) {
            return Err("链末条目 hash ≠ head.hash".to_string());
        }
        Ok(())
    })();
    if let Err(reason) = step2 {
        report.failures.push(format!("② 链式校验: {reason}"));
    }

    // ③ 锚：逐条验签 + 重算 anchorRoot + 逐条 inclusion proof
    let step3 = (|| -> Result<(), String> {
        for anchor in &package.anchors {
            if !verify_anchor(anchor) {
                return Err(format!("锚验签失败：nodeId {}", anchor.node_id));
            }
            report.member_heads.push((
                anchor.node_id.clone(),
                anchor.head_seq,
                anchor.head_hash.clone(),
            ));
        }
        let recomputed = anchor_root(&package.anchors);
        if recomputed != package.anchor_root {
            return Err("anchorRoot 重算不符".to_string());
        }
        for anchor in &package.anchors {
            let proof_value = package
                .anchor_proofs
                .get(&anchor.node_id)
                .ok_or(format!("缺 {} 的 inclusion proof", anchor.node_id))?;
            let proof: Vec<String> = serde_json::from_value(proof_value.clone())
                .map_err(|e| format!("{} 的 proof 形状非法: {e}", anchor.node_id))?;
            let root = package.anchor_root.as_deref().unwrap_or_default();
            if !verify_inclusion(&package.anchors, &anchor.node_id, &proof, root) {
                return Err(format!("{} 的 inclusion proof 验证失败", anchor.node_id));
            }
        }
        if package.anchors.is_empty() && !package.anchor_proofs.is_empty() {
            return Err("无锚却携带 proofs".to_string());
        }
        Ok(())
    })();
    if let Err(reason) = step3 {
        report.failures.push(format!("③ 锚与默克尔根: {reason}"));
    }

    // ④ scope 覆盖性断言：声明的 domain/collection 在链内有条目命中；
    // orgId 声明时锚集须全部属于该组织
    let step4 = (|| -> Result<(), String> {
        let scope = &package.scope;
        if scope.domain.is_some() || scope.collection.is_some() {
            let hit = package.entries.iter().any(|e| {
                scope.domain.as_deref().is_none_or(|d| e.domain == d)
                    && scope
                        .collection
                        .as_deref()
                        .is_none_or(|c| e.collection == c)
            });
            if !hit {
                return Err(format!(
                    "scope 声明 {:?}/{:?} 在链内无条目",
                    scope.domain, scope.collection
                ));
            }
        }
        if let Some(org) = &scope.org_id
            && package.anchors.iter().any(|a| &a.org_id != org)
        {
            return Err(format!("锚集含 scope 外组织（期望 {org}）"));
        }
        Ok(())
    })();
    if let Err(reason) = step4 {
        report.failures.push(format!("④ scope 覆盖: {reason}"));
    }

    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evidence::{EvidenceOp, NewEvidenceEntry, append_evidence};
    use crate::storage::MemoryStorage;

    fn keypair(b: u8) -> ed25519_dalek::SigningKey {
        ed25519_dalek::SigningKey::from_bytes(&[b; 32])
    }

    fn sample_storage() -> (MemoryStorage, EvidenceEntry) {
        let mut s = MemoryStorage::new();
        let mut last = None;
        for i in 1..=3u64 {
            let entry = append_evidence(
                &mut s,
                NewEvidenceEntry::from_parts(
                    "plugin:vote",
                    "ballots",
                    format!("b{i}"),
                    EvidenceOp::Put,
                    Some(&serde_json::json!({"choice": i})),
                    None,
                    1_720_000_000_000 + i as i64,
                    "nodeA",
                ),
            )
            .unwrap();
            last = Some(entry);
        }
        (s, last.unwrap())
    }

    fn build_package(
        s: &MemoryStorage,
        anchors: &[AnchorRecord],
        org: Option<&str>,
    ) -> EvidenceExportPackage {
        let mut raw = MemoryStorage::new();
        raw.clone_from(s);
        for a in anchors {
            raw.put(
                &super::super::anchor::anchor_key(&a.org_id, &a.node_id),
                &serde_json::to_string(a).unwrap(),
            )
            .unwrap();
        }
        build_export_package(
            &raw,
            ExportScope {
                domain: Some("plugin:vote".to_string()),
                collection: Some("ballots".to_string()),
                org_id: org.map(str::to_string),
            },
            &keypair(0xEE),
            1_720_000_000_500,
        )
        .unwrap()
    }

    #[test]
    fn package_build_and_verify_roundtrip() {
        let (s, _last) = sample_storage();
        let a = super::super::anchor::sign_anchor(
            &keypair(0x11),
            "org_0000000000000001",
            "nodeA",
            3,
            &"ab".repeat(32),
            1_720_000_000_100,
        );
        let b = super::super::anchor::sign_anchor(
            &keypair(0x22),
            "org_0000000000000001",
            "nodeB",
            5,
            &"cd".repeat(32),
            1_720_000_000_200,
        );
        let pkg = build_package(&s, &[a, b], Some("org_0000000000000001"));
        let raw = serde_json::to_string_pretty(&pkg).unwrap();
        let report = verify_export_package(&raw);
        assert!(report.ok(), "failures: {:?}", report.failures);
        assert_eq!(report.height, 3);
        assert_eq!(report.anchor_count, 2);
        assert_eq!(report.member_heads.len(), 2);
    }

    #[test]
    fn tamper_each_part_fails() {
        let (s, _last) = sample_storage();
        let pkg = build_package(&s, &[], None);
        let ok = verify_export_package(&serde_json::to_string(&pkg).unwrap());
        assert!(ok.ok(), "baseline: {:?}", ok.failures);

        // 篡改条目
        let mut p1 = pkg.clone();
        p1.entries[1].id = "forged".to_string();
        let r = verify_export_package(&serde_json::to_string(&p1).unwrap());
        assert!(!r.ok());
        assert!(r.failures.iter().any(|f| f.contains('②')), "{:?}", r.failures);
        // ① 也应失败（签名覆盖全文）
        assert!(r.failures.iter().any(|f| f.contains('①')), "{:?}", r.failures);

        // 篡改 head
        let mut p2 = pkg.clone();
        p2.head.hash = "00".repeat(32);
        let r = verify_export_package(&serde_json::to_string(&p2).unwrap());
        assert!(!r.ok());

        // 断链（删中间条目）
        let mut p3 = pkg.clone();
        p3.entries.remove(1);
        let r = verify_export_package(&serde_json::to_string(&p3).unwrap());
        assert!(!r.ok());
        assert!(r.failures.iter().any(|f| f.contains('②')));

        // 篡改签名
        let mut p4 = pkg.clone();
        p4.exporter.sig = B64.encode([0x42; 64]);
        let r = verify_export_package(&serde_json::to_string(&p4).unwrap());
        assert!(!r.ok());
        assert!(r.failures.iter().any(|f| f.contains('①')));
    }

    #[test]
    fn scope_coverage_assertion() {
        let (s, _last) = sample_storage();
        let mut raw = MemoryStorage::new();
        raw.clone_from(&s);
        let pkg = build_export_package(
            &raw,
            ExportScope {
                domain: Some("plugin:other".to_string()),
                collection: None,
                org_id: None,
            },
            &keypair(0xEE),
            1_720_000_000_500,
        )
        .unwrap();
        let r = verify_export_package(&serde_json::to_string(&pkg).unwrap());
        assert!(!r.ok());
        assert!(r.failures.iter().any(|f| f.contains('④')), "{:?}", r.failures);
    }
}

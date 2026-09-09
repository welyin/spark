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
pub const CANONICAL_JSON_SUMMARY: &str = "normalizeObject：key 排序（整数型 key 数值升序在前，其余 UTF-16 字典序），\
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

/// roster 段锚引用 `{orgId, anchorRoot, ts}`（ts = 覆盖锚的声明时刻）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RosterAnchorRef {
    pub org_id: String,
    pub anchor_root: String,
    pub ts: i64,
}

/// 名册快照条目 `{identity, role}`（org-signature §3 口径；identity 语义 =
/// 组织内成员标识，schema 不绑 rootId——Q20 切换后自然变为 org_user_id）。
///
/// A16 双写过渡：`identity` 保持 **rootId**——② 层签名回查（exporter/anchor
/// 均为 root 身份签名）以它为**零依赖锚点**，换 org_user_id 则核验方无法
/// 独立把签名者锚回名册（属格式 v3 重设计，归名册键切换窗口）；`orgUserId`
/// additive 携带（credential::RosterMember 同款形态），memberSetHash 仍只
/// 计 `{identity, role}` 两键，旧包无此键不受影响。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RosterMember {
    pub identity: String,
    pub role: String,
    /// 成员 org_user_id（A16 双写过渡；`None` = 未发布 accessKey 的存量成员）。
    #[serde(
        rename = "orgUserId",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub org_user_id: Option<String>,
}

/// roster 段（evidence §4.1，分层诚实第②层素材；exportV 2）：
/// 导出时刻组织名册快照 + 成员集承诺 + 覆盖该承诺的组织锚根 inclusion
/// proof。核验方零外部依赖复算（memberSetHash / anchorRoot / proof 全部
/// 自包含）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RosterSection {
    /// 成员集承诺哈希（`affair/snapshot.rs member_set_hash` 同算法复算）。
    pub member_set_hash: String,
    /// 覆盖锚引用（anchorRoot = 含覆盖锚的组织锚根默克尔根）。
    pub anchor: RosterAnchorRef,
    /// 名册快照（导出方本地 OrganizationRecord.members 的 identity + role）。
    pub snapshot: Vec<RosterMember>,
    /// 覆盖锚（导出方本机锚记录）在组织锚根默克尔树中的 inclusion proof
    /// （自叶向根兄弟哈希序列）。
    pub anchor_proof: Vec<String>,
}

/// 存证导出包（§9 线形 + evidence §4.1 roster 段；serde 字段序 = 文档列举序）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceExportPackage {
    /// 1 = 无名册段（阶段四F 初版）；2 = 含 roster 段（旧工具遇 2 fail-closed）。
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
    /// 名册存证锚快照段（治理场景默认携带；v1 包缺省）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roster: Option<RosterSection>,
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

/// 名册承诺存证条目集合名（domain = orgId，与 effectrcpt 同族口径）。
pub const ROSTER_ENTRY_COLLECTION: &str = "roster";

/// 名册承诺载荷（**无 ts**——承诺内容幂等：同名册同哈希，重复导出不重写
/// 条目；「承诺是否已在链上」按 payloadHash 全链扫描判定）。
pub fn roster_commitment_payload(org_id: &str, member_set_hash: &str) -> Value {
    serde_json::json!({
        "orgId": org_id,
        "memberSetHash": member_set_hash,
    })
}

/// 构建导出包（§9 + evidence §4.1）：全量连续链 + 锚集 + 根 + proofs +
/// 导出者签名；`roster` 段由调用方（kernel 编排层）预先构建注入——
/// 「先写条目后锚」（承诺条目 append + 锚定触发）在编排层完成，本函数
/// 保持只读。携带 roster 段时 formatVersion = 2，否则 1。
/// 链缺失中间条目 / 空链 → Err。
pub fn build_export_package<S: StorageBackend>(
    storage: &S,
    scope: ExportScope,
    exporter_signing_key: &ed25519_dalek::SigningKey,
    ts: i64,
    roster: Option<RosterSection>,
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
        format_version: if roster.is_some() { 2 } else { 1 },
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
        roster,
        spec: SpecRef {
            canonical_json: CANONICAL_JSON_SUMMARY.to_string(),
            spec_ref: SPEC_REF.to_string(),
        },
    };
    let payload = export_sign_payload(&package);
    package.exporter.sig = B64.encode(exporter_signing_key.sign(payload.as_bytes()).to_bytes());
    Ok(package)
}

// ---------------------------------------------------------------------------
// 六步核验（设计 §3 五步 + evidence §4.1 名册回查第六步；CLI 与测试共用）
// ---------------------------------------------------------------------------

/// 第②层（成员资格）结论三态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MembershipOutcome {
    /// 名册快照回查全部通过。
    Pass,
    /// 名册快照回查存在失败项（见 `membership_notes`）。
    Fail,
    /// 本包不含名册快照（v1 包）——如实标注未覆盖，不算失败。
    NotCovered,
}

/// 包内签名回查条目（签名清单，报告用）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignerCheck {
    /// 签名者类别（`exporter` / `anchor:<nodeId>`）。
    pub kind: String,
    /// 签名者 identity（rootId；= sha256hex(publicKey)）。
    pub identity: String,
    /// 名册快照中的当时角色（`admin`/`member`；不在册 → None）。
    pub role: Option<String>,
    /// 回查是否通过（在册且角色满足规则）。
    pub ok: bool,
    /// 失败原因（通过 → None）。
    pub reason: Option<String>,
}

/// 名册快照摘要（报告用）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RosterSummary {
    /// 快照人数。
    pub member_count: usize,
    /// 其中管理员数。
    pub admin_count: usize,
    /// 成员集承诺哈希。
    pub member_set_hash: String,
    /// 覆盖锚根。
    pub anchor_root: String,
    /// 覆盖锚声明时刻（ms）。
    pub anchor_ts: i64,
}

/// 核验报告（人读 + 机器判定两用；分层诚实三层分列）。
#[derive(Clone, Debug, Default)]
pub struct VerifyReport {
    /// 各步失败原因（step 标签前缀）；空 = 全部通过（②层 NotCovered 不算失败）。
    pub failures: Vec<String>,
    /// 链高（核验通过语义下 = head.seq）。
    pub height: u64,
    /// 锚点数。
    pub anchor_count: usize,
    /// 各成员链头（nodeId → (headSeq, headHash)）。
    pub member_heads: Vec<(String, u64, String)>,
    /// 导出者 rootId。
    pub exporter_root_id: String,
    /// 导出时刻（ms，报告用）。
    pub exporter_ts: i64,
    /// 第①层（完整性：链 + 锚 + 签名 + scope）失败项（failures 的子集，
    /// 分层报告用）。
    pub integrity_failures: Vec<String>,
    /// 第②层（成员资格）结论。
    pub membership: MembershipOutcome,
    /// 第②层失败原因 / NotCovered 说明。
    pub membership_notes: Vec<String>,
    /// 包内签名回查清单（exporter + 各锚签名者）。
    pub signer_checks: Vec<SignerCheck>,
    /// 名册快照摘要（v2 包且 roster 段解析成功时填充）。
    pub roster_summary: Option<RosterSummary>,
    /// 第③层（业务资格）说明——当前包格式不携带业务凭证，恒为未覆盖说明。
    pub business_note: String,
}

impl Default for MembershipOutcome {
    fn default() -> Self {
        Self::NotCovered
    }
}

impl VerifyReport {
    pub fn ok(&self) -> bool {
        self.failures.is_empty()
    }
}

/// 第③层固定说明（业务凭证不随包——分层诚实如实标注）。
pub const BUSINESS_LAYER_NOTE: &str = "本包未附业务凭证（验证人凭证等业务资格材料），第③层未覆盖";

fn parse_b64_32(raw: &str) -> Option<[u8; 32]> {
    let bytes = B64.decode(raw).ok()?;
    <[u8; 32]>::try_from(bytes.as_slice()).ok()
}

/// 六步核验（§3 五步 + evidence §4.1 ⑥名册回查）：① 导出者签名 →
/// ② 链式校验（连续 + prevHash + 重算 hash + head 吻合）→ ③ 锚验签 +
/// 重算 anchorRoot + inclusion proofs → ④ scope 覆盖性断言 → ⑤ 分层
/// 报告 → ⑥ 名册快照回查（v2 包：memberSetHash/anchorRoot/proof 复算 +
/// 包内签名者在册与角色回查；v1 包第②层如实标注未覆盖）。
///
/// 输入为包 JSON 文本；解析失败即单条失败报告。所有哈希经 `canonical`
/// 复算——禁止重写序列化（红线 5）。
pub fn verify_export_package(raw: &str) -> VerifyReport {
    let mut report = VerifyReport::default();
    report.business_note = BUSINESS_LAYER_NOTE.to_string();
    let package: EvidenceExportPackage = match serde_json::from_str(raw) {
        Ok(p) => p,
        Err(e) => {
            report
                .failures
                .push(format!("parse: 包 JSON 解析失败: {e}"));
            report.integrity_failures = report.failures.clone();
            return report;
        }
    };
    if package.format_version > 2 {
        report.failures.push(format!(
            "format: 未知 formatVersion {}（请升级核验工具）",
            package.format_version
        ));
    }
    report.height = package.head.seq;
    report.anchor_count = package.anchors.len();
    report.exporter_root_id = package.exporter.root_id.clone();
    report.exporter_ts = package.exporter.ts;

    // ① 导出者签名（publicKey ⟹ rootId 绑定 + Ed25519 验签）
    let step1 = (|| -> Result<(), String> {
        let pk_arr =
            parse_b64_32(&package.exporter.public_key).ok_or("exporter.publicKey 形状非法")?;
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

    // ① 层（完整性）失败归集（分层报告用）
    report.integrity_failures = report.failures.clone();

    // ⑥ 名册快照回查（evidence §4.1，分层诚实第②层）：
    // 复算 memberSetHash → 复算 anchorRoot → inclusion proof → 包内每条
    // 签名回查「签名者在快照中、当时角色满足策略」（策略见
    // [`SignerRolePolicy`]。v1 包如实标注 NotCovered（不搞「总体通过」模糊话术）。
    verify_roster_layer(&package, &mut report);

    report
}

/// 签名者角色策略（⑥，2026-09-08 主控裁定）：
/// - **导出者：在册即可**（导出是读侧行为，成员对自己持有的数据天然可
///   导出——不强制 admin）；
/// - **锚签名者：在册即可**（任何成员的链头承诺都有效）；
/// - **治理签名（votes/resolution 等包内治理签名分量）：角色须满足签名
///   对应策略**——「角色不符必败」的 admin 检查落点在此。当前包格式不
///   携带治理签名分量，该回查随治理签名入包时按策略接线
///   （[`SignerRolePolicy::AdminRequired`] 分支保留待用）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SignerRolePolicy {
    /// 在册即可（角色不限）。
    Member,
    /// 须在册且角色 admin（治理签名策略落点，待用）。
    AdminRequired,
}

/// 单条签名回查（⑥ 签名清单条目构建，策略判定纯函数便于钉测）。
pub(crate) fn check_signer_role(
    kind: String,
    identity: &str,
    role: Option<&str>,
    policy: SignerRolePolicy,
) -> SignerCheck {
    match role {
        None => SignerCheck {
            kind,
            identity: identity.to_string(),
            role: None,
            ok: false,
            reason: Some("签名者不在名册快照中".to_string()),
        },
        Some(role) if policy == SignerRolePolicy::AdminRequired && role != "admin" => SignerCheck {
            kind,
            identity: identity.to_string(),
            role: Some(role.to_string()),
            ok: false,
            reason: Some(format!("当时角色 {role} 不满足策略（须 admin）")),
        },
        Some(role) => SignerCheck {
            kind,
            identity: identity.to_string(),
            role: Some(role.to_string()),
            ok: true,
            reason: None,
        },
    }
}

/// 第②层名册回查（⑥）。只写 `membership` / `membership_notes` /
/// `signer_checks` / `roster_summary` 与 `failures`（② 前缀项）。
fn verify_roster_layer(package: &EvidenceExportPackage, report: &mut VerifyReport) {
    if package.format_version < 2 {
        report.membership = MembershipOutcome::NotCovered;
        report
            .membership_notes
            .push("本包不含名册快照（formatVersion 1），成员资格层未覆盖".to_string());
        return;
    }
    let mut notes: Vec<String> = Vec::new();
    let Some(roster) = &package.roster else {
        report.membership = MembershipOutcome::Fail;
        report
            .membership_notes
            .push("v2 包缺 roster 段".to_string());
        report.failures.push("② 成员资格: v2 包缺 roster 段".to_string());
        return;
    };

    // a. memberSetHash 复算（snapshot 形状校验随 member_set_hash 免费获得）
    let snapshot_values: Vec<Value> = roster
        .snapshot
        .iter()
        .map(|m| serde_json::json!({ "identity": m.identity, "role": m.role }))
        .collect();
    match crate::affair::snapshot::member_set_hash(&snapshot_values) {
        Ok(recomputed) if recomputed == roster.member_set_hash => {}
        Ok(_) => notes.push("memberSetHash 复算不符".to_string()),
        Err(e) => notes.push(format!("snapshot 形状非法: {e}")),
    }

    // b. anchorRoot 复算（包内锚集 → 组织锚根；须与 roster 引用一致）
    let recomputed_root = anchor_root(&package.anchors);
    if recomputed_root.as_deref() != Some(roster.anchor.anchor_root.as_str()) {
        notes.push("roster.anchorRoot 与包内锚集复算不符".to_string());
    }
    // scope 一致性：roster 锚引用的 orgId 须与 scope 声明一致
    if package.scope.org_id.as_deref() != Some(roster.anchor.org_id.as_str()) {
        notes.push(format!(
            "roster.anchor.orgId {} 与 scope.orgId {:?} 不符",
            roster.anchor.org_id, package.scope.org_id
        ));
    }

    // c. inclusion proof：包内锚集中存在「proof 可验且覆盖链内名册承诺
    //    条目」的锚（承诺条目：domain=orgId、collection=roster 的最后一条）
    let commit_seq = package
        .entries
        .iter()
        .filter(|e| e.domain == roster.anchor.org_id && e.collection == ROSTER_ENTRY_COLLECTION)
        .map(|e| e.seq)
        .max();
    if commit_seq.is_none() {
        notes.push("链内找不到名册承诺条目（domain=orgId, collection=roster）".to_string());
    }
    let covering = package.anchors.iter().filter(|a| {
        a.org_id == roster.anchor.org_id
            && commit_seq.is_none_or(|seq| a.head_seq >= seq)
            && verify_inclusion(
                &package.anchors,
                &a.node_id,
                &roster.anchor_proof,
                &roster.anchor.anchor_root,
            )
    });
    if commit_seq.is_some() && covering.count() == 0 {
        notes.push("anchorProof 无可验覆盖锚（proof 不符或覆盖锚 headSeq 未达承诺条目）".to_string());
    }

    // d. 签名回查（签名清单；角色策略见 [`SignerRolePolicy`]）
    let role_of = |identity: &str| {
        roster
            .snapshot
            .iter()
            .find(|m| m.identity == identity)
            .map(|m| m.role.as_str())
    };
    report.signer_checks.push(check_signer_role(
        "exporter".to_string(),
        &package.exporter.root_id,
        role_of(&package.exporter.root_id),
        SignerRolePolicy::Member,
    ));
    for anchor in &package.anchors {
        report.signer_checks.push(check_signer_role(
            format!("anchor:{}", anchor.node_id),
            &anchor.root_id,
            role_of(&anchor.root_id),
            SignerRolePolicy::Member,
        ));
    }
    for c in &report.signer_checks {
        if !c.ok {
            notes.push(format!(
                "签名回查失败（{}: {}）：{}",
                c.kind,
                c.identity,
                c.reason.clone().unwrap_or_default()
            ));
        }
    }

    // 摘要（报告用；即便有失败项也如实呈现快照内容）
    report.roster_summary = Some(RosterSummary {
        member_count: roster.snapshot.len(),
        admin_count: roster.snapshot.iter().filter(|m| m.role == "admin").count(),
        member_set_hash: roster.member_set_hash.clone(),
        anchor_root: roster.anchor.anchor_root.clone(),
        anchor_ts: roster.anchor.ts,
    });

    report.membership = if notes.is_empty() {
        MembershipOutcome::Pass
    } else {
        MembershipOutcome::Fail
    };
    for reason in &notes {
        report.failures.push(format!("② 成员资格: {reason}"));
    }
    report.membership_notes = notes;
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
            None,
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
        assert!(
            r.failures.iter().any(|f| f.contains('②')),
            "{:?}",
            r.failures
        );
        // ① 也应失败（签名覆盖全文）
        assert!(
            r.failures.iter().any(|f| f.contains('①')),
            "{:?}",
            r.failures
        );

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
            None,
        )
        .unwrap();
        let r = verify_export_package(&serde_json::to_string(&pkg).unwrap());
        assert!(!r.ok());
        assert!(
            r.failures.iter().any(|f| f.contains('④')),
            "{:?}",
            r.failures
        );
    }

    // ------------------------------------------------------------------
    // ⑥ 名册快照回查（evidence §4.1 / §六验收）
    // ------------------------------------------------------------------

    const ROSTER_ORG: &str = "org_0000000000000001";

    fn identity_of(k: &ed25519_dalek::SigningKey) -> String {
        hex::encode(Sha256::digest(k.verifying_key().to_bytes()))
    }

    /// 构造 v2 包：链内含名册承诺条目，锚集含覆盖锚（nodeA），roster 段
    /// 按给定名册（identity/role 对）构建。
    fn build_v2_package(members: &[(&str, &str)]) -> EvidenceExportPackage {
        let (s, _last) = sample_storage();
        let values: Vec<Value> = members
            .iter()
            .map(|(id, role)| serde_json::json!({ "identity": id, "role": role }))
            .collect();
        let hash = crate::affair::snapshot::member_set_hash(&values).unwrap();
        let payload = roster_commitment_payload(ROSTER_ORG, &hash);
        let mut raw = MemoryStorage::new();
        raw.clone_from(&s);
        // 名册承诺条目上链（domain=orgId, collection=roster）
        append_evidence(
            &mut raw,
            NewEvidenceEntry::from_parts(
                ROSTER_ORG,
                ROSTER_ENTRY_COLLECTION,
                ROSTER_ORG,
                EvidenceOp::Put,
                Some(&payload),
                None,
                1_720_000_000_100,
                "nodeA",
            ),
        )
        .unwrap();
        // 覆盖锚（nodeA，head_seq 覆盖承诺条目）+ 另一成员锚
        let cover = super::super::anchor::sign_anchor(
            &keypair(0x11),
            ROSTER_ORG,
            "nodeA",
            4,
            &get_evidence_head(&raw).unwrap().unwrap().hash,
            1_720_000_000_200,
        );
        let other = super::super::anchor::sign_anchor(
            &keypair(0x22),
            ROSTER_ORG,
            "nodeB",
            5,
            &"cd".repeat(32),
            1_720_000_000_300,
        );
        for a in [&cover, &other] {
            raw.put(
                &super::super::anchor::anchor_key(&a.org_id, &a.node_id),
                &serde_json::to_string(a).unwrap(),
            )
            .unwrap();
        }
        let anchors = vec![cover.clone(), other];
        let roster = RosterSection {
            member_set_hash: hash,
            anchor: RosterAnchorRef {
                org_id: ROSTER_ORG.to_string(),
                anchor_root: anchor_root(&anchors).unwrap(),
                ts: cover.ts,
            },
            snapshot: members
                .iter()
                .map(|(id, role)| RosterMember {
                    identity: id.to_string(),
                    role: role.to_string(),
                    org_user_id: None,
                })
                .collect(),
            anchor_proof: inclusion_proof(&anchors, "nodeA").unwrap(),
        };
        build_export_package(
            &raw,
            ExportScope {
                domain: Some("plugin:vote".to_string()),
                collection: Some("ballots".to_string()),
                org_id: Some(ROSTER_ORG.to_string()),
            },
            &keypair(0xEE),
            1_720_000_000_500,
            Some(roster),
        )
        .unwrap()
    }

    /// 名册含导出者（admin）+ 两锚签名者。
    fn standard_member_refs() -> Vec<(String, String)> {
        vec![
            (identity_of(&keypair(0xEE)), "admin".to_string()),
            (identity_of(&keypair(0x11)), "admin".to_string()),
            (identity_of(&keypair(0x22)), "member".to_string()),
        ]
    }

    #[test]
    fn roster_v2_roundtrip_passes_layer_two() {
        let members = standard_member_refs();
        let refs: Vec<(&str, &str)> = members.iter().map(|(i, r)| (i.as_str(), r.as_str())).collect();
        let pkg = build_v2_package(&refs);
        assert_eq!(pkg.format_version, 2);
        let r = verify_export_package(&serde_json::to_string(&pkg).unwrap());
        assert!(r.ok(), "failures: {:?}", r.failures);
        assert_eq!(r.membership, MembershipOutcome::Pass);
        // 签名清单：exporter + 两锚签名者全部在册通过；exporter 角色 admin
        assert_eq!(r.signer_checks.len(), 3);
        assert!(r.signer_checks.iter().all(|c| c.ok));
        let exporter = &r.signer_checks[0];
        assert_eq!(exporter.kind, "exporter");
        assert_eq!(exporter.role.as_deref(), Some("admin"));
        // 名册摘要
        let summary = r.roster_summary.expect("roster summary");
        assert_eq!(summary.member_count, 3);
        assert_eq!(summary.admin_count, 2);
    }

    #[test]
    fn roster_signer_not_member_fails() {
        // 名册缺锚签名者 0x22 → 该签名回查必败（非成员）
        let exporter_id = identity_of(&keypair(0xEE));
        let cover_id = identity_of(&keypair(0x11));
        let members = [(exporter_id.as_str(), "admin"), (cover_id.as_str(), "admin")];
        let pkg = build_v2_package(&members);
        let r = verify_export_package(&serde_json::to_string(&pkg).unwrap());
        assert!(!r.ok(), "非成员签名者必须判失败");
        assert_eq!(r.membership, MembershipOutcome::Fail);
        assert!(
            r.signer_checks
                .iter()
                .any(|c| !c.ok && c.identity == identity_of(&keypair(0x22))),
            "0x22 不在册 → 回查失败: {:?}",
            r.signer_checks
        );
        assert!(r.failures.iter().any(|f| f.contains('②')));
    }

    /// 2026-09-08 主控裁定：导出者在册即可（导出是读侧行为，不强制
    /// admin）——member 导出者②层通过；admin 策略落点是包内治理签名
    /// （当前包格式不携带治理签名分量，AdminRequired 分支机制钉测保留）。
    #[test]
    fn roster_exporter_member_passes_and_admin_policy_pinned() {
        let exporter_id = identity_of(&keypair(0xEE));
        let cover_id = identity_of(&keypair(0x11));
        let other_id = identity_of(&keypair(0x22));
        let members = [
            (exporter_id.as_str(), "member"),
            (cover_id.as_str(), "admin"),
            (other_id.as_str(), "member"),
        ];
        let pkg = build_v2_package(&members);
        let r = verify_export_package(&serde_json::to_string(&pkg).unwrap());
        assert!(r.ok(), "member 导出者在册即通过: {:?}", r.failures);
        assert_eq!(r.membership, MembershipOutcome::Pass);
        let exporter = &r.signer_checks[0];
        assert!(exporter.ok);
        assert_eq!(exporter.role.as_deref(), Some("member"));

        // 治理签名 admin 策略保持必败（机制钉测：AdminRequired + 非 admin
        // → 必败；治理签名分量入包时按此策略接线）
        let c = check_signer_role(
            "governance:vote".to_string(),
            "aa",
            Some("member"),
            SignerRolePolicy::AdminRequired,
        );
        assert!(!c.ok, "治理签名 admin 策略：非 admin 必败");
        assert!(c.reason.unwrap().contains("admin"));
        let c = check_signer_role(
            "governance:vote".to_string(),
            "aa",
            Some("admin"),
            SignerRolePolicy::AdminRequired,
        );
        assert!(c.ok);
    }

    #[test]
    fn roster_tamper_each_part_fails_layer_two() {
        let members = standard_member_refs();
        let refs: Vec<(&str, &str)> = members.iter().map(|(i, r)| (i.as_str(), r.as_str())).collect();
        let pkg = build_v2_package(&refs);
        let baseline = verify_export_package(&serde_json::to_string(&pkg).unwrap());
        assert!(baseline.ok(), "baseline: {:?}", baseline.failures);

        // 篡改 snapshot（换一名成员 identity）→ memberSetHash 复算不符
        let mut p1 = pkg.clone();
        p1.roster.as_mut().unwrap().snapshot[2].identity = "ff".repeat(32);
        let r = verify_export_package(&serde_json::to_string(&p1).unwrap());
        assert!(!r.ok());
        assert!(
            r.failures.iter().any(|f| f.contains('②')),
            "{:?}",
            r.failures
        );

        // 篡改 memberSetHash
        let mut p2 = pkg.clone();
        p2.roster.as_mut().unwrap().member_set_hash = "00".repeat(32);
        let r = verify_export_package(&serde_json::to_string(&p2).unwrap());
        assert!(!r.ok());
        assert!(r.failures.iter().any(|f| f.contains('②')));

        // 篡改 anchorRoot
        let mut p3 = pkg.clone();
        p3.roster.as_mut().unwrap().anchor.anchor_root = "00".repeat(32);
        let r = verify_export_package(&serde_json::to_string(&p3).unwrap());
        assert!(!r.ok());
        assert!(r.failures.iter().any(|f| f.contains('②')));

        // 篡改 anchorProof（换 sibling）
        let mut p4 = pkg.clone();
        let proof = &mut p4.roster.as_mut().unwrap().anchor_proof;
        if !proof.is_empty() {
            proof[0] = "00".repeat(32);
        }
        let r = verify_export_package(&serde_json::to_string(&p4).unwrap());
        assert!(!r.ok(), "proof 篡改必败");
    }

    #[test]
    fn v1_package_membership_not_covered_and_v2_requires_roster() {
        // v1 包：② 层 NotCovered 不算失败
        let (s, _last) = sample_storage();
        let a = super::super::anchor::sign_anchor(
            &keypair(0x11),
            ROSTER_ORG,
            "nodeA",
            3,
            &"ab".repeat(32),
            1_720_000_000_100,
        );
        let pkg = build_package(&s, &[a], Some(ROSTER_ORG));
        assert_eq!(pkg.format_version, 1);
        let r = verify_export_package(&serde_json::to_string(&pkg).unwrap());
        assert!(r.ok(), "v1 包①层通过: {:?}", r.failures);
        assert_eq!(r.membership, MembershipOutcome::NotCovered);
        assert!(
            r.membership_notes
                .iter()
                .any(|n| n.contains("不含名册快照")),
            "{:?}",
            r.membership_notes
        );

        // v2 缺 roster 段：必败（format_version 手工改 2）
        let mut forged = pkg.clone();
        forged.format_version = 2;
        let r = verify_export_package(&serde_json::to_string(&forged).unwrap());
        assert!(!r.ok());
        assert!(r.failures.iter().any(|f| f.contains('②')));
    }
}

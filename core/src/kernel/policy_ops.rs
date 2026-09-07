//! policy 门面（community-affairs C9）：插件 SDK `sdk.policy` 的内核侧最小
//! 命令层（wiki/architecture/community-affairs.md §7.2）。
//!
//! 策略插件只产出声明式文档（B1 求值器，policy §1）；本门面提供本地草稿
//! 的读取与提交（提交 = 结构/引擎校验 + §5 静态分析 + 落本地草稿键）。
//! 草稿是本地工作副本（`policy:draft:` 键域，不进同步流量）；策略文档的
//! 签名合入（OrgSigSet）归 C3，正式生效面的分发不在本命令层。

use serde_json::{Value, json};

use super::{Kernel, KernelError, Result};
use crate::p2p::node::system_now_ms;
use crate::policy::{PolicyDoc, analyze, policy_doc_hash, validate_policy_doc};
use crate::storage::{ScanOptions, StorageBackend};

/// 本地策略草稿键前缀（本地键，不进同步；规格未定义策略文档存储键，
/// 草稿簿记是本命令层的最小落地形态）。
const POLICY_DRAFT_PREFIX: &str = "policy:draft:";

/// `policy:draft:{orgId}`：某组织的本地策略草稿（每组织一条，LWW）。
/// read-gate §4 第 5 步（policyRef 求值）也从本键域取策略文档
/// （`kernel/inbound_dm/orgq.rs`），故开放 crate 内访问。
pub(crate) fn policy_draft_key(org_id: &str) -> String {
    format!("{POLICY_DRAFT_PREFIX}{org_id}")
}

/// 组织签名策略文档（org-genesis §5）与本模块无关，但 orgId 双形态校验
/// 同口径（affair effect 既有判定直接复用）。
fn is_valid_org_id(org_id: &str) -> bool {
    crate::affair::is_valid_org_id(org_id)
}

impl Kernel {
    /// 读策略文档（sdk.policy.read）：返回本组织最新本地草稿
    /// `{doc, policyDocHash, savedAt}`；无草稿返回 null。
    pub fn policy_read(&self, org_id: &str) -> Result<Option<Value>> {
        if !is_valid_org_id(org_id) {
            return Err(KernelError::Internal("invalid orgId".to_string()));
        }
        let raw = self.require_storage()?.get(&policy_draft_key(org_id))?;
        match raw {
            None => Ok(None),
            Some(raw) => serde_json::from_str::<Value>(&raw)
                .map(Some)
                .map_err(|e| KernelError::Internal(format!("corrupted policy draft: {e}"))),
        }
    }

    /// 提交策略文档草稿（sdk.policy.submitDraft）：结构/引擎校验（fail-closed，
    /// 非 b1 引擎拒绝）→ §5 静态分析（与上一份草稿比较暴露面变化）→ 落
    /// 本地草稿键。返回 policyDocHash 与 findings；findings 含 Error 时
    /// 草稿仍保存（草稿语义：供编辑界面回显阻断项），是否阻断发布由插件/
    /// 产品流程按 Error findings 裁决。
    pub fn policy_submit_draft(&mut self, doc: &Value) -> Result<Value> {
        let parsed = serde_json::from_value::<PolicyDoc>(doc.clone())
            .map_err(|e| KernelError::Internal(format!("malformed policy doc: {e}")))?;
        // round-trip 比对：线形类型 serde 默认忽略未知键，误拼键（如 `sigset`）
        // 会被静默丢弃后照常受理（无声丢签名）；反序列化后再 canonical 序列化
        // 与入参 canonical 不等即拒绝。不用 deny_unknown_fields——「旧端忽略
        // 未知键」的兼容口径需规格背书。
        let round_tripped = serde_json::to_value(&parsed)
            .map_err(|e| KernelError::Internal(format!("policy doc re-serialize failed: {e}")))?;
        if crate::evidence::normalize_object(&round_tripped)
            != crate::evidence::normalize_object(doc)
        {
            return Err(KernelError::Internal(
                "malformed policy doc: unknown or non-canonical keys".to_string(),
            ));
        }
        let doc = parsed;
        validate_policy_doc(&doc).map_err(|e| KernelError::Internal(e.to_string()))?;
        let prev = self
            .policy_read(&doc.org_id)?
            .and_then(|v| serde_json::from_value::<PolicyDoc>(v["doc"].clone()).ok());
        let findings = analyze(&doc, prev.as_ref());
        let hash = policy_doc_hash(&doc)
            .map_err(|e| KernelError::Internal(format!("policy doc hash failed: {e}")))?;
        let saved = json!({
            "doc": doc,
            "policyDocHash": hash,
            "savedAt": system_now_ms(),
        });
        self.require_storage_raw_mut()?
            .put(&policy_draft_key(&doc.org_id), &saved.to_string())?;
        let findings: Vec<Value> = findings
            .iter()
            .map(|f| {
                json!({
                    "severity": match f.severity {
                        crate::policy::Severity::Error => "error",
                        crate::policy::Severity::Warning => "warning",
                    },
                    "code": f.code,
                    "detail": f.detail,
                })
            })
            .collect();
        Ok(json!({ "policyDocHash": hash, "findings": findings }))
    }

    /// 发布策略草稿（sdk.policy.publish）：本地草稿 → 附组织签名包
    /// （OrgSigSet，org-signature §2）→ 落发布键域 `org:policydoc:{orgId}`
    /// （org:structure@v1 内建集合，随组织同步分发；入站合入点
    /// [`crate::org::service::adjudicate_incoming_policy_doc`] 以同一五步链
    /// 把关）。草稿簿记保留（编辑工作副本语义不变）。
    ///
    /// 签名主体 = 本机 root 身份（名册 admin 成员）；AnyAdmin 单签自足，
    /// m-of-n（m>1）本机只能贡献一票——如实报错（多签收集流不在本命令层）。
    /// 无策略链（legacy 组织未发布创世记录）/ 无存证锚 / 本机非 admin 同样
    /// 如实报错，不产出无法通过入站合入校验的发布件。
    pub fn policy_publish(&mut self, org_id: &str) -> Result<Value> {
        if !is_valid_org_id(org_id) {
            return Err(KernelError::Internal("invalid orgId".to_string()));
        }
        let draft = self
            .policy_read(org_id)?
            .ok_or_else(|| KernelError::Internal(format!("no policy draft for {org_id}")))?;
        let mut doc: PolicyDoc = serde_json::from_value(draft["doc"].clone())
            .map_err(|e| KernelError::Internal(format!("corrupted policy draft: {e}")))?;
        validate_policy_doc(&doc).map_err(|e| KernelError::Internal(e.to_string()))?;
        if doc.org_id != org_id {
            return Err(KernelError::Internal(
                "policy draft orgId mismatch".to_string(),
            ));
        }
        // 重发布幂等：草稿改动才出新 policyDocHash；同一文档重复发布 =
        // 同键同值覆写（LWW 同刻保留现状，同步面无放大）。
        let doc_hash = policy_doc_hash(&doc)
            .map_err(|e| KernelError::Internal(format!("policy doc hash failed: {e}")))?;

        let storage = self.require_storage()?;
        // 策略链：现行签名策略 = 链头（最大 seq 修订，否则创世）；空链无法
        // 定位 policyHash（入站五步链第 2 步必拒）——如实报错。
        let policies = crate::org::service::load_policy_chain(storage, org_id)?;
        let head = policies
            .iter()
            .max_by_key(|p| p.seq())
            .ok_or_else(|| {
                KernelError::Internal(format!(
                    "no policy chain for {org_id}（legacy 组织未发布创世记录，无法签发组织签名包）"
                ))
            })?;
        match head.signing_policy() {
            crate::org::genesis::SigningPolicy::AnyAdmin => {}
            crate::org::genesis::SigningPolicy::MOfN { m, .. } if *m <= 1 => {}
            crate::org::genesis::SigningPolicy::MOfN { m, .. } => {
                return Err(KernelError::Internal(format!(
                    "signing policy requires {m} admin signatures（多签收集流不在本命令层，本机只能贡献 1 票）"
                )));
            }
        }
        let policy_hash = head
            .policy_hash()
            .map_err(|e| KernelError::Internal(format!("policy hash failed: {e}")))?;

        // 名册快照（org:meta 当前投影）+ 本机 admin 资格校验
        let record = crate::org::service::OrganizationService::get_record(storage, org_id)
            .map_err(|e| KernelError::Internal(e.to_string()))?
            .ok_or_else(|| KernelError::Internal(format!("org record not found: {org_id}")))?;
        let roster: Vec<crate::credential::RosterMember> = record
            .members
            .iter()
            .map(|m| crate::credential::RosterMember {
                identity: m.root_id.clone(),
                role: m.role.as_str().to_string(),
            })
            .collect();
        let unlocked = self.unlocked.as_ref().ok_or(KernelError::Locked)?;
        let root = crate::identity::derive_root_identity(&unlocked.seed);
        let signer_id = root.id();
        if !roster
            .iter()
            .any(|m| m.identity == signer_id && m.role == "admin")
        {
            return Err(KernelError::Internal(
                "本机身份不是该组织管理员，无权发布策略".to_string(),
            ));
        }

        // 存证锚根（sync-evidence §7 复算口径，与 sigset_storage_closures 同源）
        let anchor_prefix = format!("{}{}:", crate::evidence::EVIDENCE_ANCHOR_PREFIX, org_id);
        let anchors: Vec<crate::evidence::AnchorRecord> = storage
            .scan(&ScanOptions::prefix(&anchor_prefix))?
            .into_iter()
            .filter_map(|(_, raw)| serde_json::from_str(&raw).ok())
            .collect();
        let anchor_root = crate::evidence::anchor_root(&anchors).ok_or_else(|| {
            KernelError::Internal(format!("no evidence anchor for {org_id}（无法承诺名册状态）"))
        })?;

        // 构造 OrgSigSet（快照随包携带：接收方无名册历史也能复算 memberSetHash；
        // 锚时刻取签名时刻——锚根复算不依赖 ts，见 sigset_storage_closures 闭包）
        let now = system_now_ms();
        let mut sig_set = crate::credential::OrgSigSet {
            sig_set_v: 1,
            org_id: org_id.to_string(),
            subject: doc_hash.clone(),
            policy_hash,
            roster: crate::credential::RosterCommitment {
                member_set_hash: crate::org::sigset::roster_member_set_hash(&roster),
                anchor: crate::credential::RosterAnchor {
                    org_id: org_id.to_string(),
                    anchor_root,
                    ts: now,
                },
                snapshot: Some(roster),
            },
            signed_at: now,
            signatures: Vec::new(),
        };
        let payload = crate::org::sigset::component_sign_payload(&sig_set);
        use ed25519_dalek::Signer as _;
        use base64::Engine as _;
        sig_set.signatures.push(crate::credential::ComponentSignature {
            signer: signer_id.clone(),
            public_key: base64::engine::general_purpose::STANDARD.encode(root.public_key()),
            sig: base64::engine::general_purpose::STANDARD
                .encode(root.signing_key.sign(payload.as_bytes()).to_bytes()),
        });

        // 自检：产出必须能过入站合入的同一五步链（不通 = 实现 bug，不落库）。
        // 闭包借 storage 的析构期 borrow 会延至作用域末，块作用域确保其在可变写前释放。
        let verdict = {
            let (anchor_matches, roster_lookup) =
                crate::org::service::sigset_storage_closures(storage, org_id);
            let ctx = crate::org::sigset::OrgSigSetVerifyContext {
                policies: &policies,
                anchor_matches: &anchor_matches,
                roster_lookup: &roster_lookup,
            };
            ctx.verify_detailed(&sig_set)
                .map_err(|e| KernelError::Internal(format!("sigset self-check failed: {e}")))?
        };

        doc.sig_set = Some(sig_set);
        let raw = serde_json::to_string(&doc)
            .map_err(|e| KernelError::Internal(format!("policy doc serialize failed: {e}")))?;
        // 发布键属 org:structure@v1 受管键域：经 VersionedStorage 写入即
        // 版本化/pmeta 记账，orgsync 前缀采集自动带出（不能用 raw 句柄绕账）。
        self.require_storage_mut()?
            .put(&crate::org::service::policy_doc_key(org_id), &raw)?;
        Ok(json!({
            "orgId": org_id,
            "policyDocHash": doc_hash,
            "publishedAt": now,
            "signer": signer_id,
            "degraded": verdict.degraded,
        }))
    }
}

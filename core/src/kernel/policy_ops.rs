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

        let (sig_set, signer_id, degraded) = self.build_signed_org_sig_set(org_id, &doc_hash)?;
        let now = system_now_ms();
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
            "degraded": degraded,
        }))
    }

    /// 发布名册开放声明（A15 / membership §4.3）：`org:disclosure:{orgId}:
    /// {targetDomain}` = { 档位, 字段授权[], 开放集合[], version, effectiveAt,
    /// OrgSigSet 签名 }——组织级动作（本机须属主组织 admin）。
    ///
    /// **公示延迟**：暴露面扩大方向（档位升 / 新增字段授权 / 字段受众升 /
    /// 新增开放集合，含首份非全隐声明）`effectiveAt = updatedAt + 24h`；
    /// 收窄/持平即时生效。扩大且未显式确认（`confirm_widening = false`）→
    /// 如实报错（暴露面扩大必须显式确认，interpretation §4.2 静态分析同族
    /// 语义）。发布即公示（org:structure@v1 受管键域随 orgsync 全员流动），
    /// 生效由 effectiveAt 门控；入站合入点
    /// [`crate::org::service::adjudicate_incoming_disclosure`] 以同一五步链把关。
    pub fn disclosure_publish(
        &mut self,
        org_id: &str,
        target_domain: &str,
        tier: crate::policy::RosterTier,
        fields: Vec<crate::policy::FieldRule>,
        collections: Vec<String>,
        confirm_widening: bool,
    ) -> Result<Value> {
        if !is_valid_org_id(org_id) || !is_valid_org_id(target_domain) {
            return Err(KernelError::Internal("invalid orgId/targetDomain".to_string()));
        }
        let now = system_now_ms();
        let storage = self.require_storage()?;
        let prev: Option<crate::policy::DisclosureRecord> = storage
            .get(&crate::policy::disclosure_key(org_id, target_domain))?
            .and_then(|raw| serde_json::from_str(&raw).ok());
        let mut next = crate::policy::DisclosureRecord {
            disclosure_v: crate::policy::DISCLOSURE_V,
            org_id: org_id.to_string(),
            target_domain: target_domain.to_string(),
            tier,
            fields,
            collections,
            version: prev.as_ref().map_or(1, |p| p.version + 1),
            updated_at: now,
            effective_at: now, // 收窄/持平即时；扩大下方改写
            sig_set: None,
        };
        let widening = crate::policy::disclosure_widening(prev.as_ref(), &next);
        if widening {
            if !confirm_widening {
                return Err(KernelError::Internal(
                    "暴露面扩大（开放档位/字段授权/开放集合放宽）须显式确认后发布".to_string(),
                ));
            }
            // 公示延迟：扩大方向 24h 后生效（发布即公示，防瞬间开放不可逆暴露）
            next.effective_at = now + crate::policy::DISCLOSURE_PUB_PERIOD_MS;
        }
        crate::policy::validate_disclosure(&next)
            .map_err(|e| KernelError::Internal(e.to_string()))?;
        let hash = crate::policy::disclosure_hash(&next)
            .map_err(|e| KernelError::Internal(format!("disclosure hash failed: {e}")))?;
        let (sig_set, signer_id, _degraded) = self.build_signed_org_sig_set(org_id, &hash)?;
        next.sig_set = Some(sig_set);
        let raw = serde_json::to_string(&next)
            .map_err(|e| KernelError::Internal(format!("disclosure serialize failed: {e}")))?;
        self.require_storage_mut()?
            .put(&crate::policy::disclosure_key(org_id, target_domain), &raw)?;
        Ok(json!({
            "orgId": org_id,
            "targetDomain": target_domain,
            "disclosureHash": hash,
            "version": next.version,
            "widening": widening,
            "updatedAt": now,
            "effectiveAt": next.effective_at,
            "signer": signer_id,
        }))
    }

    /// 发布准入策略声明（A17 / membership §4.5）：`org:accept:{orgId}` =
    /// `{ acceptCredentials[], version, effectiveAt, OrgSigSet 签名 }`——组织级
    /// 动作（本机须属主组织 admin），声明「接受哪些凭证可免预录入册」。
    ///
    /// **公示延迟**（governance §4.1 适用范围）：准入面扩大方向（新增
    /// `(credType, issuerTrust)` 规则对，含首份非空声明）
    /// `effectiveAt = updatedAt + 24h`；收窄/持平即时生效。扩大且未显式确认
    /// （`confirm_widening = false`）→ 如实报错。发布即公示（org:structure@v1
    /// 受管键域随 orgsync 全员流动），生效由 effectiveAt 门控；入站合入点
    /// [`crate::org::service::adjudicate_incoming_accept_policy`] 以同一
    /// 五步链把关。
    pub fn accept_policy_publish(
        &mut self,
        org_id: &str,
        accept_credentials: Vec<crate::policy::AcceptCredentialRule>,
        confirm_widening: bool,
    ) -> Result<Value> {
        if !is_valid_org_id(org_id) {
            return Err(KernelError::Internal("invalid orgId".to_string()));
        }
        let now = system_now_ms();
        let storage = self.require_storage()?;
        let prev: Option<crate::policy::AcceptPolicyRecord> = storage
            .get(&crate::policy::accept_policy_key(org_id))?
            .and_then(|raw| serde_json::from_str(&raw).ok());
        let mut next = crate::policy::AcceptPolicyRecord {
            accept_v: crate::policy::ACCEPT_POLICY_V,
            org_id: org_id.to_string(),
            accept_credentials,
            version: prev.as_ref().map_or(1, |p| p.version + 1),
            updated_at: now,
            effective_at: now, // 收窄/持平即时；扩大下方改写
            sig_set: None,
        };
        let widening = crate::policy::accept_policy_widening(prev.as_ref(), &next);
        if widening {
            if !confirm_widening {
                return Err(KernelError::Internal(
                    "准入面扩大（新增免预录准入规则）须显式确认后发布".to_string(),
                ));
            }
            // 公示延迟：扩大方向 24h 后生效（发布即公示，防瞬间放宽不可逆准入）
            next.effective_at = now + crate::policy::ACCEPT_POLICY_PUB_PERIOD_MS;
        }
        crate::policy::validate_accept_policy(&next)
            .map_err(|e| KernelError::Internal(e.to_string()))?;
        let hash = crate::policy::accept_policy_hash(&next)
            .map_err(|e| KernelError::Internal(format!("accept policy hash failed: {e}")))?;
        let (sig_set, signer_id, _degraded) = self.build_signed_org_sig_set(org_id, &hash)?;
        next.sig_set = Some(sig_set);
        let raw = serde_json::to_string(&next)
            .map_err(|e| KernelError::Internal(format!("accept policy serialize failed: {e}")))?;
        self.require_storage_mut()?
            .put(&crate::policy::accept_policy_key(org_id), &raw)?;
        Ok(json!({
            "orgId": org_id,
            "acceptPolicyHash": hash,
            "version": next.version,
            "widening": widening,
            "updatedAt": now,
            "effectiveAt": next.effective_at,
            "signer": signer_id,
        }))
    }

    /// 组织签名包构建（policy_publish / disclosure_publish /
    /// accept_policy_publish 共用段）：策略链
    /// 定位（空链 / m>1 多签如实报错）→ 名册快照 + 本机 admin 校验 → A16
    /// 域私钥签名（signer = org_user_id）→ OrgSigSet 构造 + 自检（不通 =
    /// 实现 bug，不落库）。返回 (sigSet, signerId, degraded)。
    fn build_signed_org_sig_set(
        &self,
        org_id: &str,
        subject: &str,
    ) -> Result<(crate::credential::OrgSigSet, String, bool)> {
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
                // A16 双写：名册快照携带 org_user_id（签名面切换后名册回查
                // 按 org_user_id 命中；未发布成员为 None，旧签按 rootId 命中）。
                org_user_id: m.org_user_id(),
            })
            .collect();
        let unlocked = self.unlocked.as_ref().ok_or(KernelError::Locked)?;
        let root_id = unlocked.root_id().to_string();
        if !record
            .members
            .iter()
            .any(|m| m.root_id == root_id && m.role == crate::org::types::OrganizationRole::Admin)
        {
            return Err(KernelError::Internal(
                "本机身份不是该组织管理员，无权发布策略".to_string(),
            ));
        }
        // A16 签名面（membership §4.4-2）：组织内操作改用 `org-access:{orgId}`
        // 域私钥签名（org-signature §2.1 签名者密钥口径本就是「该组织内的域
        // 身份私钥」——线形零改动，切换的是密钥来源）；signer = org_user_id。
        let domain_identity = crate::identity::derive_domain_identity(
            &unlocked.seed,
            &crate::org::access_key::org_access_domain(org_id),
        );
        let domain_pubkey = domain_identity.signing_key.verifying_key().to_bytes();
        let signer_id = crate::org::access_key::org_user_id_from_pubkey(&domain_pubkey);

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
            subject: subject.to_string(),
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
        use base64::Engine as _;
        use ed25519_dalek::Signer as _;
        sig_set.signatures.push(crate::credential::ComponentSignature {
            signer: signer_id.clone(),
            public_key: base64::engine::general_purpose::STANDARD.encode(domain_pubkey),
            sig: base64::engine::general_purpose::STANDARD
                .encode(domain_identity.signing_key.sign(payload.as_bytes()).to_bytes()),
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
        Ok((sig_set, signer_id, verdict.degraded))
    }
}

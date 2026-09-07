//! credential 门面（community-affairs C9）：插件 SDK `sdk.credentials` 的内核
//! 侧最小命令层（wiki/architecture/community-affairs.md §7.2）。
//!
//! 边界（credential §1）：签发流程不在内核——持有凭证由验证插件的人机流程
//! 产出后存入 `cred:held:` 键域（本门面只读）；holderProof 呈现用插件域身份
//! 私钥签名（域私钥永不离开内核，与 identity.sign 同机械层）；验证人信任
//! 声明（`org:verifiers:`，org:structure 键域）本门面只做只读查询，合入
//! 校验（OrgSigSet 五步链）归 C3。

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use serde_json::{Value, json};

use super::{Kernel, KernelError, Result};
use crate::credential::{
    CRED_HELD_PREFIX, Credential, HolderProof, RevocationSnapshot, TrustDecl, credential_id,
    held_credential_key, holder_proof_payload, revocation_snapshot_key, trust_decl_key,
    validate_trust_decl_structure, verify_credential_static, verify_not_revoked,
};
use crate::p2p::node::system_now_ms;
use crate::storage::{ScanOptions, StorageBackend};

impl Kernel {
    /// 本机持有的凭证列表（sdk.credentials.listHeld）：`cred:held:` 键域全量
    /// 扫描，按 credId 字典序返回原文（损坏记录跳过并留日志）。
    pub fn credential_list_held(&self) -> Result<Vec<Value>> {
        let storage = self.require_storage()?;
        let mut items = Vec::new();
        for (key, raw) in storage.scan(&ScanOptions::prefix(CRED_HELD_PREFIX))? {
            let Some(cred_id) = key.strip_prefix(CRED_HELD_PREFIX) else {
                continue;
            };
            match serde_json::from_str::<Value>(&raw) {
                Ok(value) => items.push(json!({ "credId": cred_id, "credential": value })),
                Err(e) => log::warn!("[credential] skip corrupted held record {cred_id}: {e}"),
            }
        }
        items.sort_by(|a, b| a["credId"].as_str().cmp(&b["credId"].as_str()));
        Ok(items)
    }

    /// 呈现 holderProof（sdk.credentials.presentHolderProof）：读持有凭证，
    /// 静态校验（结构 + credId 复算 + 验证人签名）后，用调用方插件域身份
    /// 私钥对 read-gate §3 载荷（绑定 requestId/目标集合/呈现时刻，防重放
    /// 转投）签名。凭证持有者的公钥必须与该域身份一致（防为别人的凭证
    /// 出具持有证明）。
    pub fn credential_present_holder_proof(
        &self,
        domain: &str,
        cred_id: &str,
        request_id: &str,
        org_id: &str,
        collection: &str,
    ) -> Result<Value> {
        let raw = self
            .require_storage()?
            .get(&held_credential_key(cred_id))?
            .ok_or_else(|| {
                KernelError::Internal(format!("held credential not found: {cred_id}"))
            })?;
        let cred = serde_json::from_str::<Credential>(&raw)
            .map_err(|e| KernelError::Internal(format!("corrupted held credential: {e}")))?;
        verify_credential_static(&cred, Some(cred_id))
            .map_err(|e| KernelError::Internal(format!("held credential invalid: {e}")))?;

        // 域身份公钥必须就是凭证登记的 holder 公钥（持有者 = 本插件域身份）。
        let unlocked = self.unlocked.as_ref().ok_or(KernelError::Locked)?;
        let derived = crate::identity::derive_domain_identity(&unlocked.seed, domain);
        let derived_pub = B64.encode(derived.public_key());
        if derived_pub != cred.holder.public_key {
            return Err(KernelError::Internal(
                "holder key mismatch: credential is held by another identity".to_string(),
            ));
        }

        let presented_at = system_now_ms();
        let payload = holder_proof_payload(cred_id, request_id, org_id, collection, presented_at);
        let signature = self.sign_with_domain_identity(domain, &payload)?;
        let proof = HolderProof {
            cred_id: cred_id.to_string(),
            sig: signature.signature,
        };
        Ok(json!({
            "credential": cred,
            "holderProof": proof,
            "presentedAt": presented_at,
        }))
    }

    /// 查询验证人（sdk.credentials.queryVerifiers）：读 `org:verifiers:{orgId}`
    /// 信任声明，返回验证人授权集（身份 / 公钥 / 授权凭证类型 / 授权方法模式）。
    /// 声明缺失返回空集；结构损坏按错误上报（fail-closed，不把坏数据当空集）。
    pub fn credential_query_verifiers(&self, org_id: &str) -> Result<Value> {
        // orgId 双形态校验先于拼键（与 policy_read 同口径，非法形状直接拒绝）。
        if !crate::affair::is_valid_org_id(org_id) {
            return Err(KernelError::Internal("invalid orgId".to_string()));
        }
        let storage = self.require_storage()?;
        let Some(raw) = storage.get(&trust_decl_key(org_id))? else {
            return Ok(json!({ "orgId": org_id, "verifiers": [] }));
        };
        let decl = serde_json::from_str::<TrustDecl>(&raw)
            .map_err(|e| KernelError::Internal(format!("corrupted trust decl: {e}")))?;
        validate_trust_decl_structure(&decl)
            .map_err(|e| KernelError::Internal(format!("corrupted trust decl: {e}")))?;
        Ok(json!({
            "orgId": org_id,
            "effectiveFrom": decl.effective_from,
            "seq": decl.seq,
            "updatedAt": decl.updated_at,
            "verifiers": decl.verifiers,
        }))
    }

    /// 凭证验证（sdk.credentials.verify）：协议线形凭证的呈现验证链
    /// （credential §6 第 1–5 步：结构 → credId 复算 → 验签 → 签发人信任链
    /// （按 issuedAt 时刻，org:verifiers: 信任声明）→ 注销检查（cred:rev:
    /// 本地快照的头承诺 + 全量链复算））。holderProof 绑定（第 6 步）归
    /// read-gate，不在本门面。
    ///
    /// 返回结构化裁决而非整体报错——凭证来自不可信来源，逐项失败原因要
    /// 如实回显（fail-closed：任一环节不过 valid=false；注销快照缺失按
    /// revocation-unavailable 计，与 read-gate 同口径）。错误 kind 逐字
    /// 稳定（CredentialError::kind），插件可按码诊断。
    pub fn credential_verify(&self, credential: &Value) -> Result<Value> {
        let storage = self.require_storage()?;
        let now = system_now_ms();
        // 第 0 步：线形可解析性（非协议 Credential 形状 → structured invalid）
        let cred: Credential = match serde_json::from_value(credential.clone()) {
            Ok(cred) => cred,
            Err(e) => {
                return Ok(json!({
                    "credId": null,
                    "valid": false,
                    "checks": { "static": false, "trust": false, "revocation": "unavailable" },
                    "reason": format!("malformed-credential: {e}"),
                }));
            }
        };
        // 第 1–3 步：静态验证（结构 → credId 复算 → 验签；不随时间变化）
        if let Err(e) = verify_credential_static(&cred, None) {
            return Ok(json!({
                "credId": null,
                "valid": false,
                "checks": { "static": false, "trust": false, "revocation": "unavailable" },
                "reason": e.kind(),
            }));
        }
        let cred_id = credential_id(&cred)
            .map_err(|e| KernelError::Internal(format!("credential id failed: {e}")))?;
        // 第 4 步：签发人信任链（既往不咎时间线：按 issuedAt 时刻信任集）。
        // 声明缺失/损坏 → trust=false（fail-closed，不当作可信）。
        let decl: Option<TrustDecl> = storage
            .get(&trust_decl_key(&cred.subject_domain))?
            .and_then(|raw| serde_json::from_str(&raw).ok());
        let decls: Vec<&TrustDecl> = decl.iter().collect();
        let trust_ok = crate::credential::issuer_trusted_at(
            &decls,
            &cred.issuer.identity,
            &cred.cred_type,
            &cred.method,
            cred.issued_at,
        );
        // 第 5 步：注销检查（issuer 注销快照；缺失/链无效 → unavailable，
        // fail-closed 与 verify_credential_chain 的 RevocationUnavailable 同口径）。
        let revocation = match storage.get(&revocation_snapshot_key(&cred.issuer.identity))? {
            None => "unavailable",
            Some(raw) => match serde_json::from_str::<RevocationSnapshot>(&raw) {
                Ok(snap) => match verify_not_revoked(
                    &cred_id,
                    &snap.entries,
                    &snap.head,
                    &cred.issuer.public_key,
                    now,
                ) {
                    Ok(()) => "not-revoked",
                    Err(crate::credential::CredentialError::Revoked) => "revoked",
                    // 快照链/头承诺自身无效：不可据以判定，按不可用 fail-closed
                    Err(_) => "unavailable",
                },
                Err(_) => "unavailable",
            },
        };
        let valid = trust_ok && revocation == "not-revoked";
        let reason = if valid {
            Value::Null
        } else if !trust_ok {
            json!(crate::credential::CredentialError::IssuerNotTrusted.kind())
        } else if revocation == "revoked" {
            json!(crate::credential::CredentialError::Revoked.kind())
        } else {
            json!(crate::credential::CredentialError::RevocationUnavailable.kind())
        };
        Ok(json!({
            "credId": cred_id,
            "valid": valid,
            "checks": { "static": true, "trust": trust_ok, "revocation": revocation },
            "reason": reason,
        }))
    }

    /// 注销查询（sdk.credentials.queryRevocations）：按 issuer identity 读本地
    /// 注销快照（`cred:rev:` 键域；分发承载面未定，快照由分发渠道落地后写入）。
    /// 返回头承诺与条目清单（credId/seq/revokedAt/reason）；快照缺失如实报
    /// `available:false`（消费方按 fail-closed 取舍，不冒充「无注销」）。
    pub fn credential_query_revocations(&self, issuer: &str) -> Result<Value> {
        if !crate::affair::is_valid_identity_id(issuer) {
            return Err(KernelError::Internal("invalid issuer".to_string()));
        }
        let storage = self.require_storage()?;
        let Some(raw) = storage.get(&revocation_snapshot_key(issuer))? else {
            return Ok(json!({ "issuer": issuer, "available": false }));
        };
        let snap: RevocationSnapshot = serde_json::from_str(&raw)
            .map_err(|e| KernelError::Internal(format!("corrupted revocation snapshot: {e}")))?;
        Ok(json!({
            "issuer": issuer,
            "available": true,
            "headSeq": snap.head.head_seq,
            "headHash": snap.head.head_hash,
            "asOf": snap.head.as_of,
            "entries": snap.entries.iter().map(|entry| json!({
                "seq": entry.seq,
                "credId": entry.cred_id,
                "revokedAt": entry.revoked_at,
                "reason": entry.reason,
            })).collect::<Vec<_>>(),
        }))
    }
}

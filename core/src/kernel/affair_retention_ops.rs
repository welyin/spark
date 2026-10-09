//! 事务本体保留策略（retention）门面（affair-model §4.2，A22）：
//!
//! - **读面** [`Kernel::affair_retention_status`]：规则链 replay 取当前版本
//!   retention 声明 + 逐组织 pin 接受记录状态（none / pending / effective /
//!   revoked / corrupted）→ 双条件并集（事务声明 ∧ 组织接受 ∧ 已生效）；
//! - **组织侧发布** [`Kernel::org_pin_publish`]：`org:pin:{orgId}:{affairId}`
//!   接受/撤销声明（组织级动作，OrgSigSet 背书）——接受 = 摊派面扩大，走 24h
//!   公示延迟且须显式确认；撤销 = 收窄即时；
//! - **存留判定** [`Kernel::affair_body_hold`]：GC/清理面清扫事务本体前的
//!   必查挂钩（不杀最后副本原则）——followers 档只看关注副本；org-pinned 档
//!   关注 ∨（双条件生效 ∧ 本机是任一生效 pin 组织的数据节点）。判定失败
//!   （锁定/存储错误）向外报错，清理面须按 fail-safe 跳过回收（宁可多留）。

use serde_json::{Value, json};

use super::{Kernel, KernelError, Result};
use crate::affair::{
    ORG_PIN_PREFIX, PIN_PUB_PERIOD_MS, PIN_V, RetentionDecl, RetentionPolicy, effective_pin_orgs,
    is_valid_identity_id, is_valid_org_id, org_pin_key, parse_pin_record, pin_effective, pin_hash,
    pin_widening, static_check_rules, validate_pin_fields,
};
use crate::p2p::node::system_now_ms;
use crate::storage::{ScanOptions, StorageBackend};
use crate::sync::affairsync::is_following;

impl Kernel {
    /// 当前规则版本的 retention 声明（规则链 replay → §5.6 静态检查视图；
    /// 链上版本构建期已把关，此处失败为防御分支）。返回（声明， 求值时刻）。
    fn affair_retention_decl(&self, affair_id: &str) -> Result<(RetentionDecl, i64)> {
        let eval = self.affair_eval(affair_id)?;
        let doc = static_check_rules(&eval.chain.current().rules)
            .map_err(|e| KernelError::Internal(format!("corrupted rules: {}", e.reason())))?;
        Ok((doc.retention, eval.now_ms))
    }

    /// 读某组织对本事务的现行 pin 接受记录（键-文一致才回收；损坏 → None）。
    fn read_pin_record(
        &self,
        org_id: &str,
        affair_id: &str,
    ) -> Result<Option<crate::affair::OrgPinRecord>> {
        let key = org_pin_key(org_id, affair_id)
            .map_err(|e| KernelError::Internal(e.reason().to_string()))?;
        let Some(raw) = self.require_storage()?.get(&key)? else {
            return Ok(None);
        };
        let Ok(value) = serde_json::from_str::<Value>(&raw) else {
            return Ok(None); // 损坏记录按无 pin 处理（保留方向不受影响：见 hold 判定）
        };
        let Ok(record) = parse_pin_record(&value) else {
            return Ok(None);
        };
        if record.key != key {
            return Ok(None); // 键-文不符 fail-closed
        }
        Ok(Some(record))
    }

    /// 事务保留策略状态（sdk 读面）：当前 retention 声明 + 逐声明组织的 pin
    /// 状态 + 双条件并集结果。`effectivePinOrgs` 即 affairsync 复制组并集
    /// 语义的组织集合（其数据节点将本体纳入长期副本）。
    pub fn affair_retention_status(&self, affair_id: &str) -> Result<Value> {
        if !is_valid_identity_id(affair_id) {
            return Err(KernelError::Internal("invalid affairId".to_string()));
        }
        let (decl, now_ms) = self.affair_retention_decl(affair_id)?;
        let mut pins = Vec::new();
        let mut effective_records = Vec::new();
        for org_id in &decl.orgs {
            match self.read_pin_record(org_id, affair_id)? {
                None => pins.push(json!({ "orgId": org_id, "state": "none" })),
                Some(record) => {
                    let state = if record.revoked {
                        "revoked"
                    } else if record.effective_at > now_ms {
                        "pending" // 公示窗内（发布即公示 ≠ 即时生效）
                    } else {
                        "effective"
                    };
                    if pin_effective(&record, now_ms) {
                        effective_records.push(record.clone());
                    }
                    pins.push(json!({
                        "orgId": org_id,
                        "state": state,
                        "version": record.version,
                        "updatedAt": record.updated_at,
                        "effectiveAt": record.effective_at,
                    }));
                }
            }
        }
        let effective_refs: Vec<&crate::affair::OrgPinRecord> = effective_records.iter().collect();
        let effective_orgs = effective_pin_orgs(&decl.orgs, &effective_refs, now_ms);
        Ok(json!({
            "affairId": affair_id,
            "policy": decl.policy.as_str(),
            "declaredOrgs": decl.orgs,
            "pins": pins,
            "effectivePinOrgs": effective_orgs,
            "nowMs": now_ms,
        }))
    }

    /// 组织 pin 接受声明发布/撤销（组织级动作，本机须属主组织 admin）：
    /// `org:pin:{orgId}:{affairId}` = { pinV, orgId, affairId, version,
    /// updatedAt, effectiveAt, revoked, OrgSigSet }。
    ///
    /// **公示延迟**（affair-model §4.2「与 org:disclosure 同族」）：接受长期
    /// 副本义务 = 副本摊派面扩大方向（首次接受 / 撤销后再接受）
    /// `effectiveAt = updatedAt + 24h`，且须显式确认（`confirm_widening`）；
    /// 撤销 = 收窄即时生效。发布即公示（org:structure@v1 受管键域随 orgsync
    /// 全员流动）；入站合入点
    /// [`crate::org::service::adjudicate_incoming_pin`] 以同一五步链把关。
    ///
    /// 发布不校验事务侧 retention 声明——pin 是组织自己的承诺，双条件
    /// （声明 ∧ 接受）在求值侧（[`Self::affair_retention_status`] /
    /// [`Self::affair_body_hold`]）取交集生效。
    pub fn org_pin_publish(
        &mut self,
        org_id: &str,
        affair_id: &str,
        revoked: bool,
        confirm_widening: bool,
    ) -> Result<Value> {
        if !is_valid_org_id(org_id) || !is_valid_identity_id(affair_id) {
            return Err(KernelError::Internal("invalid orgId/affairId".to_string()));
        }
        let now = system_now_ms();
        let key = org_pin_key(org_id, affair_id)
            .map_err(|e| KernelError::Internal(e.reason().to_string()))?;
        let prev = self.read_pin_record(org_id, affair_id)?;
        let widening = pin_widening(prev.as_ref(), revoked);
        if widening && !confirm_widening {
            return Err(KernelError::Internal(
                "接受事务本体长期副本义务（副本摊派面扩大）须显式确认后发布".to_string(),
            ));
        }
        let effective_at = if widening { now + PIN_PUB_PERIOD_MS } else { now };
        let mut value = json!({
            "pinV": PIN_V,
            "orgId": org_id,
            "affairId": affair_id,
            "version": prev.as_ref().map_or(1, |p| p.version + 1),
            "updatedAt": now,
            "effectiveAt": effective_at,
        });
        if revoked {
            value["revoked"] = json!(true);
        }
        validate_pin_fields(&value)
            .map_err(|e| KernelError::Internal(format!("pin validate failed: {}", e.reason())))?;
        let hash = pin_hash(&value);
        let (sig_set, signer_id, _degraded) = self.build_signed_org_sig_set(org_id, &hash)?;
        value["sigSet"] =
            serde_json::to_value(&sig_set).map_err(|e| KernelError::Internal(e.to_string()))?;
        // 防御：签名后整记录须过完整解析（含 sigSet 存在性）
        parse_pin_record(&value)
            .map_err(|e| KernelError::Internal(format!("pin recheck failed: {}", e.reason())))?;
        let raw = serde_json::to_string(&value)
            .map_err(|e| KernelError::Internal(format!("pin serialize failed: {e}")))?;
        self.require_storage_mut()?.put(&key, &raw)?;
        Ok(json!({
            "orgId": org_id,
            "affairId": affair_id,
            "pinHash": hash,
            "version": value["version"],
            "widening": widening,
            "revoked": revoked,
            "updatedAt": now,
            "effectiveAt": effective_at,
            "signer": signer_id,
        }))
    }

    /// 事务本体存留判定（不杀最后副本原则挂钩）：GC/清理面清扫事务本体前
    /// 必查——返回 true 一律不得回收本机副本。
    ///
    /// - followers 档（缺省）：只看关注者副本——本机关注即保留，取关即可回收；
    /// - org-pinned 档：关注 ∨（双条件生效 ∧ 本机是任一生效 pin 组织的数据
    ///   节点（A14 全员数据节点：成员即数据节点））；
    /// - 保守分支：生效 pin 记录在手但组织记录缺失（无法证伪成员资格）→
    ///   保留（能收到该组织 pin 公示流量本身即复制组成员的强信号）。
    pub fn affair_body_hold(&self, affair_id: &str) -> Result<bool> {
        if !is_valid_identity_id(affair_id) {
            return Err(KernelError::Internal("invalid affairId".to_string()));
        }
        let storage = self.require_storage()?;
        if is_following(storage, affair_id).map_err(|e| KernelError::Internal(e.to_string()))? {
            return Ok(true); // 关注即副本（两档通用）
        }
        let (decl, now_ms) = self.affair_retention_decl(affair_id)?;
        if decl.policy == RetentionPolicy::Followers {
            return Ok(false); // followers 档：取关后本机副本可消亡
        }
        let my_root = self.require_current_root_id()?;
        for org_id in &decl.orgs {
            let Some(record) = self.read_pin_record(org_id, affair_id)? else {
                continue;
            };
            if !pin_effective(&record, now_ms) {
                continue; // 公示窗内 / 已撤销不生效力
            }
            let org_record =
                crate::org::service::OrganizationService::get_record(storage, org_id)
                    .map_err(|e| KernelError::Internal(e.to_string()))?;
            match org_record {
                Some(org_record) => {
                    if crate::org::roles::is_data_node(&org_record, &my_root) {
                        return Ok(true);
                    }
                }
                // 组织记录缺失：无法证伪成员资格 → 保留（fail-safe）
                None => return Ok(true),
            }
        }
        Ok(false)
    }

    /// 组织现行 pin 接受声明列表（accept 侧视图；键-文一致才回收，损坏跳过，
    /// 按 affairId 字典序）。
    pub fn org_pin_list(&self, org_id: &str) -> Result<Vec<Value>> {
        if !is_valid_org_id(org_id) {
            return Err(KernelError::Internal("invalid orgId".to_string()));
        }
        let now = system_now_ms();
        let storage = self.require_storage()?;
        let prefix = format!("{ORG_PIN_PREFIX}{org_id}:");
        let mut out = Vec::new();
        for (key, raw) in storage.scan(&ScanOptions::prefix(&prefix))? {
            let Ok(value) = serde_json::from_str::<Value>(&raw) else {
                continue;
            };
            let Ok(record) = parse_pin_record(&value) else {
                continue;
            };
            if record.key != key {
                continue; // 键-文不符 fail-closed
            }
            out.push(json!({
                "affairId": record.affair_id,
                "version": record.version,
                "revoked": record.revoked,
                "updatedAt": record.updated_at,
                "effectiveAt": record.effective_at,
                "effective": pin_effective(&record, now),
            }));
        }
        out.sort_by(|a, b| a["affairId"].as_str().cmp(&b["affairId"].as_str()));
        Ok(out)
    }
}

#[cfg(test)]
#[path = "affair_retention_ops_tests.rs"]
mod tests;

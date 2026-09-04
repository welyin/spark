//! 跨组织网关邮箱编排（阶段四E，org-gateway-mailbox §2 + p2p-org-mail §21）：
//! 发送方直连目标网关投递 + 收件人两轮挑战拉取（orgsync hello/tick 节奏
//! 触发）+ 网关入站处理（deliver/fetch 两 op）。
//!
//! 最小形态边界（设计 §6）：无投递回执、无取信推送通知、无多网关副本
//! 同步、无呈现层（收信落 `orgmail:in:` 本地键，呈现归 UI 批次）。

use serde_json::{Value, json};

use super::{Kernel, KernelError, Result};
use crate::org::mailbox::{
    OrgMailEnvelope, OrgMailFrom, OrgMailTo, domain_id_of, new_envelope_id, normalize_ttl,
    org_mail_domain, orgmail_box, orgmail_sign,
};
use crate::org::mailbox_store::{
    fetch_challenge_sign, gateway_deliver, gateway_fetch, gateway_fetch_challenge, inbox_key,
};
use crate::p2p::node::system_now_ms;
use crate::p2p::peer_targets::PeerNodeInfo;
use crate::storage::StorageBackend;

impl Kernel {
    /// 发送方编排（§21.5 deliver）：构造签名信封 → 解析目标组织地址记录
    ///（验签 + 取 gateways）→ 逐网关解析端点直连投递。返回首个网关的应答
    /// JSON（全部不可达 → Err）。
    ///
    /// 网关端点解析：① 显式 `gateway_hint`（带外名片/二维码通道的输入口，
    /// 测试同径）② 我的朋友记录 peers（目标网关是我的联系人时）。DHT 节点
    /// 存在记录按 libp2p peerId 定键，而地址记录 gateways 是 rootId——
    /// rootId→peerId 映射无公共索引，DHT 解析挂账（报告偏差项）。
    pub fn org_mail_send(
        &mut self,
        from_org_id: &str,
        to_org_address: &str,
        recipient_domain_id: &str,
        payload: &Value,
        gateway_hint: Option<&PeerNodeInfo>,
    ) -> Result<Value> {
        let now = system_now_ms();
        // 目标组织地址记录：验签（自认证 + 有效期）
        let record: crate::org::OrgAddressRecord = serde_json::from_str(to_org_address)
            .map_err(|e| KernelError::Internal(format!("invalid to orgAddress: {e}")))?;
        if !crate::org::verify_org_address_record(&record, now).is_ok() {
            return Err(KernelError::Internal("orgAddress 记录验签失败".to_string()));
        }
        // 发送方域身份（org-mail:{fromOrgId}，双层身份红线：rootId 不出线）
        let seed = self
            .unlocked
            .as_ref()
            .map(|u| u.seed)
            .ok_or(KernelError::Locked)?;
        let sender = crate::identity::derive_domain_identity(
            &seed,
            &org_mail_domain(from_org_id),
        );
        // 信封 to.orgAddress = 地址记录**完整线形**（网关归属判定要重解析验签
        // + 取 gateways，§21.2；不是 55 字符地址串）
        let record_json = serde_json::to_string(&record)
            .map_err(|e| KernelError::Internal(format!("orgAddress 记录序列化失败: {e}")))?;
        let plain = payload.to_string();
        let id = new_envelope_id();
        let (nonce, ct) = orgmail_box(
            plain.as_bytes(),
            &sender.signing_key,
            recipient_domain_id,
            &record_json,
            &id,
        )
        .ok_or_else(|| KernelError::Internal("org-mail box failed".to_string()))?;
        let mut envelope = OrgMailEnvelope {
            id,
            to: OrgMailTo {
                org_address: record_json,
                domain_id: recipient_domain_id.to_string(),
            },
            from: OrgMailFrom {
                domain_id: domain_id_of(&sender.signing_key.verifying_key()),
                org_address: None,
            },
            ts: now,
            ttl: normalize_ttl(0),
            nonce,
            ct,
            sig: String::new(),
        };
        envelope.sig = orgmail_sign(&sender.signing_key, &envelope);

        // 网关端点解析 + 直连投递（首个成功即止；全部失败 → Err）
        let request = json!({ "op": "deliver", "envelope": envelope });
        let mut targets: Vec<PeerNodeInfo> = Vec::new();
        if let Some(hint) = gateway_hint {
            targets.push(hint.clone());
        }
        for gateway_root in &record.gateways {
            if let Ok(Some(friend)) =
                crate::contact::ContactService::get_friend(self.require_storage()?, gateway_root)
            {
                for p in friend.peers {
                    if !p.peer_id.is_empty() || !p.addresses.is_empty() {
                        targets.push(PeerNodeInfo {
                            peer_id: (!p.peer_id.is_empty()).then_some(p.peer_id),
                            addresses: p.addresses,
                        });
                    }
                }
            }
        }
        if targets.is_empty() {
            return Err(KernelError::Internal(
                "无法解析目标组织网关端点（需带外名片通道或联系人记录）".to_string(),
            ));
        }
        let node = self.p2p.clone().ok_or(crate::p2p::P2pError::NotStarted)?;
        let mut last_err = None;
        for target in &targets {
            match self
                .runtime
                .handle()
                .block_on(node.org_mail_request(target, &request.to_string()))
            {
                Ok(Some(resp)) => return Ok(resp),
                Ok(None) => last_err = Some("no response".to_string()),
                Err(e) => last_err = Some(e.to_string()),
            }
        }
        Err(KernelError::Internal(format!(
            "org-mail deliver failed: {}",
            last_err.unwrap_or_default()
        )))
    }

    /// 收件人编排（§21.5 fetch 两轮 + §21.6 取信即删）：本机为成员的每个
    /// 组织 → 逐活跃网关（显式 gateways，缺省推导活跃集，取成员表端点）
    /// 两轮挑战拉取 → 落 `orgmail:in:`（解箱在呈现层——先落原始信封，
    /// 明文由读取方按需 orgmail_unbox）。返回收取信封数。
    pub fn org_mail_fetch(&mut self) -> Result<usize> {
        let Ok(Some(root_id)) = self.current_root_id() else {
            return Ok(0);
        };
        let Some(seed) = self.unlocked.as_ref().map(|u| u.seed) else {
            return Ok(0);
        };
        let Some(node) = self.p2p.clone() else {
            return Ok(0);
        };
        let storage = self.require_storage()?.clone();
        self.runtime
            .handle()
            .block_on(org_mail_fetch_async(storage, node, seed, root_id))
    }

    /// spawn 一轮后台拉取（start_p2p 成功 / orgsync tick 触发点用）。
    /// 句柄缺失/未解锁时静默跳过（尽力而为）。
    pub(crate) fn spawn_org_mail_fetch(&self) {
        let (Some(storage), Some(node)) = (self.storage.clone(), self.p2p.clone()) else {
            return;
        };
        let Some(seed) = self.unlocked.as_ref().map(|u| u.seed) else {
            return;
        };
        let Ok(Some(root_id)) = self.current_root_id() else {
            return;
        };
        self.runtime.handle().spawn(async move {
            let _ = org_mail_fetch_async(storage, node, seed, root_id).await;
        });
    }
}

/// org_mail_fetch 的异步实现（worker/门面共用）：句柄全部克隆传入。
pub(crate) async fn org_mail_fetch_async(
    storage: super::KernelStorage,
    node: std::sync::Arc<crate::p2p::P2pNode>,
    seed: [u8; 64],
    root_id: String,
) -> Result<usize> {
    let now = system_now_ms();
    let mut fetched = 0usize;
    for record in crate::org::OrganizationService::read_all_organizations(&storage)? {
            if record.find_member(&root_id).is_none() {
                continue;
            }
            // 私有组织不参与（不发布地址记录即无邮箱入口）；地址记录本身
            // 不在本地 org 记录内——邮箱存在性以「有网关可拉」判定
            let domain = org_mail_domain(&record.org_id);
            let identity = crate::identity::derive_domain_identity(&seed, &domain);
            let my_domain_id = domain_id_of(&identity.signing_key.verifying_key());
            // 活跃网关 = 显式 gateways，缺省推导活跃集（roles::gateway_active_set）
            let gateways = if record.gateways.is_empty() {
                crate::org::roles::gateway_active_set(&record, now)
            } else {
                record.gateways.clone()
            };
            for gateway_root in gateways {
                if gateway_root == root_id {
                    continue; // 自己是网关：信在本机箱内，拉取无意义（本地直读另行）
                }
                let Some(member) = record.find_member(&gateway_root) else {
                    continue;
                };
                let Some(set) = &member.node_info else {
                    continue;
                };
                for endpoint in set.iter() {
                    let target = PeerNodeInfo {
                        peer_id: endpoint.peer_id.clone(),
                        addresses: endpoint.addresses.clone(),
                    };
                    let gateway_peer_id = endpoint.peer_id.clone().unwrap_or_default();
                    // 第一轮：取挑战
                    let round1 = json!({ "op": "fetch", "recipientDomainId": my_domain_id });
                    let Ok(Some(resp)) =
                        node.org_mail_request(&target, &round1.to_string()).await
                    else {
                        continue;
                    };
                    if resp.get("ok").and_then(Value::as_bool) != Some(true) {
                        continue;
                    }
                    let Some(nonce) = resp.get("nonce").and_then(Value::as_str) else {
                        continue;
                    };
                    let Some(challenge_ts) = resp.get("ts").and_then(Value::as_i64) else {
                        continue;
                    };
                    // 第二轮：挑战应答（载荷绑 nonce + 网关 peerId + ts，防跨网关/重放）
                    let challenge = fetch_challenge_sign(
                        &identity.signing_key,
                        nonce,
                        &gateway_peer_id,
                        challenge_ts,
                    );
                    let round2 = json!({
                        "op": "fetch",
                        "recipientDomainId": my_domain_id,
                        "nonce": nonce,
                        "challengeTs": challenge_ts,
                        "challenge": challenge,
                    });
                    let Ok(Some(resp2)) =
                        node.org_mail_request(&target, &round2.to_string()).await
                    else {
                        continue;
                    };
                    if resp2.get("ok").and_then(Value::as_bool) != Some(true) {
                        continue;
                    }
                    let Some(envelopes) = resp2.get("envelopes").and_then(Value::as_array) else {
                        continue;
                    };
                    for value in envelopes {
                        let Ok(env) = serde_json::from_value::<OrgMailEnvelope>(value.clone())
                        else {
                            continue;
                        };
                        // 收信落本地（解箱在呈现层之前——先落原始信封，明文由
                        // 读取方按需 orgmail_unbox；这里只做写入，幂等键 = id）
                        let key = inbox_key(&my_domain_id, &env.id);
                        if storage.get(&key)?.is_none() {
                            storage.clone().put(&key, &serde_json::to_string(&env)?)?;
                            fetched += 1;
                        }
                    }
                }
            }
        }
        Ok(fetched)
}

/// 网关入站处理（KernelHost `handle_org_mail` 的实现体；存储 + 本机身份 +
/// 连接层 peerId 注入，纯同步存储 IO）。
pub fn handle_org_mail_inbound(
    storage: &mut impl StorageBackend,
    my_root_id: Option<&str>,
    my_peer_id: &str,
    payload: &Value,
    _remote_peer_id: &str,
) -> std::result::Result<Value, String> {
    let now = system_now_ms();
    let Some(op) = payload.get("op").and_then(Value::as_str) else {
        return Ok(json!({ "ok": false, "reason": "invalid-request" }));
    };
    match op {
        "deliver" => {
            let Some(my_root_id) = my_root_id else {
                return Ok(json!({ "ok": false, "reason": "wrong-org" }));
            };
            let Some(env_value) = payload.get("envelope") else {
                return Ok(json!({ "ok": false, "reason": "invalid-envelope" }));
            };
            let Ok(envelope) = serde_json::from_value::<OrgMailEnvelope>(env_value.clone())
            else {
                return Ok(json!({ "ok": false, "reason": "invalid-envelope" }));
            };
            Ok(gateway_deliver(storage, &envelope, now, my_root_id))
        }
        "fetch" => {
            let Some(recipient) = payload.get("recipientDomainId").and_then(Value::as_str)
            else {
                return Ok(json!({ "ok": false, "reason": "invalid-request" }));
            };
            let (nonce, challenge_ts, challenge) = (
                payload.get("nonce").and_then(Value::as_str),
                payload.get("challengeTs").and_then(Value::as_i64),
                payload.get("challenge").and_then(Value::as_str),
            );
            match (nonce, challenge_ts, challenge) {
                (None, None, None) => Ok(gateway_fetch_challenge(storage, recipient, now)),
                (Some(nonce), Some(ts), Some(challenge)) => Ok(gateway_fetch(
                    storage,
                    recipient,
                    nonce,
                    ts,
                    challenge,
                    my_peer_id,
                    now,
                )),
                _ => Ok(json!({ "ok": false, "reason": "invalid-request" })),
            }
        }
        _ => Ok(json!({ "ok": false, "reason": "invalid-request" })),
    }
}

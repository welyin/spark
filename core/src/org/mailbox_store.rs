//! 网关邮箱存储面（阶段四E，p2p-org-mail §21.5/§21.6）：投递校验管线 +
//! 配额/TTL/幂等 + 惰性清扫 + 两轮挑战拉取（取信即删）。
//!
//! 网关只见路由元数据（§21.2 网关可见面），内容不可读；不校验收发双方
//! 组织成员资格（内容真伪由收件人解密+验签把关，设计 §2.4）。
//!
//! 存储键族（网关本地，不进任何同步流量）：
//! - `orgmail:box:{orgId}:{envelopeId}` → 信封 JSON 原样（邮箱本体）；
//! - `orgmail:rl:deliver:{orgId}:{fromDomainId}` / `orgmail:rl:fetch:{orgId}:
//!   {recipientDomainId}` → 上次通过时刻（限流，最小间隔 1s）；
//! - `orgmail:chal:{nonce}` → `{ts, recipientDomainId}`（挑战 nonce，TTL 60s
//!   一次性，用后即焚）。
//! 收件人侧收信落 `orgmail:in:{domainId}:{envelopeId}`（本地键，呈现层归
//! UI 批次）。

use serde_json::{Value, json};

use crate::storage::{ScanOptions, StorageBackend};

use super::mailbox::{
    MAIL_TS_FRESHNESS_WINDOW_MS, OrgMailEnvelope, normalize_ttl, orgmail_verify, parse_domain_id,
};

/// 每组织邮箱容量上限（条）。
pub const MAIL_MAX_PER_ORG: usize = 1000;
/// 每组织邮箱字节上限（10MB，信封 JSON 字节合计近似）。
pub const MAIL_MAX_BYTES_PER_ORG: usize = 10 * 1024 * 1024;
/// 每收件人条数上限（按 to.domainId 计）。
pub const MAIL_MAX_PER_RECIPIENT: usize = 100;
/// 投递/拉取限流：同一来源/收件人最小间隔（ms）。
pub const MAIL_RATE_MIN_INTERVAL_MS: i64 = 1000;
/// 挑战 nonce TTL（60s，一次性）。
pub const MAIL_CHALLENGE_TTL_MS: i64 = 60_000;

/// 邮箱本体键（§21.5）。
pub fn mailbox_key(org_id: &str, envelope_id: &str) -> String {
    format!("orgmail:box:{org_id}:{envelope_id}")
}

/// 收件人侧收信键（本地；呈现层归 UI 批次）。
pub fn inbox_key(domain_id: &str, envelope_id: &str) -> String {
    format!("orgmail:in:{domain_id}:{envelope_id}")
}

fn rl_deliver_key(org_id: &str, from_domain_id: &str) -> String {
    format!("orgmail:rl:deliver:{org_id}:{from_domain_id}")
}

fn rl_fetch_key(org_id: &str, recipient_domain_id: &str) -> String {
    format!("orgmail:rl:fetch:{org_id}:{recipient_domain_id}")
}

fn challenge_key(nonce: &str) -> String {
    format!("orgmail:chal:{nonce}")
}

/// 惰性过期清扫（§21.6：随网关读写时点执行，不设独立定时器）——
/// `ts + ttl <= now` 的信封删除。返回清扫条数（观测用）。
pub fn sweep_expired<S: StorageBackend>(storage: &mut S, now_ms: i64) -> Result<usize, String> {
    let mut expired = Vec::new();
    for (key, raw) in storage
        .scan(&ScanOptions::prefix("orgmail:box:"))
        .map_err(|e| e.to_string())?
    {
        let Ok(env) = serde_json::from_str::<OrgMailEnvelope>(&raw) else {
            continue;
        };
        if env.ts + normalize_ttl(env.ttl) <= now_ms {
            expired.push(key);
        }
    }
    let n = expired.len();
    for key in expired {
        storage.delete(&key).map_err(|e| e.to_string())?;
    }
    Ok(n)
}

/// 信封形状校验（§21.5「形状」步）：解析 + id 24hex + domainId/nonce/ct
/// b64 形状 + ttl 归一（写回截断值）。
fn validate_shape(envelope: &OrgMailEnvelope) -> Result<(), &'static str> {
    let id_ok = envelope.id.len() == 24
        && envelope
            .id
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase());
    if !id_ok {
        return Err("invalid-envelope");
    }
    if parse_domain_id(&envelope.to.domain_id).is_none()
        || parse_domain_id(&envelope.from.domain_id).is_none()
    {
        return Err("invalid-envelope");
    }
    let b64 = base64::Engine::decode;
    let nonce_ok = b64(&base64::engine::general_purpose::STANDARD, &envelope.nonce)
        .map(|v| v.len() == 12)
        .unwrap_or(false);
    let ct_ok = b64(&base64::engine::general_purpose::STANDARD, &envelope.ct).is_ok();
    let sig_ok = b64(&base64::engine::general_purpose::STANDARD, &envelope.sig)
        .map(|v| v.len() == 64)
        .unwrap_or(false);
    if !nonce_ok || !ct_ok || !sig_ok {
        return Err("invalid-envelope");
    }
    Ok(())
}

/// 投递（§21.5 deliver）：处理顺序 = 形状 → ts 新鲜窗 → 签名 → wrong-org →
/// 限流 → 幂等去重 → 配额 → 落库。返回应答 JSON（`{ok:true}` 或
/// `{ok:false, reason}`；内部异常文本原样，对齐 §9.1 既有口径）。
///
/// 归属判定（wrong-org）：`to.orgAddress` 解析为自认证组织地址记录并验签
///（形状/签名/有效期）→ 其 `gateways` 字段须含本机 rootId（A9 后地址记录
/// 携带的是发布方当下计分推导的履职集快照——发送方看到哪个记录就用哪个）
/// 且本机本地持有该组织且为成员。活跃集漂移容忍：本地推导的履职集与信封
/// 记录不一致不拦（design §5 网关活跃集漂移——入口以信封所附记录为准）。
pub fn gateway_deliver<S: StorageBackend>(
    storage: &mut S,
    envelope: &OrgMailEnvelope,
    now_ms: i64,
    my_root_id: &str,
) -> Value {
    gateway_deliver_inner(storage, envelope, now_ms, my_root_id)
        .unwrap_or_else(|reason| json!({ "ok": false, "reason": reason }))
}

fn gateway_deliver_inner<S: StorageBackend>(
    storage: &mut S,
    envelope: &OrgMailEnvelope,
    now_ms: i64,
    my_root_id: &str,
) -> Result<Value, String> {
    // 1. 形状
    validate_shape(envelope).map_err(str::to_string)?;
    // 2. ts 新鲜窗（同 dm 信封口径）
    if (envelope.ts - now_ms).abs() > MAIL_TS_FRESHNESS_WINDOW_MS {
        return Err("invalid-envelope".to_string());
    }
    // 3. 发送方签名（自包含验签键 from.domainId）
    if !orgmail_verify(envelope) {
        return Err("invalid-envelope".to_string());
    }
    // 4. 归属本组织：解析 + 验签 to.orgAddress（自认证地址记录）→ 其
    //    gateways 含本机 ∧ 本机本地持有该组织且为成员
    let record: crate::org::org_address::OrgAddressRecord =
        serde_json::from_str(&envelope.to.org_address)
            .map_err(|_| "invalid-envelope".to_string())?;
    let hosted = crate::org::org_address::verify_org_address_record(&record, now_ms).is_ok()
        && record.gateways.iter().any(|g| g == my_root_id)
        && crate::org::OrganizationService::get_record(storage, &record.org_id)
            .ok()
            .flatten()
            .is_some_and(|local| local.find_member(my_root_id).is_some());
    if !hosted {
        return Err("wrong-org".to_string());
    }
    // 惰性清扫随写时点执行
    let _ = sweep_expired(storage, now_ms);
    // 5. 投递限流（按 from.domainId，最小间隔 1s）
    let rl_key = rl_deliver_key(&record.org_id, &envelope.from.domain_id);
    if let Some(last) = storage
        .get(&rl_key)
        .map_err(|e| e.to_string())?
        .and_then(|raw| raw.parse::<i64>().ok())
        && now_ms - last < MAIL_RATE_MIN_INTERVAL_MS
    {
        return Err("rate-limited".to_string());
    }
    // 6. 幂等去重（同 id 已有 → ok:true 不重复落库）
    let key = mailbox_key(&record.org_id, &envelope.id);
    if storage.get(&key).map_err(|e| e.to_string())?.is_some() {
        return Ok(json!({ "ok": true }));
    }
    // 7. 配额（组织条数/字节 + 每收件人条数）
    let mut org_count = 0usize;
    let mut org_bytes = 0usize;
    let mut recipient_count = 0usize;
    for (_, raw) in storage
        .scan(&ScanOptions::prefix(format!(
            "orgmail:box:{}:",
            record.org_id
        )))
        .map_err(|e| e.to_string())?
    {
        org_count += 1;
        org_bytes += raw.len();
        let Ok(existing) = serde_json::from_str::<OrgMailEnvelope>(&raw) else {
            continue;
        };
        if existing.to.domain_id == envelope.to.domain_id {
            recipient_count += 1;
        }
    }
    if org_count >= MAIL_MAX_PER_ORG
        || org_bytes >= MAIL_MAX_BYTES_PER_ORG
        || recipient_count >= MAIL_MAX_PER_RECIPIENT
    {
        return Err("quota".to_string());
    }
    // 8. 落库（值 = 信封 JSON 原样；ttl 归一截断写回）+ 限流记账
    let mut stored = envelope.clone();
    stored.ttl = normalize_ttl(stored.ttl);
    storage
        .batch(vec![
            crate::storage::BatchOperation::put(
                key,
                serde_json::to_string(&stored).map_err(|e| e.to_string())?,
            ),
            crate::storage::BatchOperation::put(rl_key, now_ms.to_string()),
        ])
        .map_err(|e| e.to_string())?;
    Ok(json!({ "ok": true }))
}

// ---------------------------------------------------------------------------
// 拉取（§21.5 fetch，两轮挑战握手）
// ---------------------------------------------------------------------------

/// 挑战 nonce 签发（第一轮）：16B 随机 b64 + 在册登记（TTL 60s）。
/// `gateway_peer_id` 不进 nonce 记录——它经挑战签名载荷绑定（§21.5）。
pub fn gateway_fetch_challenge<S: StorageBackend>(
    storage: &mut S,
    recipient_domain_id: &str,
    now_ms: i64,
) -> Value {
    if parse_domain_id(recipient_domain_id).is_none() {
        return json!({ "ok": false, "reason": "invalid-request" });
    }
    let mut nonce = [0u8; 16];
    rand::Rng::fill_bytes(&mut rand::rng(), &mut nonce);
    let nonce_b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, nonce);
    let record = json!({ "ts": now_ms, "recipientDomainId": recipient_domain_id });
    if storage
        .put(&challenge_key(&nonce_b64), &record.to_string())
        .is_err()
    {
        return json!({ "ok": false, "reason": "internal-error" });
    }
    json!({ "ok": true, "phase": "challenge", "nonce": nonce_b64, "ts": now_ms })
}

/// 挑战签名载荷（§21.5）：`orgmail-fetch\x00{nonce}\x00{gatewayPeerId}\x00
/// {challengeTs}`（nonce 为 b64 字符串原样；challengeTs 十进制 ASCII）。
pub fn fetch_challenge_payload(nonce: &str, gateway_peer_id: &str, challenge_ts: i64) -> String {
    format!("orgmail-fetch\0{nonce}\0{gateway_peer_id}\0{challenge_ts}")
}

/// 挑战签名（收件人侧）：域身份私钥签载荷 → b64。
pub fn fetch_challenge_sign(
    signing_key: &ed25519_dalek::SigningKey,
    nonce: &str,
    gateway_peer_id: &str,
    challenge_ts: i64,
) -> String {
    use ed25519_dalek::Signer as _;
    let payload = fetch_challenge_payload(nonce, gateway_peer_id, challenge_ts);
    base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        signing_key.sign(payload.as_bytes()).to_bytes(),
    )
}

/// 取信（第二轮）：nonce 在册未过期未用 → 挑战验签 → 域名匹配 →
/// 取信即删（响应与删除同事务）。网关扫全部在箱组织（收件人域身份按组织
/// 派生，domainId 天然跨组织不撞）。
#[allow(clippy::too_many_arguments)]
pub fn gateway_fetch<S: StorageBackend>(
    storage: &mut S,
    recipient_domain_id: &str,
    nonce: &str,
    challenge_ts: i64,
    challenge: &str,
    gateway_peer_id: &str,
    now_ms: i64,
) -> Value {
    gateway_fetch_inner(
        storage,
        recipient_domain_id,
        nonce,
        challenge_ts,
        challenge,
        gateway_peer_id,
        now_ms,
    )
    .unwrap_or_else(|reason| json!({ "ok": false, "reason": reason }))
}

#[allow(clippy::too_many_arguments)]
fn gateway_fetch_inner<S: StorageBackend>(
    storage: &mut S,
    recipient_domain_id: &str,
    nonce: &str,
    challenge_ts: i64,
    challenge: &str,
    gateway_peer_id: &str,
    now_ms: i64,
) -> Result<Value, String> {
    // 形状
    let Some(recipient_pk_raw) = parse_domain_id(recipient_domain_id) else {
        return Err("invalid-request".to_string());
    };
    let Ok(challenge_sig) =
        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, challenge)
    else {
        return Err("invalid-challenge".to_string());
    };
    // nonce 在册且未过期未用（用后即焚：先取出再删，任何后续路径都不再放回）
    let chal_key = challenge_key(nonce);
    let Some(chal_raw) = storage.get(&chal_key).map_err(|e| e.to_string())? else {
        return Err("invalid-challenge".to_string());
    };
    storage.delete(&chal_key).map_err(|e| e.to_string())?;
    let chal_ts = serde_json::from_str::<Value>(&chal_raw)
        .ok()
        .and_then(|v| v.get("ts").and_then(Value::as_i64))
        .ok_or("invalid-challenge")?;
    if now_ms - chal_ts > MAIL_CHALLENGE_TTL_MS {
        return Err("invalid-challenge".to_string());
    }
    // 拉取限流（按 recipientDomainId，最小间隔 1s）——任一在箱组织键位即
    // 命中（网关本地记账按组织分键，限流语义按收件人；扫全部键位取最近值）
    let mut last_fetch = 0i64;
    for (key, raw) in storage
        .scan(&ScanOptions::prefix("orgmail:rl:fetch:"))
        .map_err(|e| e.to_string())?
    {
        if key.ends_with(&format!(":{recipient_domain_id}")) {
            if let Ok(ts) = raw.parse::<i64>() {
                last_fetch = last_fetch.max(ts);
            }
        }
    }
    if last_fetch > 0 && now_ms - last_fetch < MAIL_RATE_MIN_INTERVAL_MS {
        return Err("rate-limited".to_string());
    }
    // 挑战验签（验签键 = recipientDomainId 解码公钥）
    let Ok(recipient_pk) = ed25519_dalek::VerifyingKey::from_bytes(&recipient_pk_raw) else {
        return Err("invalid-challenge".to_string());
    };
    let Ok(sig_arr) = <[u8; 64]>::try_from(challenge_sig.as_slice()) else {
        return Err("invalid-challenge".to_string());
    };
    let payload = fetch_challenge_payload(nonce, gateway_peer_id, challenge_ts);
    use ed25519_dalek::Verifier as _;
    if recipient_pk
        .verify(
            payload.as_bytes(),
            &ed25519_dalek::Signature::from_bytes(&sig_arr),
        )
        .is_err()
    {
        return Err("invalid-challenge".to_string());
    }
    // 惰性清扫随读时点执行
    let _ = sweep_expired(storage, now_ms);
    // 域名匹配收集 + 取信即删（同一 batch）
    let mut envelopes = Vec::new();
    let mut ops = Vec::new();
    for (key, raw) in storage
        .scan(&ScanOptions::prefix("orgmail:box:"))
        .map_err(|e| e.to_string())?
    {
        let Ok(env) = serde_json::from_str::<OrgMailEnvelope>(&raw) else {
            continue;
        };
        if env.to.domain_id == recipient_domain_id {
            envelopes.push(env);
            ops.push(crate::storage::BatchOperation::delete(key));
        }
    }
    if !ops.is_empty() {
        storage.batch(ops).map_err(|e| e.to_string())?;
    }
    // 限流记账（按网关视角的组织归属逐信封记同一收件人键——key 后缀即
    // recipientDomainId，fetch 侧扫后缀取最近值）
    for env in &envelopes {
        if let Ok(record) =
            serde_json::from_str::<crate::org::org_address::OrgAddressRecord>(&env.to.org_address)
        {
            let _ = storage.put(
                &rl_fetch_key(&record.org_id, recipient_domain_id),
                &now_ms.to_string(),
            );
        }
    }
    Ok(json!({ "ok": true, "envelopes": envelopes }))
}

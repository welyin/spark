//! 组织加入共同体的邀请/接受载荷（org-genesis §3/§4 + community-affairs §4.5）：
//! 邀请码线形/编解码（纯逻辑）与 org-mail 载荷类型常量。
//!
//! 传输模型（对齐既有个人邀请模式 `invite.rs` + `kernel/org_invite_ops.rs`）：
//! 邀请码本身**不签名、不含密钥**——它不是 capability，成员资格在落库前的
//! 加入验证（[`crate::org::service::OrganizationService::validate_org_join`]
//! 域类型硬规则 + DAG 成环检查）把关；编码同样为
//! `base64url(JSON.stringify(payload) 的 UTF-8)`（无 padding）。
//!
//! 与个人邀请的差异：邀请码不经 DM 而经**跨组织网关邮箱**（p2p-org-mail §21）
//! 投递——org-mail 明文载荷类型 `community-org-invite`（内嵌邀请码 + 展示字段）；
//! 接受方落库后尽力回发 `community-org-join-notice`（组织域身份 + opt-in 公开
//! 绑定），回信寻址取自邀请码内的 `communityOrgAddress`/`replyDomainId`。

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::invite::{ORG_INVITE_MAX_AGE_MS, OrgInviteInviter};
use super::types::{OrgBinding, is_valid_org_id, is_valid_root_id};

/// 共同体邀请码 payload 类型标签。
pub const COMMUNITY_ORG_INVITE_TYPE: &str = "community-org-invite";

/// org-mail 明文载荷类型：加入通知（接受侧 → 共同体回信，尽力而为）。
pub const COMMUNITY_ORG_JOIN_NOTICE_TYPE: &str = "community-org-join-notice";

/// 共同体成员域身份域串（org-genesis §4）：`community:{communityOrgId}`——
/// 组织根密钥对派生该共同体内的域身份，成员条目 rootId 槽位承载其身份 id
/// （跨域不可关联，orgId 不直接出现在成员关系中）。
pub fn community_domain(community_org_id: &str) -> String {
    format!("community:{community_org_id}")
}

/// 共同体邀请码 payload（org-mail `community-org-invite` 载荷内嵌的 code）。
///
/// 字段顺序即编码 JSON 键序（与 `OrgInvitePayload` 同约定）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommunityOrgInvitePayload {
    /// 固定 `community-org-invite`。
    #[serde(rename = "type")]
    pub type_: String,
    /// 固定 1。
    pub version: u32,
    /// 共同体域 orgId（双形态）。
    #[serde(rename = "communityOrgId")]
    pub community_org_id: String,
    /// 共同体名（展示用自报字段；缺省归一为 `""`）。
    #[serde(rename = "communityOrgName", default)]
    pub community_org_name: String,
    /// 共同体组织地址记录完整线形（可省；回信寻址/归属判定用——私有共同体
    /// 未发布地址记录时缺省丢键，接受侧跳过加入通知回发）。
    #[serde(
        rename = "communityOrgAddress",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub community_org_address: Option<String>,
    /// 邀请人收取回信的域身份（邀请人 `org-mail:{communityOrgId}` 域身份公钥
    /// b64；可省，缺省时接受侧无法回发加入通知）。
    #[serde(
        rename = "replyDomainId",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub reply_domain_id: Option<String>,
    /// 邀请人引导信息（共同体管理员个人 rootId + 节点线索）。
    pub inviter: OrgInviteInviter,
    /// 创建时间（ms）。
    #[serde(rename = "createdAt")]
    pub created_at: i64,
}

impl CommunityOrgInvitePayload {
    /// 构造新 payload（type/version 填常量）。
    pub fn new(
        community_org_id: impl Into<String>,
        community_org_name: impl Into<String>,
        inviter: OrgInviteInviter,
        created_at: i64,
    ) -> Self {
        Self {
            type_: COMMUNITY_ORG_INVITE_TYPE.to_string(),
            version: 1,
            community_org_id: community_org_id.into(),
            community_org_name: community_org_name.into(),
            community_org_address: None,
            reply_domain_id: None,
            inviter,
            created_at,
        }
    }
}

/// 邀请码解析错误（文案与 `OrgInviteError` 同风格，面向用户可读）。
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CommunityInviteError {
    /// 空输入。
    #[error("邀请码为空")]
    Empty,
    /// base64url/JSON 解码失败。
    #[error("邀请码格式不正确")]
    Malformed,
    /// type/version 不符。
    #[error("不是有效的星火共同体邀请码")]
    NotCommunityInvite,
    /// communityOrgId 缺失。
    #[error("邀请码缺少共同体标识")]
    MissingOrgId,
    /// communityOrgId 非双形态 orgId。
    #[error("邀请码中的共同体标识非法")]
    InvalidOrgId,
    /// inviter.rootId 非法。
    #[error("邀请码缺少有效的邀请人身份")]
    InvalidInviter,
    /// createdAt 非正数或超过 24h。
    #[error("邀请码已过期，请让管理员重新生成")]
    Expired,
}

/// 编码邀请码：紧凑 JSON → base64url（无 padding）。
pub fn encode_community_org_invite(payload: &CommunityOrgInvitePayload) -> String {
    let json = serde_json::to_string(payload).expect("invite payload is always serializable");
    URL_SAFE_NO_PAD.encode(json.as_bytes())
}

/// 解码并校验邀请码，`now_ms` 由调用方注入（对齐 `decode_org_invite_at`）。
///
/// 按序校验，任一不符返回对应错误：
/// 1. base64url 可解码且为合法 JSON（[Malformed](CommunityInviteError::Malformed)）
/// 2. `type == "community-org-invite" && version == 1`
/// 3. `communityOrgId` 非空且为双形态 orgId（org-genesis §2）
/// 4. `inviter.rootId` trim+lowercase 后匹配 `^[0-9a-f]{64}$`
/// 5. `createdAt` 为 number 且 `> 0` 且 `now - createdAt <= 24h`（同个人邀请
///    口径：只查"过去 24h"，未来的 createdAt 不设上限）
///
/// inviter 的 peerId/addresses 仅作寻址线索保留（过滤空串），**不做至少其一
/// 要求**——共同体邀请的传输是 org-mail 而非直连，回信走地址记录解析。
pub fn decode_community_org_invite_at(
    text: &str,
    now_ms: i64,
) -> std::result::Result<CommunityOrgInvitePayload, CommunityInviteError> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(CommunityInviteError::Empty);
    }

    let bytes = URL_SAFE_NO_PAD
        .decode(trimmed.as_bytes())
        .map_err(|_| CommunityInviteError::Malformed)?;
    let parsed: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| CommunityInviteError::Malformed)?;

    if parsed.get("type").and_then(|v| v.as_str()) != Some(COMMUNITY_ORG_INVITE_TYPE)
        || parsed.get("version").and_then(|v| v.as_u64()) != Some(1)
    {
        return Err(CommunityInviteError::NotCommunityInvite);
    }

    let community_org_id = parsed
        .get("communityOrgId")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    if community_org_id.is_empty() {
        return Err(CommunityInviteError::MissingOrgId);
    }
    if !is_valid_org_id(community_org_id) {
        return Err(CommunityInviteError::InvalidOrgId);
    }

    let inviter = parsed.get("inviter");
    let inviter_root_id = inviter
        .and_then(|i| i.get("rootId"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if inviter.is_none() || !is_valid_root_id(inviter_root_id) {
        return Err(CommunityInviteError::InvalidInviter);
    }

    let addresses: Vec<String> = inviter
        .and_then(|i| i.get("addresses"))
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str())
                .filter(|s| !s.trim().is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let peer_id = inviter
        .and_then(|i| i.get("peerId"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    // 新鲜度校验放在结构校验之后：格式错误优先报格式问题。
    let created_at = match parsed.get("createdAt") {
        Some(v) if v.is_i64() || v.is_u64() => v.as_i64().unwrap_or(0),
        Some(v) if v.is_f64() => v.as_f64().unwrap_or(0.0) as i64,
        _ => 0,
    };
    if created_at <= 0 || now_ms - created_at > ORG_INVITE_MAX_AGE_MS {
        return Err(CommunityInviteError::Expired);
    }

    Ok(CommunityOrgInvitePayload {
        type_: COMMUNITY_ORG_INVITE_TYPE.to_string(),
        version: 1,
        community_org_id: community_org_id.to_string(),
        community_org_name: parsed
            .get("communityOrgName")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        community_org_address: parsed
            .get("communityOrgAddress")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        reply_domain_id: parsed
            .get("replyDomainId")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        inviter: OrgInviteInviter {
            root_id: inviter_root_id.trim().to_lowercase(),
            peer_id,
            addresses,
        },
        created_at,
    })
}

/// org-mail 明文载荷：共同体邀请（`community-org-invite`）。邀请码内嵌 `code`
/// 键（对齐个人 DM 邀请信封 body 内嵌 inviteCode 的模式），其余为展示字段。
pub fn build_community_invite_mail_body(payload: &CommunityOrgInvitePayload, code: &str) -> Value {
    json!({
        "type": COMMUNITY_ORG_INVITE_TYPE,
        "version": 1,
        "code": code,
        "communityOrgId": payload.community_org_id,
        "communityOrgName": payload.community_org_name,
        "inviterRootId": payload.inviter.root_id,
        "createdAt": payload.created_at,
    })
}

/// org-mail 明文载荷：加入通知（`community-org-join-notice`，接受侧落库后尽力
/// 回发邀请人的回信域身份）。携带接受方组织在本共同体的域身份 id 与 opt-in
/// 公开绑定（org-genesis §3.2 orgBinding；未公开绑定时丢键）。
pub fn build_community_join_notice(
    community_org_id: &str,
    member_identity: &str,
    org_binding: Option<&OrgBinding>,
    accepted_at: i64,
) -> Value {
    let mut body = json!({
        "type": COMMUNITY_ORG_JOIN_NOTICE_TYPE,
        "version": 1,
        "communityOrgId": community_org_id,
        "memberIdentity": member_identity,
        "acceptedAt": accepted_at,
    });
    if let Some(binding) = org_binding
        && let Ok(value) = serde_json::to_value(binding)
    {
        body["orgBinding"] = value;
    }
    body
}

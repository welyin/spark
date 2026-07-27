//! 组织邀请码（对齐 desktop/src/main/organization/invite.ts）。
//!
//! 邀请码仅携带组织标识与邀请人节点地址，经线下渠道传播；**不签名、不含
//! 密钥**——它不是 capability，成员资格校验始终在拉取侧完成（invite.ts:1-7）。
//!
//! 编码：`base64url(JSON.stringify(payload) 的 UTF-8)`，`+`→`-`、`/`→`_`、
//! 去掉 `=` padding；解码时反向并补 `=`。

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};

use super::types::is_valid_root_id;

/// 邀请码接受侧有效期（24 小时，invite.ts:27）。
pub const ORG_INVITE_MAX_AGE_MS: i64 = 24 * 60 * 60 * 1000;

/// 邀请码 payload 类型标签。
pub const ORG_INVITE_TYPE: &str = "spark-org-invite";

/// 邀请人引导信息。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgInviteInviter {
    /// 邀请人 rootId（64 hex 小写）。
    #[serde(rename = "rootId")]
    pub root_id: String,
    /// 邀请人 peerId（可省；与 addresses 至少其一）。
    #[serde(rename = "peerId", default, skip_serializing_if = "Option::is_none")]
    pub peer_id: Option<String>,
    /// 邀请人 multiaddr 列表。
    #[serde(default)]
    pub addresses: Vec<String>,
}

/// 邀请码 payload（invite.ts:9-20）。
///
/// 字段顺序即 TS `JSON.stringify` 的插入序，编码字节级对齐。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgInvitePayload {
    /// 固定 `spark-org-invite`。
    #[serde(rename = "type")]
    pub type_: String,
    /// 固定 1。
    pub version: u32,
    /// 组织 id。
    #[serde(rename = "orgId")]
    pub org_id: String,
    /// 组织名（可空串；缺省归一为 `""`）。
    #[serde(rename = "orgName", default)]
    pub org_name: String,
    /// 邀请人引导信息。
    pub inviter: OrgInviteInviter,
    /// 创建时间（ms）。
    #[serde(rename = "createdAt")]
    pub created_at: i64,
}

impl OrgInvitePayload {
    /// 构造新 payload（type/version 填常量）。
    pub fn new(
        org_id: impl Into<String>,
        org_name: impl Into<String>,
        inviter: OrgInviteInviter,
        created_at: i64,
    ) -> Self {
        Self {
            type_: ORG_INVITE_TYPE.to_string(),
            version: 1,
            org_id: org_id.into(),
            org_name: org_name.into(),
            inviter,
            created_at,
        }
    }
}

/// 邀请码解析错误（消息与 TS 抛出的中文错误逐字一致）。
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum OrgInviteError {
    /// 空输入。
    #[error("邀请码为空")]
    Empty,
    /// base64url/JSON 解码失败。
    #[error("邀请码格式不正确")]
    Malformed,
    /// type/version 不符。
    #[error("不是有效的星火组织邀请码")]
    NotSparkOrgInvite,
    /// orgId 缺失。
    #[error("邀请码缺少组织标识")]
    MissingOrgId,
    /// inviter.rootId 非法。
    #[error("邀请码缺少有效的邀请人身份")]
    InvalidInviter,
    /// peerId 与 addresses 皆缺。
    #[error("邀请码缺少邀请人的节点地址，无法建立连接")]
    MissingInviterAddress,
    /// createdAt 非正数或超过 24h。
    #[error("邀请码已过期，请让管理员重新生成")]
    Expired,
}

/// `encodeOrgInvite`：紧凑 JSON → base64url（无 padding）。
pub fn encode_org_invite(payload: &OrgInvitePayload) -> String {
    let json = serde_json::to_string(payload).expect("invite payload is always serializable");
    URL_SAFE_NO_PAD.encode(json.as_bytes())
}

/// `decodeOrgInvite`（invite.ts:44-89），`now_ms` 由调用方注入（对齐 `Date.now()`）。
///
/// 按序校验，任一不符返回对应中文错误：
/// 1. base64url 可解码且为合法 JSON（[Malformed](OrgInviteError::Malformed)）
/// 2. `type == "spark-org-invite" && version == 1`
/// 3. `orgId` 非空（trim 后使用）
/// 4. `inviter.rootId` trim+lowercase 后匹配 `^[0-9a-f]{64}$`
/// 5. addresses 过滤非字符串/空串；peerId trim 后非空才保留；两者至少其一
/// 6. `createdAt` 为 number 且 `> 0` 且 `now - createdAt <= 24h`
///
/// ⚠️ 如实复刻 TS 口径：只查"过去 24h"，**未来的 createdAt 不设上限**
/// （invite.ts:76-79 无 `Math.abs`，spec org.md §2.3/§14.7）。
pub fn decode_org_invite_at(text: &str, now_ms: i64) -> Result<OrgInvitePayload, OrgInviteError> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(OrgInviteError::Empty);
    }

    let bytes = URL_SAFE_NO_PAD
        .decode(trimmed.as_bytes())
        .map_err(|_| OrgInviteError::Malformed)?;
    let parsed: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| OrgInviteError::Malformed)?;

    if parsed.get("type").and_then(|v| v.as_str()) != Some(ORG_INVITE_TYPE)
        || parsed.get("version").and_then(|v| v.as_u64()) != Some(1)
    {
        return Err(OrgInviteError::NotSparkOrgInvite);
    }

    let org_id = parsed.get("orgId").and_then(|v| v.as_str()).unwrap_or("");
    if org_id.trim().is_empty() {
        return Err(OrgInviteError::MissingOrgId);
    }

    let inviter = parsed.get("inviter");
    let inviter_root_id = inviter
        .and_then(|i| i.get("rootId"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if inviter.is_none() || !is_valid_root_id(inviter_root_id) {
        return Err(OrgInviteError::InvalidInviter);
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
    if peer_id.is_none() && addresses.is_empty() {
        return Err(OrgInviteError::MissingInviterAddress);
    }

    // 新鲜度校验放在结构校验之后：格式错误优先报格式问题。
    // createdAt 必须是 JSON number（整数或浮点均按数值截断为 ms；
    // 亚毫秒差异无实际语义）；其他类型按 TS `typeof !== 'number' → 0` 归一（必过期）。
    let created_at = match parsed.get("createdAt") {
        Some(v) if v.is_i64() || v.is_u64() => v.as_i64().unwrap_or(0),
        Some(v) if v.is_f64() => v.as_f64().unwrap_or(0.0) as i64,
        _ => 0,
    };
    if created_at <= 0 || now_ms - created_at > ORG_INVITE_MAX_AGE_MS {
        return Err(OrgInviteError::Expired);
    }

    Ok(OrgInvitePayload {
        type_: ORG_INVITE_TYPE.to_string(),
        version: 1,
        org_id: org_id.trim().to_string(),
        org_name: parsed
            .get("orgName")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        inviter: OrgInviteInviter {
            root_id: inviter_root_id.trim().to_lowercase(),
            peer_id,
            addresses,
        },
        created_at,
    })
}

/// `decodeOrgInvite` 的当前时间版本。
pub fn decode_org_invite(text: &str) -> Result<OrgInvitePayload, OrgInviteError> {
    decode_org_invite_at(text, now_ms())
}

/// 当前 Unix 毫秒时间（供 [`decode_org_invite`] 使用；测试走 `_at` 注入版本）。
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

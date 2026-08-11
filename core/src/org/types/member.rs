//! 成员角色、节点信息与成员记录（含 `sortMembers` 排序）。

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::super::{OrgError, Result};

/// 组织成员角色。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OrganizationRole {
    /// 管理员（创建者自动为唯一初始 admin）。
    Admin,
    /// 普通成员（addMember 新成员固定为该角色）。
    #[default]
    Member,
}

impl OrganizationRole {
    /// TS 字符串形式。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Admin => "admin",
            Self::Member => "member",
        }
    }
}

/// 成员设备端点（个人域 deviceUid 绑定 + peerId + addresses）。
///
/// 既用于 nodeInfoClaim 的声明（单端点，随签名链路流动），也作为成员表端点集
/// 中的一项（O1 工作项 1 端点化）。`device_uid` 为**物理设备稳定标识**
/// （随机 128bit hex，重装/peerId 漂移不变）——成员表按它聚合端点、判定
/// 同设备陈旧 peerId 墓碑化替换。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrganizationNodeInfo {
    /// 物理设备稳定标识（个人域 `p2p:device:uid`；旧版本声明/记录缺省为 None，
    /// 无法归属到设备时按 peerId 聚合兜底）。
    #[serde(
        rename = "deviceUid",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub device_uid: Option<String>,
    /// libp2p peerId（可省）。
    #[serde(rename = "peerId", default, skip_serializing_if = "Option::is_none")]
    pub peer_id: Option<String>,
    /// multiaddr 列表。
    #[serde(default)]
    pub addresses: Vec<String>,
}

/// `normalizeNodeInfo`（service.ts:46-64）：peerId/addresses 各自 trim 滤空；
/// 两者皆空报错；peerId 非空但 < 8 字符报错。deviceUid 原样保留（trim 滤空，
/// 空则 None）。
pub fn normalize_node_info(node_info: &OrganizationNodeInfo) -> Result<OrganizationNodeInfo> {
    let device_uid = node_info
        .device_uid
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(str::to_string);
    let peer_id = node_info
        .peer_id
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(str::to_string);
    let addresses: Vec<String> = node_info
        .addresses
        .iter()
        .map(|a| a.trim())
        .filter(|a| !a.is_empty())
        .map(str::to_string)
        .collect();

    if peer_id.is_none() && addresses.is_empty() {
        return Err(OrgError::NodeInfoRequired);
    }
    if let Some(p) = &peer_id
        && p.len() < 8
    {
        return Err(OrgError::InvalidPeerId);
    }
    Ok(OrganizationNodeInfo {
        device_uid,
        peer_id,
        addresses,
    })
}

/// `normalizeOptionalNodeInfo`（service.ts:67-77）：未提供或全空视为 `None`
/// （成员地址可后续经 nodeInfoClaim 回填）。
pub fn normalize_optional_node_info(
    node_info: Option<&OrganizationNodeInfo>,
) -> Result<Option<OrganizationNodeInfo>> {
    let Some(info) = node_info else {
        return Ok(None);
    };
    let has_peer_id = info
        .peer_id
        .as_deref()
        .is_some_and(|p| !p.trim().is_empty());
    let has_addresses = info.addresses.iter().any(|a| !a.trim().is_empty());
    if !has_peer_id && !has_addresses {
        return Ok(None);
    }
    normalize_node_info(info).map(Some)
}

/// 成员端点集：按 deviceUid 聚合的成员设备端点列表（O1 工作项 1 端点化）。
///
/// 端点化把"成员 = 单 peerId 端点"改为"成员账号的若干设备端点"，每端点绑定
/// 个人域 deviceUid（复用个人域 deviceUid 绑定与同设备陈旧 peerId 墓碑化机制）。
///
/// **wire 兼容（serde）**：旧线形 `nodeInfo` 为单端点对象 `{peerId, addresses}`，
/// 新线形为端点对象数组 `[{deviceUid?, peerId?, addresses}, ...]`。为不炸旧库、
/// 且保持 golden 向量（单端点无 deviceUid）逐字段一致：
/// - **反序列化**同时接受单对象与数组两种线形；
/// - **序列化**仅当端点集恰含一个**无 deviceUid** 的端点时退化为单对象线形，
///   其余（多端点 / 含 deviceUid）序列化为数组。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OrganizationDeviceSet {
    /// 端点列表（内部保序，聚合键见 [`Self::upsert`]）。
    pub endpoints: Vec<OrganizationNodeInfo>,
}

impl serde::Serialize for OrganizationDeviceSet {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        // Z2：单端点**不论有无 deviceUid** 恒序列化为单对象线形
        // `{deviceUid?, peerId, addresses}`——旧端无 deny_unknown_fields 可
        // 忽略 deviceUid 键，线形兼容；与 golden 向量（单端点无 uid）逐字段
        // 一致。数组仅用于多端点（len>1）。
        if self.endpoints.len() == 1 {
            return self.endpoints[0].serialize(serializer);
        }
        self.endpoints.serialize(serializer)
    }
}

impl<'de> serde::Deserialize<'de> for OrganizationDeviceSet {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        // 旧线形：单端点对象 `{peerId, addresses}` → 单元素集合。
        let value = Value::deserialize(deserializer).map_err(serde::de::Error::custom)?;
        match value {
            Value::Array(items) => {
                let endpoints = items
                    .into_iter()
                    .map(serde_json::from_value::<OrganizationNodeInfo>)
                    .collect::<std::result::Result<Vec<_>, _>>()
                    .map_err(serde::de::Error::custom)?;
                Ok(Self { endpoints })
            }
            obj @ Value::Object(_) => {
                let single =
                    serde_json::from_value::<OrganizationNodeInfo>(obj).map_err(serde::de::Error::custom)?;
                Ok(Self {
                    endpoints: vec![single],
                })
            }
            _ => Err(serde::de::Error::custom("nodeInfo must be an object or array")),
        }
    }
}

impl OrganizationDeviceSet {
    /// 由单端点构造集合（`add_member` / 旧数据迁移用）。
    pub fn from_single(info: OrganizationNodeInfo) -> Self {
        Self {
            endpoints: vec![info],
        }
    }

    /// 端点数。
    pub fn len(&self) -> usize {
        self.endpoints.len()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.endpoints.is_empty()
    }

    /// 按 deviceUid（兜底 peerId）合并一个到达的端点声明（nodeInfoClaim /
    /// addMember 更新）。
    ///
    /// **聚合键 = deviceUid**：同 deviceUid 的旧 peerId 端点被墓碑化替换（成员
    /// 换设备/重装后 peerId 漂移，凭稳定 deviceUid 识别同一台物理设备）；
    /// deviceUid 缺失（旧声明）时按 peerId 兜底——同 peerId 更新地址，否则新增。
    ///
    /// 返回是否发生变更（未变 → 调用方不 bump 版本）。
    pub fn upsert(&mut self, incoming: &OrganizationNodeInfo) -> bool {
        // 归一化后的到达端点（调用方保证已 normalize）。
        if let Some(uid) = incoming.device_uid.as_deref() {
            // 同 deviceUid：旧 peerId 墓碑化（从集合移除），保留新 peerId。
            let before = self.endpoints.len();
            self.endpoints.retain(|e| {
                !(e.device_uid.as_deref() == Some(uid) && e.peer_id != incoming.peer_id)
            });
            // F2：升级路径——吸收同 peerId 的无 uid 旧版端点。既有端点是
            // 本设备旧版记录（deviceUid 缺失），incoming 携带 uid 且同
            // peerId：把旧无 uid 端点移除（由其有 uid 版本取代），避免
            // 同 peerId 双端点并存。
            self.endpoints.retain(|e| {
                !(e.device_uid.is_none() && e.peer_id.is_some() && e.peer_id == incoming.peer_id)
            });
            let removed_stale = self.endpoints.len() != before;
            if let Some(existing) = self
                .endpoints
                .iter_mut()
                .find(|e| e.device_uid.as_deref() == Some(uid))
            {
                let unchanged = existing.peer_id == incoming.peer_id && existing.addresses == incoming.addresses;
                *existing = incoming.clone();
                return removed_stale || !unchanged;
            }
            self.endpoints.push(incoming.clone());
            true
        } else if let Some(peer_id) = incoming.peer_id.as_deref() {
            if let Some(existing) = self
                .endpoints
                .iter_mut()
                .find(|e| e.peer_id.as_deref() == Some(peer_id))
            {
                let unchanged = existing.addresses == incoming.addresses;
                existing.addresses = incoming.addresses.clone();
                return !unchanged;
            }
            self.endpoints.push(incoming.clone());
            true
        } else {
            // 无 deviceUid 无 peerId 仅地址：按地址串合并（新端点）。
            let key = incoming.addresses.join("|");
            if !key.is_empty()
                && self
                    .endpoints
                    .iter()
                    .any(|e| e.peer_id.is_none() && e.addresses.join("|") == key)
            {
                return false;
            }
            self.endpoints.push(incoming.clone());
            true
        }
    }

    /// 遍历端点，供消费方（org-pull 候选 / orgsync-hello 目标 / dm 寻址 /
    /// 记账）构造 [`crate::p2p::peer_targets::PeerNodeInfo`]。
    pub fn iter(&self) -> impl Iterator<Item = &OrganizationNodeInfo> {
        self.endpoints.iter()
    }
}

/// 成员组织身份访问密钥记录（O4）：`org-access:{orgId}` 域身份的 Ed25519 公钥
/// base64 + 根密钥对该公钥的绑定签名。
///
/// 线形含 `publicKey`（域身份公钥 b64）与 `bindSig`（根密钥对绑定载荷的签名
/// b64）。绑定载荷 = `"org-access:{orgId}:{publicKey}"`——由成员根私钥签名，
/// 证明本成员拥有该组织访问身份（验签方据此拿公钥验 acl/orgkey-deliver 签名）。
/// 组织身份字段仅本人可改（与 nickname/avatar 同路径，经快照 members 段传播）。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrganizationAccessKey {
    /// 组织访问域身份公钥（`derive_domain_identity(seed, "org-access:{orgId}")`
    /// 的 Ed25519 公钥，base64）。
    #[serde(rename = "publicKey")]
    pub public_key: String,
    /// 根密钥对绑定载荷 `org-access:{orgId}:{publicKey}` 的签名（base64）。
    #[serde(rename = "bindSig")]
    pub bind_sig: String,
}

/// `OrganizationAccessKey` 的绑定签名载荷：`org-access:{orgId}:{publicKeyB64}`。
pub fn access_key_bind_payload(org_id: &str, public_key_b64: &str) -> String {
    format!("org-access:{org_id}:{public_key_b64}")
}

/// 组织成员。
///
/// `extra` 捕获 wire 上成员对象的非标准键（合并时随 existing 保留，对齐 TS 的
/// 对象展开语义 `{...existingMember, ...member}`）。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct OrganizationMember {
    /// 成员 rootId（64 hex 小写）。
    #[serde(rename = "rootId")]
    pub root_id: String,
    /// 角色。
    pub role: OrganizationRole,
    /// 加入时间（ms）。
    #[serde(rename = "joinedAt")]
    pub joined_at: i64,
    /// 录入人 rootId。
    #[serde(rename = "addedBy")]
    pub added_by: String,
    /// 节点信息（按 deviceUid 聚合的端点集；可后续经 nodeInfoClaim 回填）。
    #[serde(rename = "nodeInfo", default, skip_serializing_if = "Option::is_none")]
    pub node_info: Option<OrganizationDeviceSet>,
    /// 组织内昵称（组织身份；全部身份字段仅本人可改，经快照 members 段传播）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nickname: Option<String>,
    /// 组织内头像（data URL）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar: Option<String>,
    /// 个性签名。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    /// 性别。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gender: Option<String>,
    /// 地区。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    /// 是否在组织内展示个人身份（`None` = 未设置，语义视同 false；
    /// `Some(false)` = 显式关闭——M1 墓碑语义：true→false 必须能经快照传播，
    /// 故升级为 `Option<bool>`；旧数据 true/false 读为 `Some`，缺键读为
    /// `None`（serde default 兼容）。
    #[serde(
        rename = "usePersonalIdentity",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub use_personal_identity: Option<bool>,
    /// 组织身份访问密钥（O4）：`org-access:{orgId}` 域身份公钥 + 根密钥绑定签名。
    /// 仅本人可改（与 nickname/avatar 同路径）；`None` = 尚未发布（未启用
    /// encrypted 能力/未加入组织惰性派生），存量记录兼容。
    #[serde(
        rename = "accessKey",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub access_key: Option<OrganizationAccessKey>,
    /// 非标准动态键。
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

/// `sortMembers`（service.ts:79-86）：admin 优先，其余按 joinedAt 升序。
///
/// 注意：TS `Array.prototype.sort` 稳定；Rust `sort_by` 同样稳定，逐键对齐。
pub fn sort_members(members: &[OrganizationMember]) -> Vec<OrganizationMember> {
    let mut sorted = members.to_vec();
    sorted.sort_by(|left, right| {
        if left.role != right.role {
            return if left.role == OrganizationRole::Admin {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Greater
            };
        }
        left.joined_at.cmp(&right.joined_at)
    });
    sorted
}

//! 组织级私有 DHT（网关代理）的纯逻辑（org.md §13-14、p2p-messages.md §15）。
//!
//! - key 派生：`H(orgSecret + ":members")` = sha256hex(拼接字符串的 UTF-8)。
//!   key 由 orgSecret 单向派生——非持密者无法计算 key、无法枚举该组织的
//!   provider 集合（p2p-messages.md §15 不可枚举性）。
//! - 成员提示线形：网关节点在私有 DHT 上提供的记录内容，**只含**
//!   `{peerId, addresses}`，不含 orgId、组织名等任何组织语义。
//!
//! 网络动作（start_providing / 查询 / 入池）在 p2p 与 kernel 层，本模块只放
//! 可单测的纯函数。

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// 私有 DHT key 的派生后缀（p2p-messages.md §15）。
pub const ORG_MEMBERS_DHT_KEY_SUFFIX: &str = ":members";

/// 组织私有 DHT key：`sha256hex(orgSecret + ":members")`（64 字符小写 hex）。
pub fn org_members_dht_key(org_secret: &str) -> String {
    let input = format!("{org_secret}{ORG_MEMBERS_DHT_KEY_SUFFIX}");
    hex::encode(Sha256::digest(input.as_bytes()))
}

/// 网关节点在私有 DHT 上提供的成员提示（p2p-messages.md §15：响应成员地址
/// 查询时只返回 `{peerId, addresses}`，不含组织语义）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgMemberHint {
    /// 网关节点 libp2p peerId。
    #[serde(rename = "peerId")]
    pub peer_id: String,
    /// 网关节点 multiaddr 列表。
    #[serde(default)]
    pub addresses: Vec<String>,
}

impl OrgMemberHint {
    /// 序列化为 DHT 记录值（紧凑 JSON，无空格）。
    pub fn to_record_value(&self) -> Vec<u8> {
        serde_json::to_string(self)
            .unwrap_or_else(|_| "{}".to_string())
            .into_bytes()
    }

    /// 从 DHT 记录值解析：形状不符返回 `None`（静默丢弃口径）。
    ///
    /// 只接受 `{peerId, addresses}` 形状——node-announce 等其他 DHT 记录
    /// 内容不会被误认为成员提示。
    pub fn from_record_value(value: &[u8]) -> Option<Self> {
        let text = std::str::from_utf8(value).ok()?;
        let parsed: serde_json::Value = serde_json::from_str(text).ok()?;
        let obj = parsed.as_object()?;
        // 形状闸：恰好只含 peerId/addresses 两键（§15 的响应内容约定）
        if obj.len() != 2 || !obj.contains_key("peerId") || !obj.contains_key("addresses") {
            return None;
        }
        let hint: Self = serde_json::from_value(parsed).ok()?;
        if hint.peer_id.trim().is_empty() {
            return None;
        }
        Some(hint)
    }

    /// 转拨号候选：只含 peerId/addresses 的最小形状（调用方自行组装 p2p 类型）。
    pub fn into_parts(self) -> (String, Vec<String>) {
        (self.peer_id, self.addresses)
    }
}

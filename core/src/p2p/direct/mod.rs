//! 直连协议帧与响应侧纯逻辑（core/spec/p2p-messages.md §4/§6/§7/§8/§9）。
//!
//! 通用约定：四个协议均为"写一帧 JSON → 读一帧 JSON"的 request-response，
//! 应用层无长度前缀、无分隔符，帧边界由流承载保证；解析失败返回 null 不抛异常。
//!
//! 代码组织：按协议拆分为 `version`（版本帧）、`exchange`（邻居交换）、
//! `recovery`（组织恢复查询/转发合并）、`org_share`（组织分享/拉取）四个
//! 子模块，外加通用 `rate_limit` 限流器；本文件仅做全量再导出，调用方
//! 路径 `p2p::direct::*` 不变；单测在 `core/tests/unit_p2p/direct.rs`。

mod exchange;
mod org_share;
mod rate_limit;
mod recovery;
mod version;

pub use exchange::{
    PeerExchangeSample, build_exchange_request, build_exchange_response, filter_incoming_sample,
    normalize_exchange_want, parse_exchange_response,
};
pub use org_share::{
    OrgShareRequestKind, build_org_share_ack_response, build_org_share_error_response,
    build_org_share_request, build_pull_list_request, build_pull_org_request,
    parse_org_share_direct_response, parse_org_share_request,
};
pub use rate_limit::MinIntervalRateLimiter;
pub use recovery::{
    RecoveryQuery, build_recovery_request, build_recovery_response, dedupe_recovery_peers,
    match_recovery_view, normalize_recovery_ttl, normalize_recovery_want, parse_recovery_request,
    parse_recovery_response,
};
pub use version::{PeerVersionResponse, build_peer_version_response, parse_peer_version_response};

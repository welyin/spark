//! orgsync 组织域同步协议（O2a）：三信封反熵，语义对齐 pdsync §5，
//! 仅作用域从 category 换成 (orgId, collection)。
//!
//! 三族信封：
//! - `orgsync-hello`：摘要交换（collection → 折叠 vv + dlogAck + roles +
//!   deviceClass）；
//! - `orgsync-need`：diff 请求（orgId + collection + 本地 knownVv + dlogAck）；
//! - `orgsync-data`：数据传输（逐条 key/value/meta + 分批 + 墓碑 + dseq）。
//!
//! 验签规则：公共前置 from ∈ org:meta 成员表；同步类另要求 from ∈ 该集合
//! 复制组（all-members = 全体成员；data-accounts = 当前数据账号集含缺省推导）。
//!
//! 本模块是纯逻辑（存储泛型），不触碰 p2p/签名——信封装配与投递由 kernel
//! 层完成。
//!
//! 目录形态（Z1 拆分，按职责）：
//! - [`builtin`]：内建 all-members 集合注册 + 存量键作用域 + 公共常量；
//! - [`dlog`]：复制组判定 + org 域 dlog 辅助与 GC + org 域本地墓碑原语；
//! - [`envelope`]：三信封 build/parse + 记录类型 + 分批切分；
//! - [`collect`]：vv 折叠、增量采集、墓碑采集、diff 裁决、角色履职；
//! - [`orgq`]（O3）：orgq-req/orgq-resp 信封 build/parse + requestId 关联 +
//!   成员侧缓存命名空间 + 在线数据账号目录 + 成员侧路由决策；
//! - [`orgq_deliver`]（O3）：成员侧在线 orgq-req 投递的纯逻辑辅助——在途记录
//!   读写 / TTL 清理 / 上限 / 写入回执存储 / 同步等待清除（三通路共享骨架）。
//!
//! C7（community-affairs §5）：org 集合级 `encrypted` 轴已退役——原
//! `access`/`access_data` 的 orgkey 密钥表、org:acl、orgkey-deliver、
//! encrypted 值加密面全部移除；`access` 仅存 Ed25519↔X25519 转换原语。

pub(crate) mod access;
mod builtin;
mod collect;
mod dlog;
mod envelope;
mod orgq;
mod orgq_cache;
mod orgq_deliver;
mod orgq_online;
mod orgq_queue;

pub use access::{ed_pk_to_x25519, ed_sk_to_x25519};
pub use builtin::{
    BuiltinOrgCollection, ORGSYNC_BATCH_BYTES, ORGSYNC_DLOG_ACK_RETRY_FIRST_MS,
    ORGSYNC_DLOG_ACK_RETRY_SECOND_MS, ORGSYNC_HELLO_DEBOUNCE_MS, builtin_collection_by_name,
    collection_data_prefixes, legacy_org_key_scope,
};
pub use collect::{
    OrgDiffOutcome, collect_org_collection_vv, collect_org_collections, collect_org_incremental,
    collect_org_tombstones_after, diff_org_collection, self_roles,
};
pub use dlog::{
    is_in_replication_group, org_dlog_append_ops, org_dlog_current_seq, org_dlog_entries_after,
    org_dlog_gc, org_dlog_gc_threshold, org_dlog_get_seen, org_dlog_get_watermark,
    org_dlog_remove_member_marks, org_dlog_set_seen, org_dlog_set_watermark, org_tombstone_local,
    replication_group_members,
};
pub use envelope::{
    OrgsyncRecord, build_orgsync_data_batch, build_orgsync_hello, build_orgsync_need,
    parse_orgsync_data, parse_orgsync_hello, parse_orgsync_need, split_orgsync_batches,
};
pub use orgq::{
    ORGQ_LIMIT_DEFAULT, ORGQ_LIMIT_MAX, OrgqReq, OrgqResp, OrgqRespRecord, OrgqWriteRecord,
    build_orgq_query_req, build_orgq_query_resp, build_orgq_write_req, build_orgq_write_resp,
    collect_orgq_records, collect_orgq_records_page, parse_orgq_req, parse_orgq_resp,
    split_orgq_resp_batches,
};
pub use orgq_cache::{
    ORGQ_CACHE_MAX_KEYS_PER_COLLECTION, orgq_cache_evict, orgq_cache_has_data, orgq_cache_key,
    orgq_cache_prefix,
};
pub use orgq_deliver::{
    ORGQ_PENDING_MAX, ORGQ_PENDING_TTL_MS, PendingPutError, orgq_gen_request_id,
    orgq_pending_cleanup_stale, orgq_pending_get, orgq_pending_key, orgq_pending_put,
    orgq_pending_remove, orgq_resp_cleanup_stale, orgq_resp_key, orgq_resp_put, orgq_resp_take,
    orgq_wait_cleared,
};
pub use orgq_online::{
    MemberReadPlan, member_orgq_read_plan, orgq_da_degraded_key, orgq_da_duty_observations,
    orgq_da_online_key, orgq_degraded_for_collection, orgq_mark_data_account_degraded,
    orgq_mark_data_account_online, orgq_note_data_account_duty, orgq_online_data_accounts,
    select_online_data_account, should_route_orgq,
};
pub use orgq_queue::{
    orgq_queue_clear_by_collection, orgq_queue_drain, orgq_queue_drain_by_org, orgq_queue_has_data,
    orgq_queue_key, orgq_queue_prefix, orgq_queue_put, orgq_queue_read_by_org, orgq_wipe_org_local,
};

#[cfg(test)]
mod tests;

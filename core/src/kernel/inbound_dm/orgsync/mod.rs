//! dm 入站编排（orgsync 系）：orgsync-hello / orgsync-need / orgsync-data
//! 三信封组织域反熵同步。
//!
//! 从 `inbound_dm` 拆出的子模块（O2a），共享父模块的 [`InboundContext`]/
//! 应答助手/[`done`] 等。
//!
//! 目录形态（卫生批拆分，按职责）：`acl`（acl 验签+whole 合并）、
//! `data`（orgsync-data 合入主循环 + F1/F2 分流）、`tests`（模块内单测）；
//! hello/need 两 handler 留在本文件。
//!
//! 验签规则：
//! 1. 公共前置：from ∈ org:meta 成员表；
//! 2. 同步类另要求：from ∈ 该集合复制组（all-members=全体成员；
//!    data-accounts=当前数据账号集含缺省推导），不符静默丢弃。
//!
//! B3 红线：orgsync-data 入站逐条校验 key 属于该集合数据前缀（`orgd:`
//! 或保留系统集合 `org:coll:`），不符整批拒收（对齐 pdsync.rs 白名单先例）。

use serde_json::Value;

use super::{
    InboundContext, InboundDmResult, OrgsyncOut, Result, done, fail_response, ok_response,
};
use crate::org::OrganizationService;
use crate::storage::StorageBackend;

mod acl;
mod data;
#[cfg(test)]
mod tests;

pub(super) use data::handle_orgsync_data;

/// orgsync-hello：收到组织内复制组成员的摘要。逐集合与本地折叠 vv 比对：
/// - 本机落后 → 发 `orgsync-need`；
/// - 本机领先 → 主动推 `orgsync-data`；
/// - 相等 → 不动。
///
/// 验签：from ∈ 成员表（公共前置），且 from ∈ 各集合的复制组。
///
/// B2：hello 按收件人逐成员生成——dlogAck = 我已收讫**对端**该集合删除日志
/// 最大序号（`org_dlog_get_seen(recipient)`），随入站 ctx 的 peerId 取。
pub(super) fn handle_orgsync_hello<S: StorageBackend>(
    storage: &mut S,
    ctx: &InboundContext<'_>,
    from: &str,
    body: &Value,
) -> Result<InboundDmResult> {
    let Some((org_id, collections, roles, device_class)) =
        crate::sync::orgsync::parse_orgsync_hello(body)
    else {
        return done(fail_response("invalid-body"), Vec::new());
    };

    // 公共前置：from ∈ 成员表
    let Ok(Some(record)) = OrganizationService::get_record(storage, &org_id) else {
        log::info!("[ORGSYNC] hello rejected: org not found orgId={org_id}");
        return done(fail_response("rejected"), Vec::new());
    };
    if record.find_member(from).is_none() {
        log::info!("[ORGSYNC] hello rejected: from={from} not in org={org_id}");
        return done(fail_response("rejected"), Vec::new());
    }

    // O3 在线数据账号目录：对端 hello roles 含 "data" 即数据账号在线履职声明，
    // 更新目录（成员侧路由据此选在线数据账号发 orgq-req）。
    if roles.iter().any(|r| r == "data") {
        crate::sync::orgsync::orgq_mark_data_account_online(storage, &org_id, from, ctx.now_ms);
        // batch2 §1.2：履职观测持久化（K 记账证据）——按设备粒度记录
        // deviceClass + 观测时刻（在线目录是瞬态提示，K 记账要窗口期持久观测）
        crate::sync::orgsync::orgq_note_data_account_duty(
            storage, &org_id, from, ctx.remote_peer_id, &device_class, ctx.now_ms,
        );
    }

    let mut out = Vec::new();
    // O3 工作项 4：数据账号上线（hello roles 含 "data"）→ 冲刷本组织离线写入
    // 队列。离线期间成员写入被排入 `orgq:queue:{org}:`，此处按集合 drain 并
    // 逐集合重放为 orgq-req 写入信封（目标 = 上线的数据账号 from）。
    if roles.iter().any(|r| r == "data") {
        flush_orgq_offline_queue(storage, &mut out, &org_id, from, ctx);
    }
    for (col_full, (remote_vv, remote_dlog_ack, remote_degraded)) in &collections {
        // 解析集合名和代际：`name@v{version}`
        let Some(at) = col_full.rfind("@v") else {
            continue;
        };
        let name = &col_full[..at];
        let version = &col_full[at + 2..]; // skip "@v"
        // F4：对端 hello 标注该集合 degraded（数据账号对 filtered 集合插件未
        // 运行 → 只存不服务）→ 记录到目录供路由避让（orgq 查询此集合会 denied）。
        if *remote_degraded {
            crate::sync::orgsync::orgq_mark_data_account_degraded(
                storage, &org_id, from, col_full, ctx.now_ms,
            );
        }

        // 查找声明以确定 accounts 轴
        let decl_key = crate::plugindata::org_decl_key(&org_id, name, version);
        let accounts = storage
            .get(&decl_key)
            .ok()
            .flatten()
            .and_then(|raw| {
                serde_json::from_str::<crate::plugindata::CollectionDeclaration>(&raw).ok()
            })
            .map(|d| d.accounts)
            .unwrap_or_default();

        // 复制组验签：from ∈ 该集合复制组
        if !crate::sync::orgsync::is_in_replication_group(&record, from, accounts) {
            log::info!(
                "[ORGSYNC] hello rejected: from={from} not in replication group for {col_full}"
            );
            continue;
        }

        // 收集本地折叠 vv
        let Ok(local_vv) =
            crate::sync::orgsync::collect_org_collection_vv(storage, &org_id, name, version)
        else {
            continue;
        };

        let diff = crate::sync::orgsync::diff_org_collection(&local_vv, remote_vv);
        // 我对对端删除日志的已收序号（B6：按 (rootId=from, peerId) 设备粒度）
        let my_seen = crate::sync::orgsync::org_dlog_get_seen(
            storage,
            &org_id,
            name,
            version,
            from,
            ctx.remote_peer_id,
        )
        .unwrap_or(0);

        // 推进对端回执水位 + 尝试 GC（B6：水位按 (from, peerId) 设备粒度）
        if *remote_dlog_ack > 0 {
            let _ = crate::sync::orgsync::org_dlog_set_watermark(
                storage,
                &org_id,
                name,
                version,
                from,
                ctx.remote_peer_id,
                *remote_dlog_ack,
            );
            // 尝试 GC
            let members = crate::sync::orgsync::replication_group_members(&record, accounts);
            if let Ok(threshold) = crate::sync::orgsync::org_dlog_gc_threshold(
                storage,
                &org_id,
                name,
                version,
                &members,
                ctx.my_root_id,
            ) {
                if let Ok(removed) =
                    crate::sync::orgsync::org_dlog_gc(storage, &org_id, name, version, threshold)
                    && removed > 0
                {
                    log::info!("[ORGSYNC] dlog gc | org={org_id} col={col_full} removed={removed}");
                }
            }
        }

        match diff {
            crate::sync::orgsync::OrgDiffOutcome::LocalBehind { local_vv } => {
                let need_body =
                    crate::sync::orgsync::build_orgsync_need(&org_id, col_full, &local_vv, my_seen);
                out.push(OrgsyncOut::Need {
                    to_root_id: from.to_string(),
                    body: need_body,
                });
            }
            crate::sync::orgsync::OrgDiffOutcome::LocalAhead => {
                push_org_collection_data(
                    storage,
                    &mut out,
                    &org_id,
                    name,
                    version,
                    col_full,
                    remote_vv,
                    *remote_dlog_ack,
                    from,
                );
            }
            crate::sync::orgsync::OrgDiffOutcome::Concurrent => {
                let need_body =
                    crate::sync::orgsync::build_orgsync_need(&org_id, col_full, &local_vv, my_seen);
                out.push(OrgsyncOut::Need {
                    to_root_id: from.to_string(),
                    body: need_body,
                });
                push_org_collection_data(
                    storage,
                    &mut out,
                    &org_id,
                    name,
                    version,
                    col_full,
                    remote_vv,
                    *remote_dlog_ack,
                    from,
                );
            }
            crate::sync::orgsync::OrgDiffOutcome::Equal => {
                // 墓碑独立补推
                let Ok(tombs) = crate::sync::orgsync::collect_org_tombstones_after(
                    storage,
                    &org_id,
                    name,
                    version,
                    *remote_dlog_ack,
                ) else {
                    continue;
                };
                if !tombs.is_empty() {
                    let batches = crate::sync::orgsync::split_orgsync_batches(
                        tombs,
                        crate::sync::orgsync::ORGSYNC_BATCH_BYTES,
                    );
                    let total = batches.len();
                    for (i, batch) in batches.into_iter().enumerate() {
                        let body = crate::sync::orgsync::build_orgsync_data_batch(
                            &org_id, col_full, &batch, i, total,
                        );
                        out.push(OrgsyncOut::Data {
                            to_root_id: from.to_string(),
                            body,
                        });
                    }
                }
            }
        }
    }

    // 卫生批项2（replica.rs TODO 落地）：验签成功的 orgsync 交换反哺
    // org-sync-state 记账（账号口径）——overview 的 everSynced 口径覆盖纯
    // orgsync 部署（此前只看 org-share/org-pull 的 sync-state）。
    crate::org::sync_state::note_orgsync_activity(storage, &record, from, ctx.now_ms);

    Ok(InboundDmResult {
        response: ok_response(),
        events: Vec::new(),
        auto_accept: None,
        self_profile: None,
        device_sync_reply: None,
        device_notice_broadcast: false,
        profile_sync_reply: None,
        pdsync_out: Vec::new(),
        orgsync_out: out,
        profile_applied: false,
        orgkey_unbox: Vec::new(),
        feed_blob_out: None,
    })
}

/// orgsync-need：收到复制组成员的 diff 请求。采集该集合增量，分批发回
/// `orgsync-data`。
pub(super) fn handle_orgsync_need<S: StorageBackend>(
    storage: &mut S,
    ctx: &InboundContext<'_>,
    from: &str,
    body: &Value,
) -> Result<InboundDmResult> {
    let Some((org_id, col_full, known_vv, dlog_ack)) =
        crate::sync::orgsync::parse_orgsync_need(body)
    else {
        return done(fail_response("invalid-body"), Vec::new());
    };

    // 公共前置：from ∈ 成员表
    let Ok(Some(record)) = OrganizationService::get_record(storage, &org_id) else {
        return done(fail_response("rejected"), Vec::new());
    };
    if record.find_member(from).is_none() {
        return done(fail_response("rejected"), Vec::new());
    }

    // 解析集合名
    let Some(at) = col_full.rfind("@v") else {
        return done(fail_response("invalid-collection"), Vec::new());
    };
    let name = &col_full[..at];
    let version = &col_full[at + 2..];

    // 查找声明以确定 accounts 轴
    let decl_key = crate::plugindata::org_decl_key(&org_id, name, version);
    let accounts = storage
        .get(&decl_key)
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str::<crate::plugindata::CollectionDeclaration>(&raw).ok())
        .map(|d| d.accounts)
        .unwrap_or_default();

    // 复制组验签
    if !crate::sync::orgsync::is_in_replication_group(&record, from, accounts) {
        log::info!("[ORGSYNC] need rejected: from={from} not in replication group");
        return done(fail_response("rejected"), Vec::new());
    }

    // 推进对端回执水位 + 尝试 GC（F5：need 路径推进水位后也尝试 GC，
    // 对齐 pdsync ack_remote_journal 两路 GC——need 是另一条回执通道）
    if dlog_ack > 0 {
        let _ = crate::sync::orgsync::org_dlog_set_watermark(
            storage,
            &org_id,
            name,
            version,
            from,
            ctx.remote_peer_id,
            dlog_ack,
        );
        let members = crate::sync::orgsync::replication_group_members(&record, accounts);
        if let Ok(threshold) = crate::sync::orgsync::org_dlog_gc_threshold(
            storage,
            &org_id,
            name,
            version,
            &members,
            ctx.my_root_id,
        ) {
            if let Ok(removed) =
                crate::sync::orgsync::org_dlog_gc(storage, &org_id, name, version, threshold)
                && removed > 0
            {
                log::info!(
                    "[ORGSYNC] need dlog gc | org={org_id} col={col_full} removed={removed}"
                );
            }
        }
    }

    // 采集增量
    let Ok(records) = crate::sync::orgsync::collect_org_incremental(
        storage, &org_id, name, version, &known_vv, dlog_ack,
    ) else {
        return done(fail_response("collection-failed"), Vec::new());
    };

    let batches = crate::sync::orgsync::split_orgsync_batches(
        records,
        crate::sync::orgsync::ORGSYNC_BATCH_BYTES,
    );
    let total = batches.len();
    let mut out = Vec::with_capacity(total);
    for (i, batch) in batches.into_iter().enumerate() {
        let body =
            crate::sync::orgsync::build_orgsync_data_batch(&org_id, &col_full, &batch, i, total);
        out.push(OrgsyncOut::Data {
            to_root_id: from.to_string(),
            body,
        });
    }

    // 卫生批项2（replica.rs TODO 落地）：验签成功的 orgsync 交换反哺
    // org-sync-state 记账（账号口径）——overview 的 everSynced 口径覆盖纯
    // orgsync 部署（此前只看 org-share/org-pull 的 sync-state）。
    crate::org::sync_state::note_orgsync_activity(storage, &record, from, ctx.now_ms);

    Ok(InboundDmResult {
        response: ok_response(),
        events: Vec::new(),
        auto_accept: None,
        self_profile: None,
        device_sync_reply: None,
        device_notice_broadcast: false,
        profile_sync_reply: None,
        pdsync_out: Vec::new(),
        orgsync_out: out,
        profile_applied: false,
        orgkey_unbox: Vec::new(),
        feed_blob_out: None,
    })
}

/// orgsync-data：收到复制组成员的增量数据，逐条合入（幂等）。
///
/// B3 红线：逐条校验记录 key 属于该集合数据前缀（`orgd:{orgId}:{name}@v{version}:`）
/// 或保留系统集合 `org:coll:`（B5 声明记录放行），不符整批拒收——防被攻陷/
/// 故障的复制组成员覆写任意 sled 键。
/// O3 工作项 4：数据账号上线 → 冲刷本组织离线写入队列。
///
/// **send-then-delete**（Z4）：只读 `orgq:queue:{org}:*`（不删除），逐集合
/// 装配 orgq-req 写入信封（目标 = 上线的数据账号 `from` rootId，经 orgsync_out
/// 出站），requestId 写**在途记录**（回执可关联——canWrite 拒绝的重放写经
/// orgq-resp denied/rejected 有拒绝反馈路径，至少日志可查）。队列条目**不在
/// flush 时删除**：投递失败（数据账号实际未达/超时）保留队列条目，下次上线
/// 再冲刷重新投递（不丢写）；受理回执（handle_orgq_resp 消费在途）后按集合
/// 清理队列（`orgq_queue_clear_by_collection`）。
fn flush_orgq_offline_queue<S: StorageBackend>(
    storage: &mut S,
    out: &mut Vec<OrgsyncOut>,
    org_id: &str,
    from: &str,
    ctx: &InboundContext<'_>,
) {
    let queued = crate::sync::orgsync::orgq_queue_read_by_org(storage, org_id);
    if queued.is_empty() {
        return;
    }
    log::info!(
        "[ORGSYNC] flush orgq offline queue | org={org_id} collections={} da={}",
        queued.len(),
        &from[..std::cmp::min(16, from.len())]
    );
    // 每个集合一次 orgq-req 写入请求（fresh requestId 关联，写 pending 供回执关联）
    let mut seq: u64 = 0;
    for (collection, records) in queued {
        seq += 1;
        let request_id = crate::sync::orgsync::orgq_gen_request_id(ctx.now_ms + seq as i64);
        let col_full = collection.clone();
        let _ = crate::sync::orgsync::orgq_pending_put(
            storage,
            &request_id,
            org_id,
            &col_full,
            "write",
            from,
            ctx.now_ms + seq as i64,
        );
        let body =
            crate::sync::orgsync::build_orgq_write_req(org_id, &collection, &records, &request_id);
        out.push(OrgsyncOut::OrgqReq {
            to_root_id: from.to_string(),
            body,
        });
    }
}

/// 采集 org 集合增量并分批发入 out。
fn push_org_collection_data<S: StorageBackend>(
    storage: &mut S,
    out: &mut Vec<OrgsyncOut>,
    org_id: &str,
    name: &str,
    version: &str,
    col_full: &str,
    remote_vv: &crate::sync::meta::VersionVector,
    dlog_ack: u64,
    to_root_id: &str,
) {
    let Ok(records) = crate::sync::orgsync::collect_org_incremental(
        storage, org_id, name, version, remote_vv, dlog_ack,
    ) else {
        return;
    };
    let batches = crate::sync::orgsync::split_orgsync_batches(
        records,
        crate::sync::orgsync::ORGSYNC_BATCH_BYTES,
    );
    let total = batches.len();
    for (i, batch) in batches.into_iter().enumerate() {
        let body =
            crate::sync::orgsync::build_orgsync_data_batch(org_id, col_full, &batch, i, total);
        out.push(OrgsyncOut::Data {
            to_root_id: to_root_id.to_string(),
            body,
        });
    }
}


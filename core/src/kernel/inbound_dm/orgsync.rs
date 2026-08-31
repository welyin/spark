//! dm 入站编排（orgsync 系）：orgsync-hello / orgsync-need / orgsync-data
//! 三信封组织域反熵同步。
//!
//! 从 `inbound_dm` 拆出的子模块（O2a），共享父模块的 [`InboundContext`]/
//! 应答助手/[`done`] 等。
//!
//! Z5 拆分挂账：本文件超 650 行硬线（含 410 行内联测试）。测试拆到独立
//! `orgsync_tests.rs` 待做（测试依赖 `use super::*` 的私有辅助，拆分需调整
//! import 面）；功能部分已按 handler 内聚，暂不强行拆分（遵循「能平铺就不
//! 嵌套、拆不动注释挂账」）。
//!
//! 验签规则：
//! 1. 公共前置：from ∈ org:meta 成员表；
//! 2. 同步类另要求：from ∈ 该集合复制组（all-members=全体成员；
//!    data-accounts=当前数据账号集含缺省推导），不符静默丢弃。
//!
//! B3 红线：orgsync-data 入站逐条校验 key 属于该集合数据前缀（`orgd:`
//! 或保留系统集合 `org:coll:`），不符整批拒收（对齐 pdsync.rs 白名单先例）。

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use ed25519_dalek::VerifyingKey;
use serde_json::Value;

use super::{
    InboundContext, InboundDmResult, OrgsyncOut, Result, done, fail_response, ok_response,
};
use crate::org::{OrganizationRecord, OrganizationService};
use crate::storage::StorageBackend;

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
    let Some((org_id, collections, roles, _device_class)) =
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
            .and_then(|raw| serde_json::from_str::<crate::plugindata::CollectionDeclaration>(&raw).ok())
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
        let my_seen =
            crate::sync::orgsync::org_dlog_get_seen(storage, &org_id, name, version, from, ctx.remote_peer_id)
                .unwrap_or(0);

        // 推进对端回执水位 + 尝试 GC（B6：水位按 (from, peerId) 设备粒度）
        if *remote_dlog_ack > 0 {
            let _ = crate::sync::orgsync::org_dlog_set_watermark(
                storage, &org_id, name, version, from, ctx.remote_peer_id, *remote_dlog_ack,
            );
            // 尝试 GC
            let members =
                crate::sync::orgsync::replication_group_members(&record, accounts);
            if let Ok(threshold) = crate::sync::orgsync::org_dlog_gc_threshold(
                storage, &org_id, name, version, &members, ctx.my_root_id,
            ) {
                if let Ok(removed) =
                    crate::sync::orgsync::org_dlog_gc(storage, &org_id, name, version, threshold)
                    && removed > 0
                {
                    log::info!(
                        "[ORGSYNC] dlog gc | org={org_id} col={col_full} removed={removed}"
                    );
                }
            }
        }

        match diff {
            crate::sync::orgsync::OrgDiffOutcome::LocalBehind { local_vv } => {
                let need_body = crate::sync::orgsync::build_orgsync_need(
                    &org_id,
                    col_full,
                    &local_vv,
                    my_seen,
                );
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
                let need_body = crate::sync::orgsync::build_orgsync_need(
                    &org_id,
                    col_full,
                    &local_vv,
                    my_seen,
                );
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
                    storage, &org_id, name, version, *remote_dlog_ack,
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
        orgkey_unbox: None,
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
            storage, &org_id, name, version, from, ctx.remote_peer_id, dlog_ack,
        );
        let members = crate::sync::orgsync::replication_group_members(&record, accounts);
        if let Ok(threshold) = crate::sync::orgsync::org_dlog_gc_threshold(
            storage, &org_id, name, version, &members, ctx.my_root_id,
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
        let body = crate::sync::orgsync::build_orgsync_data_batch(
            &org_id, &col_full, &batch, i, total,
        );
        out.push(OrgsyncOut::Data {
            to_root_id: from.to_string(),
            body,
        });
    }

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
        orgkey_unbox: None,
        feed_blob_out: None,
    })
}

/// orgsync-data：收到复制组成员的增量数据，逐条合入（幂等）。
///
/// B3 红线：逐条校验记录 key 属于该集合数据前缀（`orgd:{orgId}:{name}@v{version}:`）
/// 或保留系统集合 `org:coll:`（B5 声明记录放行），不符整批拒收——防被攻陷/
/// 故障的复制组成员覆写任意 sled 键。
pub(super) fn handle_orgsync_data<S: StorageBackend>(
    storage: &mut S,
    ctx: &InboundContext<'_>,
    from: &str,
    body: &Value,
) -> Result<InboundDmResult> {
    let Some((org_id, col_full, records)) =
        crate::sync::orgsync::parse_orgsync_data(body)
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
        log::info!("[ORGSYNC] data rejected: from={from} not in replication group");
        return done(fail_response("rejected"), Vec::new());
    }

    // B3 白名单：数据键必须属于本集合数据键域（插件 `orgd:` 或内建存量键
    // 前缀）；`org:coll:` 保留系统集合声明记录另行放行（内建 all-members
    // 集合）。
    let data_prefixes = crate::sync::orgsync::collection_data_prefixes(&org_id, name, version);
    let coll_decl_prefix = format!("org:coll:{org_id}:{name}@v{version}");
    // O4：授权名单（org:acl:）为 all-members 系统数据，随本集合组织流量同步。
    let coll_acl_prefix = crate::sync::orgsync::acl_key(&org_id, name, version);
    for record_item in &records {
        let valid = data_prefixes
            .iter()
            .any(|p| record_item.key.starts_with(p))
            || record_item.key.starts_with(&coll_decl_prefix)
            || record_item.key.starts_with(&coll_acl_prefix);
        if !valid {
            // H4：拒绝日志只打 key 前缀截断（越界 key 可能是敏感键如 orgkey:，
            // 全量打印泄漏明文）。
            let key_frag = &record_item.key[..std::cmp::min(16, record_item.key.len())];
            log::info!(
                "[ORGSYNC] data rejected: key out of collection prefix | org={org_id} col={col_full} key={key_frag}"
            );
            return done(fail_response("key-out-of-collection"), Vec::new());
        }
    }

    let mut max_dseq: Option<u64> = None;
    for record_item in &records {
        if let Some(dseq) = record_item.dseq {
            max_dseq = Some(max_dseq.map_or(dseq, |m: u64| m.max(dseq)));
        }

        // O4 §20.7：授权名单（org:acl:）合入走 acl 验签 + whole 合并，不直接
        // LWW 合入——签名者须 ∈ 变更前本地 owners（创世除外）、签名用成员表
        // accessKey 公钥；验签失败拒绝合入保留本地。
        if let Some((a_org_id, a_name, a_version)) =
            crate::sync::orgsync::parse_acl_key(&record_item.key)
        {
            // R3：acl 是 all-members 系统数据，经 org:structure 集合流量全员
            // 同步——允许 acl 的 org 与当前集合一致（org:structure 承载 acl）
            // 或与当前集合完全匹配（插件集合内联旧路径），防跨组织 acl 混入。
            let scope_ok = a_org_id == org_id
                && (a_name == name && a_version == version
                    || name == "org:structure" && version == "1");
            if !scope_ok {
                let key_frag = &record_item.key[..std::cmp::min(16, record_item.key.len())];
                log::info!(
                    "[ORGSYNC] acl scope mismatch | org={org_id} col={col_full} key={key_frag}"
                );
                return done(fail_response("acl-scope-mismatch"), Vec::new());
            }
            if let Some(reason) = apply_acl_record_verified(
                storage, &record, from, &a_org_id, &a_name, &a_version,
                &record_item.value, &record_item.meta, ctx.now_ms,
            )? {
                log::info!(
                    "[ORGSYNC] acl rejected | org={org_id} col={col_full} from={} reason={}",
                    &from[..std::cmp::min(16, from.len())],
                    reason
                );
                return done(fail_response(&reason), Vec::new());
            }
            continue;
        }

        // 逐条 LWW 合入（幂等）。org 记录走 no-dlog 变体（B4：不污染个人域
        // 删除日志），墓碑落地后由本机补登 **org 域 dlog**（接力传播，
        // A→B→C）。
        let value_str = serde_json::to_string(&record_item.value)?;
        let result = crate::sync::apply_personal_remote_no_dlog(
            storage,
            &record_item.key,
            &value_str,
            &record_item.meta,
        )?;

        if result.did_apply() {
            if crate::sync::is_tombstone(&record_item.meta) {
                // 远端墓碑落地 → 补登 org 域 dlog（作用域
                // dlog:org:{orgId}:{name}@v{version}），保证接力传播
                let (_seq, dlog_ops) = crate::sync::orgsync::org_dlog_append_ops(
                    storage, &org_id, name, version, &record_item.key,
                )?;
                let mut ops = dlog_ops;
                // F5：存量组织键（内建 all-members 集合键域 org:meta/ct:org/
                // org:inv，`legacy_org_key_scope` 判定）墓碑**同时补登个人域
                // dlog**——与本地 tombstone_local 双写对称（orgsync 成员间
                // 反熵走 org dlog，pdsync 自设备同步照旧走个人 dlog）。orgd:/
                // org:coll: 键维持 org-dlog-only（O2a B4 语义不变）。
                //
                // 防双写幂等判定：`did_apply` 仅在 vv 领先/并发时成立——若本地
                // 已 tombstone（tombstone_local 已登个人 dlog），远端墓碑 vv
                // 不领先 → did_apply=false 不进入；此处再按个人域 dlog 是否已含
                // 该 key 显式兜底，保证同一删除只从一条路径登一次个人 dlog。
                if crate::sync::orgsync::legacy_org_key_scope(&record_item.key).is_some()
                    && !personal_dlog_has_entry(storage, &record_item.key)?
                {
                    let (_pseq, p_ops) = crate::sync::dlog::append_ops(storage, &record_item.key)?;
                    ops.extend(p_ops);
                }
                storage
                    .batch(ops)
                    .map_err(crate::sync::SyncError::from)?;
            }
            log::info!(
                "[ORGSYNC] data applied | org={org_id} col={col_full} key={}",
                record_item.key
            );
        }
    }

    // 删除日志回执
    let mut out = Vec::new();
    if let Some(dseq) = max_dseq {
        // B6：已收序号按 (from, peerId) 设备粒度
        let _ = crate::sync::orgsync::org_dlog_set_seen(
            storage, &org_id, name, version, from, ctx.remote_peer_id, dseq,
        );
        // 立即回发 need 携带 dlogAck
        let local_vv = crate::sync::orgsync::collect_org_collection_vv(
            storage, &org_id, name, version,
        )
        .unwrap_or_default();
        let seen = crate::sync::orgsync::org_dlog_get_seen(
            storage, &org_id, name, version, from, ctx.remote_peer_id,
        )
        .unwrap_or(0);
        out.push(OrgsyncOut::Need {
            to_root_id: from.to_string(),
            body: crate::sync::orgsync::build_orgsync_need(
                &org_id, &col_full, &local_vv, seen,
            ),
        });
    }

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
        orgkey_unbox: None,
        feed_blob_out: None,
    })
}

/// O4 §20.7：授权名单（org:acl:）记录合入——acl_verify（签名者 ∈ 变更前本地
/// owners，创世除外；签名用成员表 accessKey 公钥）+ acl_merge（whole，updatedAt
/// 大者胜）。验签/合并失败 → 返回拒绝 reason（保留本地，不落库）。
///
/// 返回 `Ok(None)` = 已处理（合入或本地胜出保留）；`Ok(Some(reason))` = 拒绝，
/// 调用方整批拒收。墓碑（value 为 null）放行走普通路径（acl 不常规删除）。
///
/// **根绑定签名说明**：accessKey 仅本人可写、经 org:structure 集合同步传播
/// （自证归属），bind_sig 的根公钥验签需根公钥（root_id 不可逆），入站纯逻辑
/// 层无法独立复核，故此处以 accessKey 公钥验 acl 签名即达「签名者持有对应
/// 组织身份」的门槛；根绑定在发布路径由内核完成。
/// O1 acl 时间窗（org-orgsync.md §20.7）：合入 acl 的 `updatedAt` 与本地时钟
/// 偏差超窗拒绝——防陈旧/伪造时间戳的 acl 抢占。与 dm 信封新鲜度窗口同口径。
const ACL_TS_WINDOW_MS: i64 = 10 * 60_000;

fn apply_acl_record_verified<S: StorageBackend>(
    storage: &mut S,
    record: &OrganizationRecord,
    from: &str,
    org_id: &str,
    name: &str,
    version: &str,
    value: &Value,
    meta: &crate::sync::meta::DocMeta,
    now_ms: i64,
) -> Result<Option<String>> {
    if value.is_null() {
        return Ok(None);
    }
    let incoming: crate::sync::orgsync::AclRecord = match serde_json::from_value(value.clone()) {
        Ok(a) => a,
        Err(_) => return Ok(Some("invalid-acl".to_string())),
    };
    let acl_key = crate::sync::orgsync::acl_key(org_id, name, version);
    let current: crate::sync::orgsync::AclRecord = storage
        .get(&acl_key)
        .ok()
        .flatten()
        .and_then(|r| serde_json::from_str(&r).ok())
        .unwrap_or(crate::sync::orgsync::AclRecord {
            owners: Vec::new(),
            readers: Vec::new(),
            epoch: 0,
            updated_at: 0,
            reset_by: None,
            sig: String::new(),
        });
    let col_full = format!("{name}@v{version}");
    // 签名者 from ∈ 变更前本地 owners（创世例外：本地 acl 为空 → 走创世锚）。
    if !current.is_empty() && !current.is_owner(from) {
        return Ok(Some("acl-signer-not-owner".to_string()));
    }
    // O1：创世（本地 acl 为空）——签名者须 == 该集合声明的 declaredBy
    // （声明记录内核已强制 declaredBy=声明者 rootId，此处对齐；防任意成员
    // 抢先自签创世 acl 抢占 owner 权）。
    if current.is_empty() {
        let decl_key = crate::plugindata::org_decl_key(org_id, name, version);
        let declared_by = storage
            .get(&decl_key)
            .ok()
            .flatten()
            .and_then(|raw| serde_json::from_str::<crate::plugindata::CollectionDeclaration>(&raw).ok())
            .and_then(|d| d.declared_by);
        if declared_by.as_deref() != Some(from) {
            return Ok(Some("acl-genesis-signer-not-declared-by".to_string()));
        }
    }
    // 成员表 accessKey 公钥
    let signer_pk = record
        .find_member(from)
        .and_then(|m| m.access_key.as_ref())
        .and_then(|ak| {
            let Ok(bytes) = B64.decode(&ak.public_key) else {
                return None;
            };
            let Ok(arr) = <[u8; 32]>::try_from(bytes.as_slice()) else {
                return None;
            };
            VerifyingKey::from_bytes(&arr).ok()
        });
    let Some(signer_pk) = signer_pk else {
        log::info!("[ORGSYNC] acl signer has no accessKey from={from}");
        return Ok(Some("acl-signer-no-access-key".to_string()));
    };
    if !crate::sync::orgsync::acl_verify(&incoming, org_id, &col_full, &signer_pk) {
        return Ok(Some("acl-bad-signature".to_string()));
    }
    // O1：updatedAt 时间窗——incoming 时间戳与本地时钟偏差超窗拒绝
    // （防陈旧/伪造 acl 抢占；与 dm 信封新鲜度同口径）。用饱和算术防溢出。
    if now_ms.saturating_sub(incoming.updated_at).saturating_abs() > ACL_TS_WINDOW_MS {
        log::info!(
            "[ORGSYNC] acl ts out of window | org={org_id} col={col_full} updatedAt={}",
            incoming.updated_at
        );
        return Ok(Some("acl-ts-out-of-window".to_string()));
    }
    // O1：epoch 单调性——incoming epoch 低于本地当前 epoch（reset 例外已由
    // resetBy 标记显式化）→ 拒绝降级/回退。
    if !current.is_empty() && incoming.epoch < current.epoch && incoming.reset_by.is_none() {
        return Ok(Some("acl-epoch-regress".to_string()));
    }
    // whole 合并：incoming updatedAt 大者胜；本地胜出 → 保持本地（不重写 vv）
    let merged = crate::sync::orgsync::acl_merge(&current, &incoming);
    if merged.updated_at == current.updated_at && merged.sig == current.sig {
        return Ok(None);
    }
    let merged_str = serde_json::to_string(&merged)?;
    crate::sync::apply_personal_remote_no_dlog(storage, &acl_key, &merged_str, meta)?;
    log::info!("[ORGSYNC] acl merged | org={org_id} col={col_full}");
    Ok(None)
}

/// F5 幂等判定：个人域 dlog（`dlog:entry:{seq}` → record_key）是否已含该
/// 记录键——远端存量组织键墓碑补登个人 dlog 前检查，防同一删除双写。
fn personal_dlog_has_entry<S: StorageBackend>(
    storage: &S,
    record_key: &str,
) -> Result<bool> {
    let entries = crate::sync::dlog::entries_after(storage, 0)
        .map_err(crate::sync::SyncError::from)?;
    Ok(entries.iter().any(|(_, k)| k == record_key))
}

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
        let body = crate::sync::orgsync::build_orgq_write_req(
            org_id,
            &collection,
            &records,
            &request_id,
        );
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
        let body = crate::sync::orgsync::build_orgsync_data_batch(
            org_id, col_full, &batch, i, total,
        );
        out.push(OrgsyncOut::Data {
            to_root_id: to_root_id.to_string(),
            body,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::org::types::{OrganizationMember, OrganizationRecord, OrganizationRole};
    use crate::plugindata::{Accounts, Scope, Space, declare};
    use crate::storage::{MemoryStorage, ScanOptions};
    use crate::sync::meta::DocMeta;
    use serde_json::json;

    fn member(root_id: &str) -> OrganizationMember {
        OrganizationMember {
            root_id: root_id.to_string(),
            role: OrganizationRole::Member,
            joined_at: 1000,
            added_by: "creator".to_string(),
            node_info: None,
            nickname: None,
            avatar: None,
            signature: None,
            gender: None,
            region: None,
            use_personal_identity: None,
            access_key: None,
            extra: Default::default(),
        }
    }

    fn ctx<'a>(
        my_root_id: &'a str,
        remote_peer_id: &'a str,
        online: &'a std::collections::HashSet<String>,
    ) -> InboundContext<'a> {
        InboundContext {
            my_root_id,
            my_nickname: "me",
            remote_peer_id,
            online_peers: online,
            node_id: "local-node",
            now_ms: 2000,
            kverify: None,
        }
    }

    fn setup_org_and_collection() -> MemoryStorage {
        let mut s = MemoryStorage::new();
        // 组织记录：member-a（发送方/接收方）与 self（本机）均为成员
        let record = OrganizationRecord {
            org_id: "org_0000000000000001".to_string(),
            name: "t".to_string(),
            description: String::new(),
            avatar: String::new(),
            base_plugin_domain: None,
            created_at: 1000,
            created_by: "self".to_string(),
            updated_at: 1000,
            members: vec![member("member-a"), member("self")],
            sync: None,
            gateways: vec![],
            data_accounts: vec![],
            org_address: None,
            is_public: false,
            extra: Default::default(),
        };
        crate::org::OrganizationService::save_record(&mut s, &record).unwrap();
        // 声明集合：all-members（复制组 = 全体成员）
        let decl = declare(
            &mut s,
            "ai-chat",
            crate::plugindata::DeclareInput {
                name: "ai-chat:finance".to_string(),
                version: Some("1.0.0".to_string()),
                space: Some(Space::Org),
                accounts: Some(Accounts::AllMembers),
                scope: Some(Scope::Sync),
                ..Default::default()
            },
            1000,
            Some("org_0000000000000001"),
        )
        .unwrap();
        let _ = decl;
        s
    }

    fn meta(node: &str, counter: i64) -> DocMeta {
        DocMeta {
            vv: [(node.to_string(), counter)].into_iter().collect(),
            ts: 2000,
            node_id: Some(node.to_string()),
            ..Default::default()
        }
    }

    /// B3：orgsync-data 入站 key 白名单——`orgd:` 数据键放行，
    /// 越界键（`p2p:` 等）整批拒收。
    #[test]
    fn orgsync_data_key_whitelist_rejects_out_of_collection() {
        let mut s = setup_org_and_collection();
        let online = std::collections::HashSet::new();
        let c = ctx("self", "peer-a", &online);

        // 合法：orgd: 数据键 → 应应用（返回 ok，orgsync_out 非空）
        let ok_body = crate::sync::orgsync::build_orgsync_data_batch(
            "org_0000000000000001",
            "ai-chat:finance@v1.0.0",
            &[crate::sync::orgsync::OrgsyncRecord {
                key: "orgd:org_0000000000000001:ai-chat:finance@v1.0.0:k1".to_string(),
                value: serde_json::json!("v"),
                meta: meta("node-a", 1),
                dseq: None,
            }],
            0,
            1,
        );
        let res = handle_orgsync_data(&mut s, &c, "member-a", &ok_body).unwrap();
        assert_eq!(res.response["ok"], json!(true), "合法 orgd 键应放行");
        assert!(
            s.get("orgd:org_0000000000000001:ai-chat:finance@v1.0.0:k1")
                .unwrap()
                .is_some(),
            "合法记录已合入"
        );

        // 越界：p2p: 键 → 整批拒收（reason key-out-of-collection）
        let bad_body = crate::sync::orgsync::build_orgsync_data_batch(
            "org_0000000000000001",
            "ai-chat:finance@v1.0.0",
            &[crate::sync::orgsync::OrgsyncRecord {
                key: "p2p:identity:privateKey".to_string(),
                value: serde_json::json!("x"),
                meta: meta("node-a", 1),
                dseq: None,
            }],
            0,
            1,
        );
        let res2 = handle_orgsync_data(&mut s, &c, "member-a", &bad_body).unwrap();
        assert_eq!(res2.response["ok"], json!(false));
        assert_eq!(res2.response["reason"], json!("key-out-of-collection"));
        assert!(
            s.get("p2p:identity:privateKey").unwrap().is_none(),
            "越界键不得落库"
        );

        // O4 红线：orgkey（集合对称密钥，personal 域）**永不进组织同步流量**——
        // orgsync-data 白名单只放行 orgd:/org:coll:/org:acl:/存量组织键，orgkey:
        // 越界整批拒收（密钥只经 dm 定向投递 + pdsync 自设备扩散）。
        let key_body = crate::sync::orgsync::build_orgsync_data_batch(
            "org_0000000000000001",
            "ai-chat:finance@v1.0.0",
            &[crate::sync::orgsync::OrgsyncRecord {
                key: crate::sync::orgsync::orgkey_key(
                    "org_0000000000000001",
                    "ai-chat:finance",
                    "1.0.0",
                    1,
                ),
                value: serde_json::json!("secret"),
                meta: meta("node-a", 1),
                dseq: None,
            }],
            0,
            1,
        );
        let res3 = handle_orgsync_data(&mut s, &c, "member-a", &key_body).unwrap();
        assert_eq!(res3.response["ok"], json!(false));
        assert_eq!(res3.response["reason"], json!("key-out-of-collection"));
        assert!(
            s.get(&crate::sync::orgsync::orgkey_key(
                "org_0000000000000001",
                "ai-chat:finance",
                "1.0.0",
                1
            ))
            .unwrap()
            .is_none(),
            "orgkey 密文不得经组织同步流量落库"
        );
    }

    /// B4：orgsync-data 入站远端墓碑落地后补登 **org 域 dlog**（接力传播），
    /// 个人域 dlog 不被 orgd: 污染。
    #[test]
    fn orgsync_data_tombstone_relay_appends_org_dlog() {
        let mut s = setup_org_and_collection();
        let online = std::collections::HashSet::new();
        let c = ctx("self", "peer-a", &online);
        let key = "orgd:org_0000000000000001:ai-chat:finance@v1.0.0:k1";

        // 先落一条本地记录
        s.put(key, "\"v1\"").unwrap();
        s.put(
            &format!("pmeta:{key}"),
            &serde_json::to_string(&DocMeta {
                vv: [("node-a".to_string(), 1)].into_iter().collect(),
                ts: 1000,
                node_id: Some("node-a".to_string()),
                ..Default::default()
            })
            .unwrap(),
        )
        .unwrap();

        // 入站远端墓碑（vv=2 领先）
        let body = crate::sync::orgsync::build_orgsync_data_batch(
            "org_0000000000000001",
            "ai-chat:finance@v1.0.0",
            &[crate::sync::orgsync::OrgsyncRecord {
                key: key.to_string(),
                value: serde_json::Value::Null,
                meta: DocMeta {
                    vv: [("node-a".to_string(), 2)].into_iter().collect(),
                    ts: 2000,
                    node_id: Some("node-a".to_string()),
                    tombstone: Some(true),
                },
                dseq: Some(3),
            }],
            0,
            1,
        );
        handle_orgsync_data(&mut s, &c, "member-a", &body).unwrap();

        // 个人域 dlog 为空
        let personal_dlog: Vec<_> = s
            .scan(&ScanOptions::prefix("dlog:entry:"))
            .unwrap()
            .into_iter()
            .collect();
        assert!(personal_dlog.is_empty(), "个人域 dlog 不被 orgd 污染");
        // org 域 dlog 已补登
        let entries = crate::sync::orgsync::org_dlog_entries_after(
            &s,
            "org_0000000000000001",
            "ai-chat:finance",
            "1.0.0",
            0,
        )
        .unwrap();
        assert_eq!(entries.len(), 1, "远端墓碑接力进 org dlog");
        assert_eq!(entries[0].1, key);
    }

    /// O2b 双线幂等合入：存量组织键（内建 all-members 集合）经 orgsync-data
    /// 到达，与既有同 vv 数据合并幂等——重复/并发双线（orgsync + pdsync）
    /// 到达不重复 bump vv、值不被旧版本覆盖。
    #[test]
    fn orgsync_data_builtin_collection_merges_idempotently() {
        let mut s = setup_org_and_collection();
        let online = std::collections::HashSet::new();
        let c = ctx("self", "peer-a", &online);
        // 声明内建 org:contacts 集合（all-members，声明记录 + pmeta）
        let org_id = "org_0000000000000001";
        let decl_key = crate::plugindata::org_decl_key(org_id, "org:contacts", "1");
        let decl = declare(
            &mut s,
            "org",
            crate::plugindata::DeclareInput {
                name: "org:contacts".to_string(),
                version: Some("1".to_string()),
                space: Some(Space::Org),
                accounts: Some(Accounts::AllMembers),
                scope: Some(Scope::Sync),
                ..Default::default()
            },
            1000,
            Some(org_id),
        )
        .unwrap();
        s.put(&decl_key, &serde_json::to_string(&decl).unwrap()).unwrap();
        s.put(
            &format!("pmeta:{decl_key}"),
            &serde_json::to_string(&meta("node-a", 1)).unwrap(),
        )
        .unwrap();

        // 存量键 ct:org:{orgId}:* 经 orgsync 到达（vv node-a=1）
        let key = "ct:org:org_0000000000000001:member-x";
        let remote_meta = DocMeta {
            vv: [("node-a".to_string(), 1)].into_iter().collect(),
            ts: 1500,
            node_id: Some("node-a".to_string()),
            ..Default::default()
        };
        let body = crate::sync::orgsync::build_orgsync_data_batch(
            org_id,
            "org:contacts@v1",
            &[crate::sync::orgsync::OrgsyncRecord {
                key: key.to_string(),
                value: json!("member-value"),
                meta: remote_meta.clone(),
                dseq: None,
            }],
            0,
            1,
        );
        handle_orgsync_data(&mut s, &c, "member-a", &body).unwrap();
        assert_eq!(s.get(key).unwrap().as_deref(), Some("\"member-value\""));
        let stored = crate::sync::get_personal_meta(&s, key).unwrap().unwrap();
        assert_eq!(stored.vv.get("node-a"), Some(&1));

        // 双线幂等：同 vv 再次到达（pdsync/orgsync 并发重放）→ 不重复 bump、
        // 值不翻转
        let body2 = crate::sync::orgsync::build_orgsync_data_batch(
            org_id,
            "org:contacts@v1",
            &[crate::sync::orgsync::OrgsyncRecord {
                key: key.to_string(),
                value: json!("member-value"),
                meta: remote_meta,
                dseq: None,
            }],
            0,
            1,
        );
        handle_orgsync_data(&mut s, &c, "member-a", &body2).unwrap();
        assert_eq!(s.get(key).unwrap().as_deref(), Some("\"member-value\""));
        let stored2 = crate::sync::get_personal_meta(&s, key).unwrap().unwrap();
        assert_eq!(stored2.vv.get("node-a"), Some(&1), "同 vv 重复到达不重复 bump");
    }

    /// F5：远端合入**存量组织键**（内建集合键域 ct:org:）墓碑 → org dlog 与
    /// 个人域 dlog **双有**（与本地 tombstone_local 双写对称：orgsync 走 org
    /// dlog、pdsync 自设备同步走个人 dlog）。
    #[test]
    fn orgsync_data_legacy_key_tombstone_writes_both_dlogs() {
        let mut s = setup_org_and_collection();
        let online = std::collections::HashSet::new();
        let c = ctx("self", "peer-a", &online);
        let org_id = "org_0000000000000001";
        // 声明 org:contacts 内建集合（存量键域 ct:org:{orgId}:*）
        let decl_key = crate::plugindata::org_decl_key(org_id, "org:contacts", "1");
        let decl = declare(
            &mut s,
            "org",
            crate::plugindata::DeclareInput {
                name: "org:contacts".to_string(),
                version: Some("1".to_string()),
                space: Some(Space::Org),
                accounts: Some(Accounts::AllMembers),
                scope: Some(Scope::Sync),
                ..Default::default()
            },
            1000,
            Some(org_id),
        )
        .unwrap();
        s.put(&decl_key, &serde_json::to_string(&decl).unwrap()).unwrap();
        s.put(
            &format!("pmeta:{decl_key}"),
            &serde_json::to_string(&meta("node-a", 1)).unwrap(),
        )
        .unwrap();
        // 先落一条存量数据：声明 pmeta（node-a:1）已把 node-a 序号种子到 1，
        // 本次受管写拿到 per-node 序号 2 → vv={node-a:2}
        let key = "ct:org:org_0000000000000001:member-x";
        crate::sync::put_personal(&mut s, "node-a", key, "\"v1\"", 1000).unwrap();
        // 远端墓碑（vv=3 领先本地 2）经 orgsync-data 到达
        let body = crate::sync::orgsync::build_orgsync_data_batch(
            org_id,
            "org:contacts@v1",
            &[crate::sync::orgsync::OrgsyncRecord {
                key: key.to_string(),
                value: serde_json::Value::Null,
                meta: DocMeta {
                    vv: [("node-a".to_string(), 3)].into_iter().collect(),
                    ts: 2000,
                    node_id: Some("node-a".to_string()),
                    tombstone: Some(true),
                },
                dseq: Some(4),
            }],
            0,
            1,
        );
        handle_orgsync_data(&mut s, &c, "member-a", &body).unwrap();
        // org dlog 有（接力）
        let org_entries = crate::sync::orgsync::org_dlog_entries_after(&s, org_id, "org:contacts", "1", 0)
            .unwrap();
        assert_eq!(org_entries.len(), 1, "存量键墓碑登 org dlog");
        assert_eq!(org_entries[0].1, key);
        // 个人域 dlog 也有（pdsync 自设备同步）
        let personal_entries: Vec<_> = s
            .scan(&ScanOptions::prefix("dlog:entry:"))
            .unwrap()
            .into_iter()
            .collect();
        assert!(
            personal_entries.iter().any(|(_, v)| v == key),
            "存量键墓碑同时登个人域 dlog"
        );
    }

    /// F5 防双写幂等：同一存量键墓碑再次（同 vv）到达 → did_apply=false，
    /// 个人域 dlog 不重复补登（保持单条目）。
    #[test]
    fn orgsync_data_legacy_tombstone_does_not_double_log_personal() {
        let mut s = setup_org_and_collection();
        let online = std::collections::HashSet::new();
        let c = ctx("self", "peer-a", &online);
        let org_id = "org_0000000000000001";
        let decl_key = crate::plugindata::org_decl_key(org_id, "org:contacts", "1");
        let decl = declare(
            &mut s,
            "org",
            crate::plugindata::DeclareInput {
                name: "org:contacts".to_string(),
                version: Some("1".to_string()),
                space: Some(Space::Org),
                accounts: Some(Accounts::AllMembers),
                scope: Some(Scope::Sync),
                ..Default::default()
            },
            1000,
            Some(org_id),
        )
        .unwrap();
        s.put(&decl_key, &serde_json::to_string(&decl).unwrap()).unwrap();
        s.put(
            &format!("pmeta:{decl_key}"),
            &serde_json::to_string(&meta("node-a", 1)).unwrap(),
        )
        .unwrap();
        let key = "ct:org:org_0000000000000001:member-y";
        // 声明 pmeta（node-a:1）把 node-a 序号种子到 1，本地存量数据拿到序号 2
        crate::sync::put_personal(&mut s, "node-a", key, "\"v1\"", 1000).unwrap();
        // 远端墓碑 vv=3 领先本地 2 → 首达合入（登个人 dlog），同 vv 重放不重复登
        let tomb_meta = DocMeta {
            vv: [("node-a".to_string(), 3)].into_iter().collect(),
            ts: 2000,
            node_id: Some("node-a".to_string()),
            tombstone: Some(true),
        };
        let body = crate::sync::orgsync::build_orgsync_data_batch(
            org_id,
            "org:contacts@v1",
            &[crate::sync::orgsync::OrgsyncRecord {
                key: key.to_string(),
                value: serde_json::Value::Null,
                meta: tomb_meta.clone(),
                dseq: Some(4),
            }],
            0,
            1,
        );
        handle_orgsync_data(&mut s, &c, "member-a", &body).unwrap();
        // 同 vv 墓碑重放 → 不重复补登个人 dlog
        let body2 = crate::sync::orgsync::build_orgsync_data_batch(
            org_id,
            "org:contacts@v1",
            &[crate::sync::orgsync::OrgsyncRecord {
                key: key.to_string(),
                value: serde_json::Value::Null,
                meta: tomb_meta,
                dseq: Some(4),
            }],
            0,
            1,
        );
        handle_orgsync_data(&mut s, &c, "member-a", &body2).unwrap();
        let personal_entries: Vec<_> = s
            .scan(&ScanOptions::prefix("dlog:entry:"))
            .unwrap()
            .into_iter()
            .filter(|(_, v)| v == key)
            .collect();
        assert_eq!(personal_entries.len(), 1, "同 vv 重放不重复登个人 dlog");
    }

    /// O4 §20.7 acl 合入验签：owner（member-a，accessKey 已发布）签名的 acl
    /// 经 orgsync-data 到达 → 验签通过合入；篡改签名/非 owner 签名 → 拒绝保留
    /// 本地；未发布 accessKey 的成员发起的变更 → 拒绝并保留本地。
    #[test]
    fn orgsync_acl_verified_merge_positive_and_negative() {
        use base64::Engine as _;
        use base64::engine::general_purpose::STANDARD as B64;
        use crate::identity::derive_domain_identity;

        let org_id = "org_0000000000000001";
        let name = "ai-chat:finance";
        let version = "1.0.0";
        let col_full = format!("{name}@v{version}");
        // owner = member-a：org-access 域身份（seed 派生）+ accessKey 发布
        let owner_seed = [7u8; 64];
        let owner_domain = derive_domain_identity(&owner_seed, &format!("org-access:{org_id}"));
        let owner_pk_b64 = B64.encode(owner_domain.public_key());
        let owner_root = "owner-a".to_string() + &"a".repeat(49);
        let self_root = "self-a".to_string() + &"a".repeat(49);
        let mut s = MemoryStorage::new();
        let record = OrganizationRecord {
            org_id: org_id.to_string(),
            name: "t".to_string(),
            description: String::new(),
            avatar: String::new(),
            base_plugin_domain: None,
            created_at: 1000,
            created_by: self_root.clone(),
            updated_at: 1000,
            members: vec![
                OrganizationMember {
                    root_id: owner_root.clone(),
                    role: OrganizationRole::Admin,
                    joined_at: 1000,
                    added_by: self_root.clone(),
                    node_info: None,
                    nickname: None,
                    avatar: None,
                    signature: None,
                    gender: None,
                    region: None,
                    use_personal_identity: None,
                    access_key: Some(crate::org::types::OrganizationAccessKey {
                        public_key: owner_pk_b64.clone(),
                        // bind_sig 由内核发布路径生成；入站验签以 accessKey 公钥
                        // 验 acl 签名，故测试填充任意非空串（结构上存在即可）
                        bind_sig: "bind".to_string(),
                    }),
                    extra: Default::default(),
                },
                OrganizationMember {
                    root_id: self_root.clone(),
                    role: OrganizationRole::Admin,
                    joined_at: 1000,
                    added_by: self_root.clone(),
                    node_info: None,
                    nickname: None,
                    avatar: None,
                    signature: None,
                    gender: None,
                    region: None,
                    use_personal_identity: None,
                    access_key: None,
                    extra: Default::default(),
                },
            ],
            sync: None,
            gateways: vec![],
            data_accounts: vec![],
            org_address: None,
            is_public: false,
            extra: Default::default(),
        };
        crate::org::OrganizationService::save_record(&mut s, &record).unwrap();
        // 声明 all-members 集合（复制组 = 全体成员）。O1：创世 acl 锚定
        // declaredBy——声明者（owner_root）须在声明记录中登记，测试对齐真实
        // 发布路径（kernel 强制 declaredBy=调用方）。
        let decl = declare(
            &mut s,
            "ai-chat",
            crate::plugindata::DeclareInput {
                name: name.to_string(),
                version: Some(version.to_string()),
                space: Some(Space::Org),
                accounts: Some(Accounts::AllMembers),
                scope: Some(Scope::Sync),
                declared_by: Some(owner_root.clone()),
                ..Default::default()
            },
            1000,
            Some(org_id),
        )
        .unwrap();
        let _ = decl;

        let acl_key = crate::sync::orgsync::acl_key(org_id, name, version);
        let online = std::collections::HashSet::new();
        let c = ctx(&self_root, "peer-a", &online);

        // 构造 owner 签名的 acl（创世：owners=[owner]）
        let make_acl = |epoch: u64, owners: Vec<String>, readers: Vec<String>, updated_at: i64| {
            let payload = crate::sync::orgsync::acl_sign_payload(
                epoch, org_id, &col_full, &owners, &readers, None, updated_at,
            );
            let sig = crate::sync::orgsync::acl_sign(&owner_domain.signing_key, &payload);
            serde_json::json!({
                "owners": owners,
                "readers": readers,
                "epoch": epoch,
                "updatedAt": updated_at,
                "sig": sig,
            })
        };
        // (a) 正向：owner 签名 acl 合入（owners=[owner, self]，self 也是 owner
        // 但无 accessKey——供 (c) 走「无 accessKey」分支）
        let value = make_acl(
            1,
            vec![owner_root.clone(), self_root.clone()],
            vec![owner_root.clone()],
            100,
        );
        let body = crate::sync::orgsync::build_orgsync_data_batch(
            org_id,
            &col_full,
            &[crate::sync::orgsync::OrgsyncRecord {
                key: acl_key.clone(),
                value: value.clone(),
                meta: meta("node-a", 1),
                dseq: None,
            }],
            0,
            1,
        );
        let r = handle_orgsync_data(&mut s, &c, &owner_root, &body).unwrap();
        assert_eq!(r.response["ok"], json!(true), "owner 签名 acl 合入");
        let stored: crate::sync::orgsync::AclRecord =
            serde_json::from_str(&s.get(&acl_key).unwrap().unwrap()).unwrap();
        assert!(stored.is_owner(&owner_root), "合入 acl owner 正确");

        // (b) 负向：先按合法 readers 签名，再篡改 readers 字段（签名不再匹配）→
        // acl-bad-signature 拒绝，本地保留
        let mut tampered = make_acl(2, vec![owner_root.clone()], vec![owner_root.clone()], 200);
        tampered["readers"] = json!(["evil"]);
        let body = crate::sync::orgsync::build_orgsync_data_batch(
            org_id,
            &col_full,
            &[crate::sync::orgsync::OrgsyncRecord {
                key: acl_key.clone(),
                value: tampered,
                meta: meta("node-a", 2),
                dseq: None,
            }],
            0,
            1,
        );
        let r2 = handle_orgsync_data(&mut s, &c, &owner_root, &body).unwrap();
        assert_eq!(r2.response["ok"], json!(false));
        assert_eq!(r2.response["reason"], json!("acl-bad-signature"));
        let stored2: crate::sync::orgsync::AclRecord =
            serde_json::from_str(&s.get(&acl_key).unwrap().unwrap()).unwrap();
        assert_eq!(stored2.epoch, 1, "篡改 acl 拒绝，本地保留 epoch=1");

        // (c) 负向：未发布 accessKey 的成员（self 本机无 accessKey）发起的变更
        // → acl-signer-no-access-key 拒绝，本地保留
        let self_payload = crate::sync::orgsync::acl_sign_payload(
            1, org_id, &col_full, &[self_root.clone()], &[self_root.clone()], None, 300,
        );
        // 用 owner 的域身份签（self 无域身份可用；重点是走「无 accessKey」分支）
        let self_sig = crate::sync::orgsync::acl_sign(&owner_domain.signing_key, &self_payload);
        let self_value = serde_json::json!({
            "owners": [self_root],
            "readers": [self_root],
            "epoch": 1,
            "updatedAt": 300,
            "sig": self_sig,
        });
        let body3 = crate::sync::orgsync::build_orgsync_data_batch(
            org_id,
            &col_full,
            &[crate::sync::orgsync::OrgsyncRecord {
                key: acl_key.clone(),
                value: self_value,
                meta: meta("node-self", 3),
                dseq: None,
            }],
            0,
            1,
        );
        let r3 = handle_orgsync_data(&mut s, &c, &self_root, &body3).unwrap();
        assert_eq!(r3.response["ok"], json!(false));
        assert_eq!(r3.response["reason"], json!("acl-signer-no-access-key"));
        let stored3: crate::sync::orgsync::AclRecord =
            serde_json::from_str(&s.get(&acl_key).unwrap().unwrap()).unwrap();
        assert_eq!(stored3.epoch, 1, "无 accessKey 变更拒绝，本地保留");
    }

    /// O1 创世锚 + 时间窗 + epoch 单调性：
    /// - 创世 acl 签名者必须是声明记录 declaredBy（非声明者抢先自签创世 → 拒绝）；
    /// - 合入 acl 的 updatedAt 与本地时钟偏差超窗 → 拒绝；
    /// - 合入 acl 的 epoch 低于本地当前 epoch（非 reset）→ 拒绝降级。
    #[test]
    fn orgsync_acl_genesis_anchor_time_window_and_epoch_monotonic() {
        use base64::Engine as _;
        use base64::engine::general_purpose::STANDARD as B64;
        use crate::identity::derive_domain_identity;

        let org_id = "org_0000000000000001";
        let name = "ai-chat:fin2";
        let version = "1.0.0";
        let col_full = format!("{name}@v{version}");
        // 声明者 = owner（declaredBy），攻击者 = other（有 accessKey 但非声明者）
        let owner_seed = [21u8; 64];
        let other_seed = [22u8; 64];
        let owner_domain =
            derive_domain_identity(&owner_seed, &format!("org-access:{org_id}"));
        let other_domain =
            derive_domain_identity(&other_seed, &format!("org-access:{org_id}"));
        let owner_root = "owner-b".to_string() + &"b".repeat(48);
        let other_root = "other-c".to_string() + &"c".repeat(48);
        let self_root = "self-d".to_string() + &"d".repeat(48);

        let mut s = MemoryStorage::new();
        let record = OrganizationRecord {
            org_id: org_id.to_string(),
            name: "t".to_string(),
            description: String::new(),
            avatar: String::new(),
            base_plugin_domain: None,
            created_at: 1000,
            created_by: self_root.clone(),
            updated_at: 1000,
            members: vec![
                OrganizationMember {
                    root_id: owner_root.clone(),
                    role: OrganizationRole::Member,
                    joined_at: 1000,
                    added_by: self_root.clone(),
                    node_info: None,
                    nickname: None,
                    avatar: None,
                    signature: None,
                    gender: None,
                    region: None,
                    use_personal_identity: None,
                    access_key: Some(crate::org::types::OrganizationAccessKey {
                        public_key: B64.encode(owner_domain.public_key()),
                        bind_sig: "bind".to_string(),
                    }),
                    extra: Default::default(),
                },
                OrganizationMember {
                    root_id: other_root.clone(),
                    role: OrganizationRole::Member,
                    joined_at: 1000,
                    added_by: self_root.clone(),
                    node_info: None,
                    nickname: None,
                    avatar: None,
                    signature: None,
                    gender: None,
                    region: None,
                    use_personal_identity: None,
                    access_key: Some(crate::org::types::OrganizationAccessKey {
                        public_key: B64.encode(other_domain.public_key()),
                        bind_sig: "bind".to_string(),
                    }),
                    extra: Default::default(),
                },
                OrganizationMember {
                    root_id: self_root.clone(),
                    role: OrganizationRole::Member,
                    joined_at: 1000,
                    added_by: self_root.clone(),
                    node_info: None,
                    nickname: None,
                    avatar: None,
                    signature: None,
                    gender: None,
                    region: None,
                    use_personal_identity: None,
                    access_key: None,
                    extra: Default::default(),
                },
            ],
            sync: None,
            gateways: vec![],
            data_accounts: vec![],
            org_address: None,
            is_public: false,
            extra: Default::default(),
        };
        crate::org::OrganizationService::save_record(&mut s, &record).unwrap();
        // 声明集合（declaredBy = owner_root）
        declare(
            &mut s,
            "ai-chat",
            crate::plugindata::DeclareInput {
                name: name.to_string(),
                version: Some(version.to_string()),
                space: Some(Space::Org),
                accounts: Some(Accounts::AllMembers),
                scope: Some(Scope::Sync),
                declared_by: Some(owner_root.clone()),
                ..Default::default()
            },
            1000,
            Some(org_id),
        )
        .unwrap();

        let acl_key = crate::sync::orgsync::acl_key(org_id, name, version);
        let online = std::collections::HashSet::new();
        // ctx now_ms = 2000（create_ctx 固定）——时间窗 ±10min 内
        let c = ctx(&self_root, "peer-a", &online);

        let make_acl = |signing: &ed25519_dalek::SigningKey,
                        epoch: u64,
                        owners: Vec<String>,
                        readers: Vec<String>,
                        updated_at: i64| {
            let payload = crate::sync::orgsync::acl_sign_payload(
                epoch, org_id, &col_full, &owners, &readers, None, updated_at,
            );
            let sig = crate::sync::orgsync::acl_sign(signing, &payload);
            serde_json::json!({
                "owners": owners,
                "readers": readers,
                "epoch": epoch,
                "updatedAt": updated_at,
                "sig": sig,
            })
        };
        let deliver_acl = |s: &mut MemoryStorage, from: &str, value: Value| {
            let body = crate::sync::orgsync::build_orgsync_data_batch(
                org_id,
                &col_full,
                &[crate::sync::orgsync::OrgsyncRecord {
                    key: acl_key.clone(),
                    value,
                    meta: meta("node-x", 1),
                    dseq: None,
                }],
                0,
                1,
            );
            handle_orgsync_data(s, &c, from, &body).unwrap()
        };

        // (1) 创世抢注反例：非声明者（other）抢先自签创世 acl → 拒绝
        let squatter = make_acl(
            &other_domain.signing_key,
            1,
            vec![other_root.clone()],
            vec![other_root.clone()],
            1500,
        );
        let r1 = deliver_acl(&mut s, &other_root, squatter);
        assert_eq!(r1.response["ok"], json!(false));
        assert_eq!(r1.response["reason"], json!("acl-genesis-signer-not-declared-by"));
        assert!(s.get(&acl_key).unwrap().is_none(), "创世抢注 acl 不落库");

        // (2) 正向：声明者（owner）创世 acl 合入
        let genesis = make_acl(
            &owner_domain.signing_key,
            1,
            vec![owner_root.clone()],
            vec![owner_root.clone()],
            1500,
        );
        let r2 = deliver_acl(&mut s, &owner_root, genesis);
        assert_eq!(r2.response["ok"], json!(true), "声明者创世 acl 合入");

        // (3) 时间窗外（真正超窗）：updatedAt = now - 11min → 拒绝
        let stale_ts = 2000 - super::ACL_TS_WINDOW_MS - 1;
        let stale = make_acl(
            &owner_domain.signing_key,
            2,
            vec![owner_root.clone()],
            vec![owner_root.clone(), other_root.clone()],
            stale_ts,
        );
        let r3 = deliver_acl(&mut s, &owner_root, stale);
        assert_eq!(r3.response["ok"], json!(false));
        assert_eq!(r3.response["reason"], json!("acl-ts-out-of-window"));
        let stored: crate::sync::orgsync::AclRecord =
            serde_json::from_str(&s.get(&acl_key).unwrap().unwrap()).unwrap();
        assert_eq!(stored.epoch, 1, "时间窗外 acl 拒绝，保留本地 epoch=1");

        // (4) epoch 回退：owner 提交 epoch=1（< 当前 1？需 < 当前）——
        // 当前 epoch=1，提交 epoch=0 非 reset → acl-epoch-regress 拒绝
        let regress = make_acl(
            &owner_domain.signing_key,
            0,
            vec![owner_root.clone()],
            vec![owner_root.clone()],
            1900,
        );
        let r4 = deliver_acl(&mut s, &owner_root, regress);
        assert_eq!(r4.response["ok"], json!(false));
        assert_eq!(r4.response["reason"], json!("acl-epoch-regress"));
    }
}

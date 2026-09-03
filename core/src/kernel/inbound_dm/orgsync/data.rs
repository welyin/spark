//! orgsync-data 入站（从 `orgsync` 拆出，文件长度硬线）：逐条合入 + B3 键域
//! 白名单 + acl/org:meta/org:member/声明四个分流分支 + 删除日志回执。
//! （阶段四A P1：新增 org:member per-member 记录分支与 whole 合入就地投影。）

use serde_json::Value;

use super::acl::apply_acl_record_verified;
use super::super::{
    InboundContext, InboundDmResult, OrgsyncOut, Result, done, fail_response, ok_response,
};
use crate::org::{OrganizationRecord, OrganizationService};
use crate::storage::StorageBackend;

// pub(crate) 而非 pub(super)：mod.rs 以 `pub(super) use` 再导出给
// inbound_dm 直调（crate 内集成测试亦直调）——再导出的可见性要求
// 原项在目标层级可见，pub(super) 会被 E0364 拒。
pub(crate) fn handle_orgsync_data<S: StorageBackend>(
    storage: &mut S,
    ctx: &InboundContext<'_>,
    from: &str,
    body: &Value,
) -> Result<InboundDmResult> {
    let Some((org_id, col_full, records)) = crate::sync::orgsync::parse_orgsync_data(body) else {
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
    // 集合）。F2-P1：`org:coll:{orgId}:` 已并入 org:structure 键域
    // （builtin.rs），org:structure 流量经 data_prefixes 自动放行全部声明；
    // 此处的精确 decl 前缀放行保留兼容（旧端可能仍内联携带本集合声明）。
    let data_prefixes = crate::sync::orgsync::collection_data_prefixes(&org_id, name, version);
    let coll_decl_prefix = format!("org:coll:{org_id}:{name}@v{version}");
    // O4：授权名单（org:acl:）为 all-members 系统数据，随本集合组织流量同步。
    let coll_acl_prefix = crate::sync::orgsync::acl_key(&org_id, name, version);
    for record_item in &records {
        let valid = data_prefixes.iter().any(|p| record_item.key.starts_with(p))
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
    // F3 残余 §7.1：acl / org:meta（成员表 accessKey 段）合入后重评估
    // orgkey-deliver 暂存产出的 unbox 指令（随本结果带出，host 解包落库）。
    let mut stash_unboxes: Vec<super::super::orgkey::OrgkeyUnbox> = Vec::new();
    let mut events = Vec::new();
    // F2-P3 同批顺序（评审修复）：创世 acl 的锚 = 本地收敛声明的 declaredBy
    // ——同批到达时必须先合入声明（及 org:meta/数据/墓碑）再验 acl。采集侧按
    // 前缀序 org:acl: < org:coll: 排列（builtin.rs 键域序），同批 acl 抢跑
    // 声明会创世锚失败整批拒收、声明永不落地、下轮同序同败（死锁）。两趟
    // 稳定排序：acl 记录一律最后处理（max_dseq 是 max 聚合，与顺序无关）。
    let mut ordered: Vec<&crate::sync::orgsync::OrgsyncRecord> = Vec::with_capacity(records.len());
    ordered.extend(
        records
            .iter()
            .filter(|r| crate::sync::orgsync::parse_acl_key(&r.key).is_none()),
    );
    ordered.extend(
        records
            .iter()
            .filter(|r| crate::sync::orgsync::parse_acl_key(&r.key).is_some()),
    );
    for record_item in ordered {
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
            // batch1 §2（f123 建议 2）：acl 验签读**最新**成员表——同批
            // org:meta（携带 signer accessKey）已先合入（两趟排序 acl 最后），
            // 函数入口快照不含它；重读见到的是过了全部既有闸（复制组 + 白名单
            // + F1 合并 accessKey 写一次守卫）的快照。
            let fresh_record = match OrganizationService::get_record(storage, &a_org_id) {
                Ok(Some(r)) => r,
                _ => record.clone(), // 记录异常缺失/损坏 → 回退入口快照（不会更糟）
            };
            if let Some(reason) = apply_acl_record_verified(
                storage,
                &fresh_record,
                from,
                &a_org_id,
                &a_name,
                &a_version,
                &record_item.value,
                &record_item.meta,
                ctx.now_ms,
            )? {
                log::info!(
                    "[ORGSYNC] acl rejected | org={org_id} col={col_full} from={} reason={}",
                    &from[..std::cmp::min(16, from.len())],
                    reason
                );
                return done(fail_response(&reason), Vec::new());
            }
            // F3 残余 §7.1：acl 合入（含本地胜出保留——acl 可能此前已更新）
            // 后重评估 orgkey-deliver 暂存
            stash_unboxes.extend(super::super::orgkey::reevaluate_orgkey_stash(
                storage, ctx, &org_id,
            )?);
            continue;
        }

        // F1：org:meta（org:structure whole 记录）并发合入走成员级结构化合并
        // （merge_org_meta_record），避免 whole-record LWW 丢更新（并发发布的
        // accessKey 被整份覆盖抹掉，O2 联调实测）。
        if record_item.key == format!("org:meta:{org_id}")
            && !crate::sync::is_tombstone(&record_item.meta)
        {
            if apply_org_meta_record_merged(
                storage,
                &record_item.key,
                &record_item.value,
                &record_item.meta,
                from,
            )? {
                log::info!(
                    "[ORGSYNC] data applied | org={org_id} col={col_full} key={}",
                    record_item.key
                );
                // F3 残余 §7.1：org:meta（成员表 accessKey 段）合入后重评估暂存
                stash_unboxes.extend(super::super::orgkey::reevaluate_orgkey_stash(
                    storage, ctx, &org_id,
                )?);
                // F4 第二层（batch3 §1.2 成员表对账兜底）：invitee 已在成员表
                // ⟹ 必已接受——对应 outbound pending 邀请原地标 accepted +
                // 管理面投影同步 + OrgInviteUpdated 事件。重读合并后的最新记录。
                let Ok(Some(fresh)) = OrganizationService::get_record(storage, &org_id) else {
                    continue;
                };
                for inv in crate::org::service::reconcile_outbound_invites_with_members(
                    storage, &fresh, ctx.now_ms, ctx.node_id, ctx.my_root_id,
                )? {
                    events.push(crate::p2p::P2pEvent::OrgInviteUpdated(
                        serde_json::to_value(&inv)?,
                    ));
                }
                // 阶段四A P1 混跑兼容（设计 §6）：旧端只写 whole org:meta——
                // whole 合入后就地投影成员条目（远端语义，不 bump 本机）。
                let whole_meta = crate::sync::get_personal_meta(storage, &record_item.key)?
                    .unwrap_or_default();
                crate::org::service::project_member_entries_from_whole(
                    storage,
                    &org_id,
                    &fresh.members,
                    &whole_meta,
                )?;
            }
            continue;
        }

        // 阶段四A P1：org:member:{orgId}:{rootId} per-member 记录合入——
        // Concurrent 走成员级结构化合并（merge_member_record：字段组按条目
        // 秩、accessKey 写一次守卫下沉于此、nodeInfo/extra 并集）；Remote 整值
        // 覆盖、Local/Equal 不写（lww-record 既有规则）。墓碑不落本分支——
        // 成员移除 = 成员记录墓碑，走下方通用 LWW + org 域 dlog 接力路径。
        if record_item
            .key
            .starts_with(crate::org::types::ORG_MEMBER_PREFIX)
            && !crate::sync::is_tombstone(&record_item.meta)
        {
            if apply_org_member_record_merged(
                storage,
                &record_item.key,
                &record_item.value,
                &record_item.meta,
            )? {
                log::info!(
                    "[ORGSYNC] data applied | org={org_id} col={col_full} key={}",
                    record_item.key
                );
                // F4 第二层挂点补齐（评审发现，P2 通道迁移后）：P2 join 只产生
                // org:member 条目流量（成员自写条目），邀请人侧 org:meta 无合入
                // 事件——对账若只挂 org:meta 分支在 P2 主链路永不触发（回执
                // 丢失时 outbound 永久 pending）。条目入站到达 =  invitee 已
                // 接受（预录条目是本机双写产物，不会经入站到达）→ 同款对账。
                let Ok(Some(fresh)) = OrganizationService::get_record(storage, &org_id) else {
                    continue;
                };
                for inv in crate::org::service::reconcile_outbound_invites_with_members(
                    storage, &fresh, ctx.now_ms, ctx.node_id, ctx.my_root_id,
                )? {
                    events.push(crate::p2p::P2pEvent::OrgInviteUpdated(
                        serde_json::to_value(&inv)?,
                    ));
                }
            }
            continue;
        }

        // F2-P2：集合声明记录（org:coll:）合入走确定性收敛（同策略按
        // (declaredAt, declaredBy) 小者胜、败方原位替换不 bump 本机分量；
        // 策略冲突保留先见者），替代整值 LWW/「保留先见者」。
        if record_item.key.starts_with("org:coll:") && !crate::sync::is_tombstone(&record_item.meta)
        {
            crate::plugindata::apply_org_decl_convergent(
                storage,
                &record_item.key,
                &record_item.value,
                &record_item.meta,
            )?;
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
                    storage,
                    &org_id,
                    name,
                    version,
                    &record_item.key,
                )?;
                let mut ops = dlog_ops;
                // F5：存量组织键（内建 all-members 集合键域 org:meta/ct:org，
                // `legacy_org_key_scope` 判定；F7 起 org:inv 已退出）墓碑
                // **同时补登个人域 dlog**——与本地 tombstone_local 双写对称
                // （orgsync 成员间反熵走 org dlog，pdsync 自设备同步照旧走个人
                // dlog）。orgd:/org:coll: 键维持 org-dlog-only（O2a B4 语义不变）。
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
                storage.batch(ops).map_err(crate::sync::SyncError::from)?;
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
            storage,
            &org_id,
            name,
            version,
            from,
            ctx.remote_peer_id,
            dseq,
        );
        // 立即回发 need 携带 dlogAck
        let local_vv =
            crate::sync::orgsync::collect_org_collection_vv(storage, &org_id, name, version)
                .unwrap_or_default();
        let seen = crate::sync::orgsync::org_dlog_get_seen(
            storage,
            &org_id,
            name,
            version,
            from,
            ctx.remote_peer_id,
        )
        .unwrap_or(0);
        out.push(OrgsyncOut::Need {
            to_root_id: from.to_string(),
            body: crate::sync::orgsync::build_orgsync_need(&org_id, &col_full, &local_vv, seen),
        });
    }

    // 卫生批项2：同 hello/need——验签成功 + 合入完成的 data 交换同样反哺
    crate::org::sync_state::note_orgsync_activity(storage, &record, from, ctx.now_ms);

    Ok(InboundDmResult {
        response: ok_response(),
        events,
        auto_accept: None,
        self_profile: None,
        device_sync_reply: None,
        device_notice_broadcast: false,
        profile_sync_reply: None,
        pdsync_out: Vec::new(),
        orgsync_out: out,
        profile_applied: false,
        orgkey_unbox: stash_unboxes,
        feed_blob_out: None,
    })
}

/// F1：org:meta（org:structure whole 记录）合入分流——Remote 整值覆盖
/// （快路径，远端严格领先无信息可丢）、Local/Equal 不写、**Concurrent 走
/// 成员级结构化合并**（[`crate::org::meta_merge::merge_org_meta_record`]）。
///
/// 合并落库 = batch[ put 合并值, put pmeta{ vv: 合并 vv（支配两个输入，
/// 值/vv 不脱节）, ts: max, nodeId: None } ]——合并是合入语义而非本地写：
/// 不 bump 本机分量、不写序号键（防回声，对齐 sync-storm-fix §5）。
/// 解析失败（损坏记录）回退整值 LWW，不阻塞同步。返回是否实际落库。
fn apply_org_meta_record_merged<S: StorageBackend>(
    storage: &mut S,
    key: &str,
    value: &Value,
    remote_meta: &crate::sync::meta::DocMeta,
    from: &str,
) -> Result<bool> {
    let local_meta = crate::sync::get_personal_meta(storage, key)?;
    let cmp = crate::sync::compare_version_vectors(
        local_meta.as_ref().map(|m| &m.vv),
        Some(&remote_meta.vv),
    );
    let fallback_lww = |storage: &mut S| -> Result<bool> {
        // F8 §2.3 观测：Remote 整值覆盖导致 members 减少 → WARN（合法踢出
        // 同触发但低频且可辨识——管理员显式操作；作为写侧脱节（RMW
        // vv/内容脱节覆盖丢成员，org-meta-rmw-fix）回归的线上监控信号）
        let local_members: Vec<String> = storage
            .get(key)?
            .and_then(|raw| serde_json::from_str::<OrganizationRecord>(&raw).ok())
            .map(|r| r.members.iter().map(|m| m.root_id.clone()).collect())
            .unwrap_or_default();
        let value_str = serde_json::to_string(value)?;
        let r = crate::sync::apply_personal_remote_no_dlog(storage, key, &value_str, remote_meta)?;
        if r.did_apply() && !local_members.is_empty() {
            let incoming_members: Vec<String> =
                serde_json::from_value::<OrganizationRecord>(value.clone())
                    .map(|r| r.members.iter().map(|m| m.root_id.clone()).collect())
                    .unwrap_or_default();
            let missing: Vec<String> = local_members
                .iter()
                .filter(|m| !incoming_members.contains(m))
                .map(|m| m[..std::cmp::min(16, m.len())].to_string())
                .collect();
            if !missing.is_empty() {
                log::warn!(
                    "[ORGSYNC] org:meta remote overwrite shrinks members | from={} missing={:?} —— 合法踢出或写侧脱节（F8 监控信号）",
                    &from[..std::cmp::min(16, from.len())],
                    missing
                );
            }
        }
        Ok(r.did_apply())
    };
    if !matches!(cmp, crate::sync::meta::CompareResult::Concurrent) {
        return fallback_lww(storage);
    }
    // Concurrent：结构化合并
    let parsed = storage
        .get(key)?
        .and_then(|raw| serde_json::from_str::<OrganizationRecord>(&raw).ok())
        .zip(serde_json::from_value::<OrganizationRecord>(value.clone()).ok());
    let Some((local_rec, remote_rec)) = parsed else {
        return fallback_lww(storage);
    };
    let merged = crate::org::meta_merge::merge_org_meta_record(&local_rec, &remote_rec);
    let local_meta = local_meta.unwrap_or_default();
    let merged_meta = crate::sync::meta::DocMeta {
        vv: crate::sync::merge_version_vectors(Some(&local_meta.vv), Some(&remote_meta.vv)),
        ts: local_meta.ts.max(remote_meta.ts),
        node_id: None,
        tombstone: None,
    };
    storage
        .batch(vec![
            crate::storage::BatchOperation::put(key, serde_json::to_string(&merged)?),
            crate::storage::BatchOperation::put(
                crate::sync::personal_meta_key(key),
                serde_json::to_string(&merged_meta)?,
            ),
        ])
        .map_err(crate::sync::SyncError::from)?;
    Ok(true)
}

/// 阶段四A P1：org:member per-member 记录合入分流——Remote 整值覆盖（快
/// 路径）、Local/Equal 不写、**Concurrent 走成员级结构化合并**
/// （[`crate::org::meta_merge::merge_member_record`]：字段组按条目秩选取、
/// accessKey 写一次守卫下沉于此、nodeInfo/extra 并集）。
///
/// 条目秩 = `(pmeta.ts, canonical 字节)`（成员条目自身无 updatedAt 字段）。
/// 合并落库 = batch[ put 合并值, put pmeta{ vv: 合并 vv（支配两个输入）,
/// ts: max, nodeId: None } ]——合入语义而非本地写：不 bump 本机分量、不写
/// 序号键（防回声，与 [`apply_org_meta_record_merged`] 同口径）。解析失败
/// （损坏记录）回退整值 LWW，不阻塞同步。返回是否实际落库。
fn apply_org_member_record_merged<S: StorageBackend>(
    storage: &mut S,
    key: &str,
    value: &Value,
    remote_meta: &crate::sync::meta::DocMeta,
) -> Result<bool> {
    let local_meta = crate::sync::get_personal_meta(storage, key)?;
    let cmp = crate::sync::compare_version_vectors(
        local_meta.as_ref().map(|m| &m.vv),
        Some(&remote_meta.vv),
    );
    let value_str = serde_json::to_string(value)?;
    let fallback_lww = |storage: &mut S| -> Result<bool> {
        let r =
            crate::sync::apply_personal_remote_no_dlog(storage, key, &value_str, remote_meta)?;
        Ok(r.did_apply())
    };
    if !matches!(cmp, crate::sync::meta::CompareResult::Concurrent) {
        return fallback_lww(storage);
    }
    // Concurrent：成员级结构化合并
    let parsed = storage
        .get(key)?
        .and_then(|raw| {
            serde_json::from_str::<crate::org::types::OrganizationMember>(&raw).ok()
        })
        .zip(serde_json::from_value::<crate::org::types::OrganizationMember>(value.clone()).ok());
    let Some((local_m, remote_m)) = parsed else {
        return fallback_lww(storage);
    };
    let local_meta = local_meta.unwrap_or_default();
    let local_rank = (
        local_meta.ts,
        serde_json::to_string(&local_m).unwrap_or_default(),
    );
    let remote_rank = (remote_meta.ts, value_str);
    let merged = crate::org::meta_merge::merge_member_record(
        &local_m,
        &remote_m,
        &local_rank,
        &remote_rank,
    );
    let merged_meta = crate::sync::meta::DocMeta {
        vv: crate::sync::merge_version_vectors(Some(&local_meta.vv), Some(&remote_meta.vv)),
        ts: local_meta.ts.max(remote_meta.ts),
        node_id: None,
        tombstone: None,
    };
    storage
        .batch(vec![
            crate::storage::BatchOperation::put(key, serde_json::to_string(&merged)?),
            crate::storage::BatchOperation::put(
                crate::sync::personal_meta_key(key),
                serde_json::to_string(&merged_meta)?,
            ),
        ])
        .map_err(crate::sync::SyncError::from)?;
    Ok(true)
}

/// F5 幂等判定：个人域 dlog（`dlog:entry:{seq}` → record_key）是否已含该
/// 记录键——远端存量组织键墓碑补登个人 dlog 前检查，防同一删除双写。
fn personal_dlog_has_entry<S: StorageBackend>(storage: &S, record_key: &str) -> Result<bool> {
    let entries =
        crate::sync::dlog::entries_after(storage, 0).map_err(crate::sync::SyncError::from)?;
    Ok(entries.iter().any(|(_, k)| k == record_key))
}


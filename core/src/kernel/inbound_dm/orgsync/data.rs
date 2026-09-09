//! orgsync-data 入站（从 `orgsync` 拆出，文件长度硬线）：逐条合入 + B3 键域
//! 白名单 + org:meta/org:member/声明分流分支 + 删除日志回执。
//! （阶段四A P1：新增 org:member per-member 记录分支与 whole 合入就地投影；
//! C7：原 acl 分流分支随 encrypted 轴退役移除。）

use serde_json::Value;

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
    for record_item in &records {
        let valid = data_prefixes.iter().any(|p| record_item.key.starts_with(p))
            || record_item.key.starts_with(&coll_decl_prefix);
        if !valid {
            // H4：拒绝日志只打 key 前缀截断（越界 key 可能是敏感键，全量打印
            // 泄漏明文）。
            let key_frag = &record_item.key[..std::cmp::min(16, record_item.key.len())];
            log::info!(
                "[ORGSYNC] data rejected: key out of collection prefix | org={org_id} col={col_full} key={key_frag}"
            );
            return done(fail_response("key-out-of-collection"), Vec::new());
        }
    }

    let mut max_dseq: Option<u64> = None;
    let mut events = Vec::new();
    for record_item in &records {
        if let Some(dseq) = record_item.dseq {
            max_dseq = Some(max_dseq.map_or(dseq, |m: u64| m.max(dseq)));
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
                // F4 第二层（batch3 §1.2 成员表对账兜底；裁决 §10.2 收紧触发
                // 为「invitee 自写分量到达」）：合入记录 vv 分量 ∩ invitee 已知
                // 端点非空才置 accepted（预录/中继不误标）。重读合并后的最新记录。
                let Ok(Some(fresh)) = OrganizationService::get_record(storage, &org_id) else {
                    continue;
                };
                for inv in crate::org::service::reconcile_outbound_invites_with_members(
                    storage,
                    &fresh,
                    &record_item.meta.vv,
                    ctx.now_ms,
                    ctx.node_id,
                    ctx.my_root_id,
                )? {
                    events.push(crate::p2p::P2pEvent::OrgInviteUpdated(
                        serde_json::to_value(&inv)?,
                    ));
                }
                // 阶段四A P1 混跑兼容（设计 §6）：旧端只写 whole org:meta——
                // whole 合入后就地投影成员条目（远端语义，不 bump 本机）。
                let whole_meta =
                    crate::sync::get_personal_meta(storage, &record_item.key)?.unwrap_or_default();
                crate::org::service::project_member_entries_from_whole(
                    storage,
                    &org_id,
                    &fresh.members,
                    &whole_meta,
                )?;
                // C1 合入侧执法（org-genesis §3.2/§3.3）：共同体硬规则
                // （成员种类 + 成环）对合入后名册逐条校验，违规 kind=org 条目
                // 剔除——值改写不 bump 本机分量、pmeta 不动（合入语义，与
                // 结构化合并同口径）。A16 同款执法：验绑失败的 accessKey 剥除。
                let mut enforced = fresh;
                let roster_fixed = OrganizationService::enforce_incoming_roster(storage, &mut enforced)? > 0;
                let aks_stripped = crate::org::access_key::strip_unverified_access_keys(&mut enforced) > 0;
                if roster_fixed || aks_stripped {
                    storage
                        .put(&record_item.key, &serde_json::to_string(&enforced)?)
                        .map_err(crate::sync::SyncError::from)?;
                }
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
                // F4 第二层挂点（P2 主链路：join 只产生 org:member 条目流量，
                // 邀请人侧 org:meta 无合入事件）；裁决 §10.2 收紧触发为
                // 「invitee 自写分量到达」（vv 分量 ∩ invitee 已知端点非空
                // 才置 accepted——预录条目是管理员双写产物，只含管理员分量，
                // 不误标）。
                let Ok(Some(fresh)) = OrganizationService::get_record(storage, &org_id) else {
                    continue;
                };
                for inv in crate::org::service::reconcile_outbound_invites_with_members(
                    storage,
                    &fresh,
                    &record_item.meta.vv,
                    ctx.now_ms,
                    ctx.node_id,
                    ctx.my_root_id,
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

        // C1：创世策略记录（org:genesis:）写一次不可变——三重校验（orgId
        // 自认证复算 + 根签名验签 + orgAddress 互绑复算）过后才准落库；
        // 本地已有且不一致保留本地（LWW 不适用于自认证锚），校验失败拒收。
        if record_item.key.starts_with("org:genesis:")
            && !crate::sync::is_tombstone(&record_item.meta)
        {
            if crate::org::service::admit_incoming_genesis(
                storage,
                &record_item.key,
                &record_item.value,
            )? {
                let value_str = serde_json::to_string(&record_item.value)?;
                crate::sync::apply_personal_remote_no_dlog(
                    storage,
                    &record_item.key,
                    &value_str,
                    &record_item.meta,
                )?;
            } else {
                log::info!(
                    "[ORGSYNC] genesis record rejected | org={org_id} key={}",
                    record_item.key
                );
            }
            continue;
        }

        // credential §4：验证人信任声明（org:verifiers:）合入走
        // merge_trust_decl（结构 + sigSet 绑定 + OrgSigSet 五步链 + 逐版
        // LWW）——裁决 Accept 才落地，其余（KeepCurrent/Rejected）不写。
        if record_item.key.starts_with("org:verifiers:")
            && !crate::sync::is_tombstone(&record_item.meta)
        {
            match crate::org::service::adjudicate_incoming_trust_decl(
                storage,
                &org_id,
                &record_item.value,
            )? {
                crate::org::service::TrustDeclMerge::Accept => {
                    let value_str = serde_json::to_string(&record_item.value)?;
                    crate::sync::apply_personal_remote_no_dlog(
                        storage,
                        &record_item.key,
                        &value_str,
                        &record_item.meta,
                    )?;
                }
                crate::org::service::TrustDeclMerge::KeepCurrent => {}
                crate::org::service::TrustDeclMerge::Rejected => {
                    log::info!(
                        "[ORGSYNC] trustDecl rejected | org={org_id} key={}",
                        record_item.key
                    );
                }
            }
            continue;
        }

        // policy §2：发布策略文档（org:policydoc:）合入走
        // adjudicate_incoming_policy_doc（结构 + sigSet subject 绑定 +
        // OrgSigSet 五步链 + updatedAt LWW）——裁决 Accept 才落地。
        if record_item.key.starts_with(crate::org::service::POLICY_DOC_PREFIX)
            && !crate::sync::is_tombstone(&record_item.meta)
        {
            match crate::org::service::adjudicate_incoming_policy_doc(
                storage,
                &org_id,
                &record_item.value,
            )? {
                crate::org::service::PolicyDocMerge::Accept => {
                    let value_str = serde_json::to_string(&record_item.value)?;
                    crate::sync::apply_personal_remote_no_dlog(
                        storage,
                        &record_item.key,
                        &value_str,
                        &record_item.meta,
                    )?;
                }
                crate::org::service::PolicyDocMerge::KeepCurrent => {}
                crate::org::service::PolicyDocMerge::Rejected => {
                    log::info!(
                        "[ORGSYNC] policyDoc rejected | org={org_id} key={}",
                        record_item.key
                    );
                }
            }
            continue;
        }

        // A15 名册开放声明（org:disclosure:）合入走
        // adjudicate_incoming_disclosure（结构 + sigSet subject 绑定 +
        // OrgSigSet 五步链 + version LWW）——裁决 Accept 才落地；发布即公示
        // （本键域随 org:structure 全员流动），生效由记录 effectiveAt 门控。
        if record_item
            .key
            .starts_with(crate::policy::DISCLOSURE_PREFIX)
            && !crate::sync::is_tombstone(&record_item.meta)
        {
            let target_domain = record_item
                .key
                .strip_prefix(&format!("{}{org_id}:", crate::policy::DISCLOSURE_PREFIX));
            let merge = match target_domain {
                Some(target) => crate::org::service::adjudicate_incoming_disclosure(
                    storage,
                    &org_id,
                    target,
                    &record_item.value,
                )?,
                // 键形错位（他组织的 disclosure 键混入本组织流量）→ 拒收
                None => crate::org::service::DisclosureMerge::Rejected,
            };
            match merge {
                crate::org::service::DisclosureMerge::Accept => {
                    let value_str = serde_json::to_string(&record_item.value)?;
                    crate::sync::apply_personal_remote_no_dlog(
                        storage,
                        &record_item.key,
                        &value_str,
                        &record_item.meta,
                    )?;
                }
                crate::org::service::DisclosureMerge::KeepCurrent => {}
                crate::org::service::DisclosureMerge::Rejected => {
                    log::info!(
                        "[ORGSYNC] disclosure rejected | org={org_id} key={}",
                        record_item.key
                    );
                }
            }
            continue;
        }

        // A17 准入策略声明（org:accept:）合入走
        // adjudicate_incoming_accept_policy（结构 + sigSet subject 绑定 +
        // OrgSigSet 五步链 + version LWW）——裁决 Accept 才落地；发布即公示
        // （本键域随 org:structure 全员流动），生效由记录 effectiveAt 门控。
        if record_item
            .key
            .starts_with(crate::policy::ACCEPT_POLICY_PREFIX)
            && !crate::sync::is_tombstone(&record_item.meta)
        {
            // 键形 `org:accept:{orgId}` 单分量：键 orgId 与本组织不符（他组织
            // 键混入本组织流量）→ 拒收
            let merge = match record_item
                .key
                .strip_prefix(crate::policy::ACCEPT_POLICY_PREFIX)
            {
                Some(key_org) if key_org == org_id => {
                    crate::org::service::adjudicate_incoming_accept_policy(
                        storage,
                        &org_id,
                        &record_item.value,
                    )?
                }
                _ => crate::org::service::AcceptPolicyMerge::Rejected,
            };
            match merge {
                crate::org::service::AcceptPolicyMerge::Accept => {
                    let value_str = serde_json::to_string(&record_item.value)?;
                    crate::sync::apply_personal_remote_no_dlog(
                        storage,
                        &record_item.key,
                        &value_str,
                        &record_item.meta,
                    )?;
                }
                crate::org::service::AcceptPolicyMerge::KeepCurrent => {}
                crate::org::service::AcceptPolicyMerge::Rejected => {
                    log::info!(
                        "[ORGSYNC] accept policy rejected | org={org_id} key={}",
                        record_item.key
                    );
                }
            }
            continue;
        }

        // 阶段四F §8：存证锚（org:evi:anchor:）LWW 覆盖前的分叉证据保全——
        // 同 nodeId 回退/同 seq 异 hash → 双份签名锚留档（本地 org:evi:fork:
        // 键）+ WARN 告警后仍按 LWW 合入（检测分歧、不做裁决；治理面采纳时
        // 查留档拒绝该节点锚）。同步面不验签（§6 既定口径）。
        if record_item
            .key
            .starts_with(crate::evidence::EVIDENCE_ANCHOR_PREFIX)
            && !crate::sync::is_tombstone(&record_item.meta)
        {
            let local_anchor = storage
                .get(&record_item.key)?
                .and_then(|raw| serde_json::from_str::<crate::evidence::AnchorRecord>(&raw).ok());
            let incoming_anchor =
                serde_json::from_value::<crate::evidence::AnchorRecord>(record_item.value.clone())
                    .ok();
            if let (Some(local), Some(incoming)) = (local_anchor, incoming_anchor)
                && let Some(kind) = crate::evidence::detect_anchor_fork(&local, &incoming)
            {
                let archive_key = crate::evidence::fork_archive_key(
                    &incoming.org_id,
                    &incoming.node_id,
                    ctx.now_ms,
                );
                let archive =
                    crate::evidence::fork_archive_value(&local, &incoming, kind, ctx.now_ms);
                storage
                    .put(&archive_key, &serde_json::to_string(&archive)?)
                    .map_err(crate::sync::SyncError::from)?;
                log::warn!(
                    "[ORGSYNC] evidence anchor fork detected | org={} node={} kind={} —— 双份签名锚已留档 {}",
                    incoming.org_id,
                    incoming.node_id,
                    kind.as_str(),
                    archive_key
                );
            }
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
        affairsync_out: Vec::new(),
        profile_applied: false,
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
    // A16 验绑（入站执法）：远端成员条目携带的 accessKey 必须 `rootPubkey`
    // 锚定名册键且绑定签名有效，否则剥除后合入——不信任 peer 注入的伪造
    // 绑定（membership §4.4「合入侧验绑」）。
    let sanitized;
    let value = {
        let org_id = key
            .strip_prefix(crate::org::types::ORG_MEMBER_PREFIX)
            .and_then(|rest| rest.split(':').next())
            .unwrap_or_default();
        match serde_json::from_value::<crate::org::types::OrganizationMember>(value.clone()) {
            Ok(mut member) if member.access_key.is_some() => {
                let verified = member.access_key.as_ref().is_some_and(|ak| {
                    crate::org::access_key::verify_access_key_binding(org_id, &member.root_id, ak)
                });
                if verified {
                    value
                } else {
                    log::warn!("[ORGSYNC] accessKey 验绑失败已剥除 | key={key}");
                    member.access_key = None;
                    sanitized = serde_json::to_value(&member)?;
                    &sanitized
                }
            }
            _ => value,
        }
    };
    let local_meta = crate::sync::get_personal_meta(storage, key)?;
    let cmp = crate::sync::compare_version_vectors(
        local_meta.as_ref().map(|m| &m.vv),
        Some(&remote_meta.vv),
    );
    let value_str = serde_json::to_string(value)?;
    let fallback_lww = |storage: &mut S| -> Result<bool> {
        let r = crate::sync::apply_personal_remote_no_dlog(storage, key, &value_str, remote_meta)?;
        Ok(r.did_apply())
    };
    if !matches!(cmp, crate::sync::meta::CompareResult::Concurrent) {
        return fallback_lww(storage);
    }
    // Concurrent：成员级结构化合并
    let parsed = storage
        .get(key)?
        .and_then(|raw| serde_json::from_str::<crate::org::types::OrganizationMember>(&raw).ok())
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
    let merged =
        crate::org::meta_merge::merge_member_record(&local_m, &remote_m, &local_rank, &remote_rank);
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

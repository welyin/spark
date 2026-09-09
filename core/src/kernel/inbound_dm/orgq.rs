//! dm 入站编排（orgq 系，O3）：`orgq-req`（成员 → 数据账号的按需查询/写入）
//! 与 `orgq-resp`（数据账号 → 成员的回执）。
//!
//! 从 `inbound_dm` 拆出的子模块（O3，文件长度拆分），共享父模块的
//! [`InboundContext`]/应答助手/[`done`] 等。
//!
//! 验签规则（org-orgsync.md §20.5）：
//! - `orgq-req`：只接受组织成员（公共前置）；
//! - `orgq-resp`：只接受数据账号，且须为对应 req 的应答（requestId 关联）。
//!
//! 限流：orgq 是成员级流量，**不做全豁免**——沿用 dm 按 from 计数模式（与
//! orgsync 反熵三信封的豁免不同，见 `p2p/direct/dm.rs`）。
//!
//! ## filtered 钩子执行（数据账号侧）
//!
//! §20.5 过滤分流：`filtered` 集合查询经插件 `canRead`、写入经 `canWrite` 钩子，
//! 钩子在数据账号侧**插件后台运行时**执行（C7：`encrypted` 轴已退役，org 集合
//! 恒为 filtered）。纯逻辑层无法执行 JS 钩子，故本层按规格
//! 「插件未运行时该集合**只存不服务**」语义 **fail-closed**：回 `denied` 降级。
//! 钩子的真实接线（host 层插件运行时）是 O3 工作项 2 的宿主接线点，本层只保证
//! 「未接线即降级」的安全默认。
//!
//! ## read-gate（读授权门禁，community read-gate §4）
//!
//! 声明带 `readPolicy` 的 org 集合，查询面授权口径按 `kind` 分流：
//! - `members`（缺省）：维持上述成员前置 + filtered 钩子路径（零变化）；
//! - `public`：公开发布——无需成员资格/凭证，不经插件钩子直接服务；
//! - `credential`：凭证校验**替代**插件钩子（read-gate §1「钩子换为凭证校验」）
//!   ——查询方无需加入来源组织，凭 readAuth 段过验证链（`verify_read_auth`，
//!   §4 第 1–4 步 + A15 城门名册回查：持有者当时确为凭证 subjectDomain 成员，
//!   双键兼容，退队即拒零轮换）+ policyRef 存在时按**开放声明**求值（A15
//!   城门口径：`disclosure_allows`，旧 B1 文档求值已随口径一次性切换下线，
//!   membership §五.2）；任一失败 fail-closed → `denied` 空集应答（非授权者
//!   连元数据都不给，§20.5 既有口径）。写路径不受 readPolicy 影响（写权限
//!   仍属来源组织成员 + canWrite 钩子）。

use serde_json::{Value, json};

use super::{
    InboundContext, InboundDmResult, OrgsyncOut, Result, done, fail_response, ok_response,
};
use crate::org::OrganizationService;
use crate::storage::{BatchOperation, StorageBackend};

/// 从 orgq-req 解析集合标识 `{name}@v{version}` → (name, version)。
fn split_collection_full(col_full: &str) -> Option<(&str, &str)> {
    let at = col_full.rfind("@v")?;
    Some((&col_full[..at], &col_full[at + 2..]))
}

/// orgq filtered 集合的权限钩子抽象（O3 工作项 2 的纯逻辑契约）。
///
/// 数据账号侧收到 filtered 集合 orgq-req 时，由**宿主层**提供本 trait 的真实
/// 实现（在插件后台运行时 QuickJS 中执行插件注册的 `canRead`/`canWrite` 钩子，
/// 见 `plugin/host_env.rs` 与 `kernel/host/dm_handler.rs` 接线）。纯逻辑层不
/// 触碰 JS 引擎——本 trait 是把「插件运行 + 钩子裁决」外置为可注入协议点的
/// 机制：`handle_inbound_dm` 默认传 `None`（fail-closed：插件未运行即只存不
/// 服务降级 denied，与规格 §8 O3 工作项 2 一致）；宿主/测试经
/// `handle_inbound_dm_with_orgq_hooks` 注入真实或假实现。
///
/// ## 契约（F2）
///
/// - **钩子内禁止重入写类宿主能力**：钩子在数据账号侧 `io_lock` 内执行
///   （`dm_handler.rs` 入站落库整体持 `io_lock`），若钩子内再调宿主写能力
///   （docs.put/data.save/declareCollection 等）会递归取同一把锁 → 死锁或
///   长阻塞。实现方必须确保钩子只做**只读裁决**（读插件内存态/插件自有读
///   能力），不触发任何写库宿主调用；
/// - **单请求总预算**：一个 orgq-req 内多条记录逐条过钩子，累计执行时间须
///   有界（单钩子超时由宿主引擎熔断；实现方应设单请求总预算，如累计 10s
///   封顶），超预算按 fail-closed 拒绝，不得让恶意/故障插件长期阻塞 io_lock。
pub trait OrgqPermHook {
    /// 该集合是否注册了可服务的钩子（插件后台运行中 + 已注册 onRead/onWriteFilter）。
    /// `kind` ∈ {"read","write"}——读/写能力独立判定（只注册 canRead 的集合
    /// 查询可服务、写入仍 fail-closed 拒绝）。
    fn has_runtime(&self, collection: &str, kind: &str) -> bool;
    /// `canRead(member, collection, key)` 裁决：true = 放行该 key 给 member。
    fn can_read(&self, member: &str, collection: &str, key: &str) -> bool;
    /// `canWrite(member, collection, key, value)` 裁决：true = 放行该写入。
    fn can_write(
        &self,
        member: &str,
        collection: &str,
        key: &str,
        value: &serde_json::Value,
    ) -> bool;
}

/// orgq-req 入站（**数据账号侧**）：受理成员的按需查询 / 写入。
///
/// 验签：
/// 1. 公共前置：`from` ∈ org:meta 成员表；
/// 2. 集合须为**已声明的 org scope 集合**（`org:coll:{orgId}:{name}@v{version}`）；
/// 3. filtered 钩子（§20.5）：钩子在数据账号侧插件后台运行时执行
///    （`canRead`/`canWrite`）。纯逻辑层无法执行 JS 钩子，按规格「插件未运行时
///    该集合**只存不服务**」语义 **fail-closed**——本层不能确认插件运行 +
///    钩子放行，故回 `denied` 降级。宿主把钩子执行接线到插件后台运行时后，
///    此处即真实放行点（O3 工作项 2 的宿主接线，本层只保证「未接线即降级」
///    的安全默认）。
/// 受理一条 orgq 写入记录落库（数据账号侧，§20.5）。`base` 为集合数据键前缀。
///
/// - `value` 非 null → `put_personal` 版本化写入（现状，随复制组扩散）；
/// - `value` 为 null → **删除语义**（Z1）：走 org 域本地墓碑原语
///   [`crate::sync::orgsync::org_tombstone_local`]——per-node 序号 bump +
///   墓碑 pmeta + org 域 dlog + 本体删除同一 batch 原子提交（orgd: 键只登
///   org 域 dlog，不污染个人域）。返回 `true` = 落库成功。
fn apply_orgq_write<S: StorageBackend>(
    storage: &mut S,
    ctx: &InboundContext<'_>,
    org_id: &str,
    name: &str,
    version: &str,
    base: &str,
    rel: &str,
    value: &Value,
    now: i64,
) -> bool {
    let key = format!("{base}{rel}");
    if !value.is_null() {
        let _ = crate::sync::put_personal(
            storage,
            ctx.node_id,
            &key,
            &serde_json::to_string(value).unwrap_or_default(),
            now,
        );
        return true;
    }
    // 删除受理：per-key bump 不推进序号分配器会导致后续新写与墓碑序号碰撞
    // （折叠失明，org-vv-fix §1.2）——统一走 org_tombstone_local，与个人域
    // delete_personal 同口径。远端 orgsync 合入墓碑同样补登 org 域 dlog
    // （handle_orgsync_data），保证删除在复制组内接力传播。
    match crate::sync::orgsync::org_tombstone_local(
        storage,
        ctx.node_id,
        org_id,
        name,
        version,
        &key,
        now,
    ) {
        Ok(_) => {
            log::info!("[ORGQ] write tombstone | org={org_id} key={key}");
            true
        }
        Err(_) => false,
    }
}

/// denied 空集查询应答（非授权者连元数据都不给，§20.5 既有口径）：
/// 插件未运行降级 / read-gate 门禁拒绝共用同一应答形态。
fn push_denied_query_resp(
    out: &mut Vec<OrgsyncOut>,
    org_id: &str,
    col_full: &str,
    request_id: &str,
    now: i64,
    from: &str,
) {
    let resp = crate::sync::orgsync::build_orgq_query_resp(
        org_id, col_full, request_id, &[], true, now, true,
    );
    out.push(OrgsyncOut::OrgqResp {
        to_root_id: from.to_string(),
        body: resp,
    });
}

/// 放行采集 + 按 dm 信封体积分批应答（Z6：complete 仅末批 true）。
/// `allow` 逐 key 裁决（filtered canRead 钩子 / 门禁通过后的恒真）。
#[allow(clippy::too_many_arguments)]
fn serve_query_page<S: StorageBackend>(
    storage: &S,
    out: &mut Vec<OrgsyncOut>,
    org_id: &str,
    col_full: &str,
    name: &str,
    version: &str,
    prefix: Option<&str>,
    limit: usize,
    cursor: Option<&str>,
    allow: &dyn Fn(&str) -> bool,
    request_id: &str,
    now: i64,
    from: &str,
) {
    // 放行：按 limit/cursor 字典序续扫驻留记录。
    let (records, has_more) = crate::sync::orgsync::collect_orgq_records_page(
        storage, org_id, name, version, prefix, limit, cursor, allow,
    );
    let page_complete = !has_more;
    let batches =
        crate::sync::orgsync::split_orgq_resp_batches(records, crate::sync::orgsync::ORGSYNC_BATCH_BYTES);
    let last = batches.len();
    for (i, batch) in batches.into_iter().enumerate() {
        let batch_complete = i == last - 1 && page_complete;
        let resp = crate::sync::orgsync::build_orgq_query_resp(
            org_id,
            col_full,
            request_id,
            &batch,
            batch_complete,
            now,
            false,
        );
        out.push(OrgsyncOut::OrgqResp {
            to_root_id: from.to_string(),
            body: resp,
        });
    }
}

/// read-gate 门禁裁决（read-gate §4 第 1–5 步，fail-closed）：true = 放行。
///
/// 第 1–4 步由 [`crate::credential::verify_read_auth`] 承载（结构/新鲜度 →
/// 逐凭证验证链 → credType/subjectDomain 匹配 readPolicy → **城门名册回查**
/// （A15：当时确为 subjectDomain 成员，退队即拒）→ holderProof 绑定本次
/// 请求）；第 5 步 policyRef 存在时按**开放声明**求值（A15：
/// [`disclosure_allows`]，旧 B1 文档求值已随口径切换下线）。任一环节数据
/// 缺失或校验失败 → false（denied 空集应答由调用方落）。
#[allow(clippy::too_many_arguments)]
fn read_gate_allows<S: StorageBackend>(
    storage: &S,
    owner_org_id: &str,
    read_policy: &crate::plugindata::ReadPolicy,
    read_auth: Option<&crate::credential::ReadAuth>,
    request_id: &str,
    col_full: &str,
    _is_member: bool,
    now: i64,
) -> bool {
    let Some(read_auth) = read_auth else {
        log::info!("[ORGQ] read-gate denied: readAuth missing | col={col_full}");
        return false;
    };
    let policy = crate::credential::CredentialReadPolicy {
        cred_types: read_policy.cred_types.clone(),
        verifier_domain: read_policy.verifier_domain.clone(),
    };
    // 信任声明：verifierDomain 现行版本（org:verifiers: 键域）；缺失/损坏
    // fail-closed。
    let trust_decl = storage
        .get(&crate::credential::trust_decl_key(&read_policy.verifier_domain))
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str::<crate::credential::TrustDecl>(&raw).ok());
    let Some(trust_decl) = trust_decl else {
        log::info!(
            "[ORGQ] read-gate denied: trust decl unavailable | domain={}",
            read_policy.verifier_domain
        );
        return false;
    };
    // 注销证明：cred:rev: 本地快照（分发承载面未定，credential §3「承载面随
    // C10 定」——渠道落地后按 issuer 写入本键域）；缺失 = 数据不可用 →
    // fail-closed（verify_read_auth 内 revocation-unavailable）。
    let revocation_for = |issuer: &str| {
        let raw = storage
            .get(&crate::credential::revocation_snapshot_key(issuer))
            .ok()
            .flatten()?;
        let snap: crate::credential::RevocationSnapshot = serde_json::from_str(&raw).ok()?;
        Some((snap.entries, snap.head))
    };
    // 城门名册回查（A15 membership §4.3）：持有者当时确为凭证 subjectDomain
    // 成员——读该域组织记录查成员表；记录缺失/损坏 → None → fail-closed。
    // 双键兼容（A16 双写过渡）：holder identity 按 rootId 或 org_user_id
    // 命中任一即在册。
    let roster_lookup = |domain: &str, identity: &str| {
        let record = crate::org::OrganizationService::get_record(storage, domain)
            .ok()
            .flatten()?;
        Some(record.find_member_any_key(identity).is_some())
    };
    if let Err(e) = crate::credential::verify_read_auth(
        read_auth,
        request_id,
        col_full,
        &policy,
        &[&trust_decl],
        &revocation_for,
        &roster_lookup,
        now,
    ) {
        log::info!("[ORGQ] read-gate denied: {} | col={col_full}", e.kind());
        return false;
    }
    // 第 5 步（A15 求值口径一次性切换，membership §五.2）：policyRef 存在时
    // 按**开放声明**求值——属主组织对凭证 subjectDomain 的生效 disclosure
    // 记录覆盖本集合才放行；旧 B1 策略文档（名册三档+字段掩码）不再在读取点
    // 求值，未声明即「仅组织」默认档（fail-closed 最保守；存量组织默认全隐，
    // 首个开放声明须经公示延迟）。readPolicy 线形不变。
    if read_policy.policy_ref.is_none() {
        return true;
    }
    disclosure_allows(storage, owner_org_id, read_auth, col_full, now)
}

/// read-gate §4 第 5 步（A15 城门口径）：开放声明求值——任一呈现凭证的
/// subjectDomain 命中属主组织对该域的生效 disclosure（collections 含本集合）
/// → 放行。声明缺失/未生效/未覆盖一律拒绝（fail-closed）。
fn disclosure_allows<S: StorageBackend>(
    storage: &S,
    owner_org_id: &str,
    read_auth: &crate::credential::ReadAuth,
    col_full: &str,
    now: i64,
) -> bool {
    for cred in &read_auth.credentials {
        let key = crate::policy::disclosure_key(owner_org_id, &cred.subject_domain);
        let Some(record) = storage
            .get(&key)
            .ok()
            .flatten()
            .and_then(|raw| serde_json::from_str::<crate::policy::DisclosureRecord>(&raw).ok())
        else {
            continue;
        };
        if crate::policy::validate_disclosure(&record).is_err() {
            continue;
        }
        let view = crate::policy::eval_disclosure(&[&record], &cred.subject_domain, now);
        if view.collections.iter().any(|c| c == col_full) {
            return true;
        }
    }
    log::info!("[ORGQ] read-gate denied: no effective disclosure | col={col_full}");
    false
}

pub(super) fn handle_orgq_req<S: StorageBackend>(
    storage: &mut S,
    ctx: &InboundContext<'_>,
    from: &str,
    body: &Value,
    hook: Option<&dyn OrgqPermHook>,
) -> Result<InboundDmResult> {
    let Some(req) = crate::sync::orgsync::parse_orgq_req(body) else {
        return done(fail_response("invalid-body"), Vec::new());
    };
    // 一次性提取全部自有值（org_id/collection/name/version），随后 match 消费
    // req 不再借用冲突
    let (org_id, col_full, name, version) = match &req {
        crate::sync::orgsync::OrgqReq::Query {
            org_id, collection, ..
        }
        | crate::sync::orgsync::OrgqReq::Write {
            org_id, collection, ..
        } => {
            let Some((name, version)) = split_collection_full(collection) else {
                return done(fail_response("invalid-collection"), Vec::new());
            };
            (
                org_id.clone(),
                collection.clone(),
                name.to_string(),
                version.to_string(),
            )
        }
    };
    // 公共前置：from ∈ 成员表（read-gate 开放种类见下——credential/public 的
    // 查询面向组织外开放，凭证/公开声明即授权，read-gate §5）
    let Ok(Some(record)) = OrganizationService::get_record(storage, &org_id) else {
        return done(fail_response("rejected"), Vec::new());
    };
    let is_member = record.find_member(from).is_some();

    // 集合须为已声明的 org scope 集合（声明先行解析：readPolicy 决定资格口径；
    // 损坏记录按未声明处理，与既有 has_decl 口径一致）
    let decl_key = crate::plugindata::org_decl_key(&org_id, &name, &version);
    let decl = storage.get(&decl_key).ok().flatten().and_then(|raw| {
        serde_json::from_str::<crate::plugindata::CollectionDeclaration>(&raw).ok()
    });
    let gate_kind = decl
        .as_ref()
        .and_then(|d| d.read_policy.as_ref())
        .map(|p| p.kind);
    // 组织外查询仅对 credential/public 门禁的查询开放；写路径恒要求成员
    let query_open = matches!(req, crate::sync::orgsync::OrgqReq::Query { .. })
        && matches!(
            gate_kind,
            Some(
                crate::plugindata::ReadPolicyKind::Credential | crate::plugindata::ReadPolicyKind::Public
            )
        );
    if !is_member && !query_open {
        log::info!("[ORGQ] req rejected: from={from} not in org={org_id}");
        return done(fail_response("rejected"), Vec::new());
    }
    let Some(decl) = decl else {
        return done(fail_response("collection-not-declared"), Vec::new());
    };

    let now = ctx.now_ms;
    let mut out = Vec::new();
    // filtered 钩子可用性：宿主注入钩子且该集合注册了对应读/写钩子（插件运行中）。
    // 读/写能力独立判定（kind），只注册 canRead 的集合写入仍 fail-closed 拒绝。
    // （C7：encrypted 轴退役，org 集合恒为 filtered。）
    let kind = if matches!(req, crate::sync::orgsync::OrgqReq::Query { .. }) {
        "read"
    } else {
        "write"
    };
    let filtered_serving = hook.is_some_and(|h| h.has_runtime(&col_full, kind));
    match req {
        crate::sync::orgsync::OrgqReq::Query {
            request_id,
            prefix,
            limit,
            cursor,
            read_auth,
            ..
        } => {
            // read-gate 分流（read-gate §4/§5）：门禁种类决定查询面授权口径。
            match gate_kind {
                // credential：凭证校验替代插件钩子——门禁通过即全集合放行
                // （不过 canRead）；失败 fail-closed → denied 空集。
                Some(crate::plugindata::ReadPolicyKind::Credential) => {
                    let read_policy = decl
                        .read_policy
                        .as_ref()
                        .expect("gate_kind 隐含 readPolicy 存在");
                    if read_gate_allows(
                        storage,
                        &org_id,
                        read_policy,
                        read_auth.as_ref(),
                        &request_id,
                        &col_full,
                        is_member,
                        now,
                    ) {
                        serve_query_page(
                            storage,
                            &mut out,
                            &org_id,
                            &col_full,
                            &name,
                            &version,
                            prefix.as_deref(),
                            limit,
                            cursor.as_deref(),
                            &|_| true,
                            &request_id,
                            now,
                            from,
                        );
                    } else {
                        push_denied_query_resp(&mut out, &org_id, &col_full, &request_id, now, from);
                    }
                }
                // public：公开发布——无需凭证、不经插件钩子直接服务。
                Some(crate::plugindata::ReadPolicyKind::Public) => {
                    serve_query_page(
                        storage,
                        &mut out,
                        &org_id,
                        &col_full,
                        &name,
                        &version,
                        prefix.as_deref(),
                        limit,
                        cursor.as_deref(),
                        &|_| true,
                        &request_id,
                        now,
                        from,
                    );
                }
                // members（缺省现状）：filtered 无运行时 fail-closed → denied
                // 空集；钩子运行中 → 放行采集（逐条过 canRead 过滤，非读者连
                // 元数据都不给）。
                _ => {
                    if !filtered_serving {
                        push_denied_query_resp(
                            &mut out, &org_id, &col_full, &request_id, now, from,
                        );
                    } else {
                        let hook = hook.expect("filtered_serving 隐含 hook 存在");
                        let allow = |rel: &str| hook.can_read(from, &col_full, rel);
                        serve_query_page(
                            storage,
                            &mut out,
                            &org_id,
                            &col_full,
                            &name,
                            &version,
                            prefix.as_deref(),
                            limit,
                            cursor.as_deref(),
                            &allow,
                            &request_id,
                            now,
                            from,
                        );
                    }
                }
            }
        }
        crate::sync::orgsync::OrgqReq::Write {
            request_id,
            records,
            ..
        } => {
            // filtered 无运行时 fail-closed denied（杜绝未授权写库）；
            // filtered + 钩子运行中 → 逐条 canWrite 裁决。
            let mut accepted = 0usize;
            let mut rejected = 0usize;
            // filtered 无运行时 → denied（整批拒绝）。
            let denied = !filtered_serving;
            let base = crate::plugindata::org_data_prefix(&org_id, &name, &version);
            if !denied {
                let hook = hook.expect("filtered_serving 隐含 hook 存在");
                for record in &records {
                    let rel = record.key.strip_prefix(&base).unwrap_or(&record.key);
                    if hook.can_write(from, &col_full, rel, &record.value) {
                        if apply_orgq_write(
                            storage,
                            ctx,
                            &org_id,
                            &name,
                            &version,
                            &base,
                            rel,
                            &record.value,
                            now,
                        ) {
                            accepted += 1;
                        } else {
                            rejected += 1;
                        }
                    } else {
                        rejected += 1;
                    }
                }
            } else {
                rejected = records.len();
            }
            let resp = crate::sync::orgsync::build_orgq_write_resp(
                &org_id,
                &col_full,
                &request_id,
                accepted,
                rejected,
                denied,
            );
            out.push(OrgsyncOut::OrgqResp {
                to_root_id: from.to_string(),
                body: resp,
            });
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
        affairsync_out: Vec::new(),
        profile_applied: false,
        feed_blob_out: None,
    })
}

/// orgq-resp 入站（**成员侧**）：数据账号对按需查询/写入的回执。
///
/// 验签：
/// 1. **请求-应答关联**（Z3）：`requestId` 必须是本机先前发出的 orgq-req
///    （`orgq:pending:{requestId}` 在途记录存在），且应答的 `from` == 在途记录
///    `targetRootId`、`orgId`/`collection`/`op` 与在途记录一致——不符静默丢弃
///    （杜绝应答张冠李戴/伪造）；
/// 2. `from` ∈ 该组织数据账号集（响应只接受数据账号，§20.5）。
///
/// 处理（Z2：在途记录删除挪到**处理之后**，缓存/回执落库完成才删——等待侧
/// `orgq_wait_cleared` 以在途记录消失判定应答已到达，先删会让等待侧误以为
/// 应答已落而读到未写完成的缓存/回执）：
/// - 查询应答：写入**成员侧缓存命名空间**（`orgq:cache:{orgId}:{collection}:`，
///   可淘汰、不计副本），随后清除在途记录；
/// - 写入回执：落 `orgq:resp:{requestId}`，随后清除在途记录，并清理对应集合
///   的离线写入队列（send-then-delete 冲刷的删除端，Z4）。
pub(super) fn handle_orgq_resp<S: StorageBackend>(
    storage: &mut S,
    ctx: &InboundContext<'_>,
    from: &str,
    body: &Value,
) -> Result<InboundDmResult> {
    let Some(resp) = crate::sync::orgsync::parse_orgq_resp(body) else {
        return done(fail_response("invalid-body"), Vec::new());
    };
    let (org_id, col_full, request_id) = match &resp {
        crate::sync::orgsync::OrgqResp::Query {
            org_id,
            collection,
            request_id,
            ..
        }
        | crate::sync::orgsync::OrgqResp::Write {
            org_id,
            collection,
            request_id,
            ..
        } => (org_id.clone(), collection.clone(), request_id.clone()),
    };

    // nit：**资格检查先于 requestId 关联**（消 oracle）——非数据账号伪造应答
    // 统一回 `rejected`，不暴露 requestId 是否存在（区分 unknown-request 会让
    // 攻击者探测合法在途 id）。
    let Ok(Some(record)) = OrganizationService::get_record(storage, &org_id) else {
        return done(fail_response("rejected"), Vec::new());
    };
    if !crate::org::roles::is_data_node(&record, from) {
        log::info!("[ORGQ] resp rejected: from={from} not a data node (member) of org={org_id}");
        return done(fail_response("rejected"), Vec::new());
    }

    // 请求-应答关联：本机必须发出过该 requestId 的 orgq-req（在途记录）
    let pending_key = format!("orgq:pending:{request_id}");
    let Some(pending_raw) = storage.get(&pending_key).ok().flatten() else {
        log::info!("[ORGQ] resp dropped: unknown requestId={request_id}");
        return done(fail_response("unknown-request"), Vec::new());
    };
    let pending: Value = serde_json::from_str(&pending_raw).unwrap_or(Value::Null);
    // Z3：应答 `from` == 在途记录的 targetRootId，且 orgId/collection/op 一致
    // ——不符即伪造/错配应答，静默丢弃（不消费在途记录）。
    let target_ok = pending
        .get("targetRootId")
        .and_then(Value::as_str)
        .is_some_and(|t| t == from);
    let scope_ok = pending
        .get("orgId")
        .and_then(Value::as_str)
        .is_some_and(|o| o == org_id)
        && pending
            .get("collection")
            .and_then(Value::as_str)
            .is_some_and(|c| c == col_full);
    let pending_op = pending.get("op").and_then(Value::as_str).unwrap_or("");
    let resp_op = if matches!(resp, crate::sync::orgsync::OrgqResp::Query { .. }) {
        "query"
    } else {
        "write"
    };
    if !(target_ok && scope_ok && pending_op == resp_op) {
        log::info!(
            "[ORGQ] resp dropped: mismatch pending target/scope/op requestId={request_id} from={from}"
        );
        return done(fail_response("unknown-request"), Vec::new());
    }

    // 先处理（落缓存/回执），全部成功后才删除在途记录（Z2：等待侧以在途记录
    // 消失判定应答已到达，须在数据落库完成后才删）。
    let response = match resp {
        crate::sync::orgsync::OrgqResp::Query {
            records,
            complete,
            denied,
            served_at,
            ..
        } => {
            if !denied {
                // 查询应答落**成员侧缓存**（独立命名空间，不算副本、可淘汰）
                let mut ops = Vec::new();
                for rec in &records {
                    // 缓存键仅存集合内相对 key（剥 orgd: 前缀后的相对 key）
                    let base = format!("orgd:{org_id}:{col_full}:");
                    let rel = rec.key.strip_prefix(&base).unwrap_or(&rec.key);
                    let cache_key = crate::sync::orgsync::orgq_cache_key(&org_id, &col_full, rel);
                    ops.push(BatchOperation::put(
                        cache_key,
                        &serde_json::to_string(rec).unwrap_or_default(),
                    ));
                }
                if !ops.is_empty() {
                    storage.batch(ops)?;
                    // O3 缓存淘汰（最简条数上限）：写缓存后按 meta.ts 淘汰最旧超限条目
                    let _ = crate::sync::orgsync::orgq_cache_evict(storage, &org_id, &col_full);
                }
            }
            json!({
                "ok": true,
                "denied": denied,
                "complete": complete,
                "servedAt": served_at,
            })
        }
        crate::sync::orgsync::OrgqResp::Write {
            accepted,
            rejected,
            denied,
            ..
        } => {
            // 写入回执落 `orgq:resp:{requestId}`（成员侧 data_save 在线投递据此
            // 判定 accepted/rejected/denied）。
            crate::sync::orgsync::orgq_resp_put(
                storage,
                &request_id,
                accepted,
                rejected,
                denied,
                ctx.now_ms,
            );
            // Z4：flush 冲刷的离线队列收到受理回执 → 确认后清理该集合队列条目
            // （send-then-delete 的删除端；超时未回执的队列条目保留，下次冲刷重发）。
            let cleared =
                crate::sync::orgsync::orgq_queue_clear_by_collection(storage, &org_id, &col_full);
            if cleared > 0 {
                log::info!(
                    "[ORGQ] resp cleared offline queue | org={org_id} col={col_full} cleared={cleared}"
                );
            }
            json!({
                "ok": true,
                "accepted": accepted,
                "rejected": rejected,
                "denied": denied,
            })
        }
    };
    // Z2：在途记录在数据落库完成后删除（应答只消费一次；等待侧据此判定完成）。
    storage.delete(&pending_key)?;

    Ok(InboundDmResult {
        response,
        events: Vec::new(),
        auto_accept: None,
        self_profile: None,
        device_sync_reply: None,
        device_notice_broadcast: false,
        profile_sync_reply: None,
        pdsync_out: Vec::new(),
        orgsync_out: Vec::new(),
        affairsync_out: Vec::new(),
        profile_applied: false,
        feed_blob_out: None,
    })
}

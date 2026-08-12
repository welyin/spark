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
//! 钩子在数据账号侧**插件后台运行时**执行；`encrypted` 由内核按当前 acl
//! `readers` 名单过滤（O4 落地）。纯逻辑层无法执行 JS 钩子，故本层按规格
//! 「插件未运行时该集合**只存不服务**」语义 **fail-closed**：回 `denied` 降级。
//! 钩子的真实接线（host 层插件运行时）是 O3 工作项 2 的宿主接线点，本层只保证
//! 「未接线即降级」的安全默认。

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
    fn can_write(&self, member: &str, collection: &str, key: &str, value: &serde_json::Value)
        -> bool;
}

/// orgq-req 入站（**数据账号侧**）：受理成员的按需查询 / 写入。
///
/// 验签：
/// 1. 公共前置：`from` ∈ org:meta 成员表；
/// 2. 集合须为**已声明的 org scope 集合**（`org:coll:{orgId}:{name}@v{version}`）；
/// 3. confidentiality 分流（§20.5）：
///    - `encrypted`：O4 名单过滤占位——本机查询/写入统一回 `denied`（明确
///      未实现，注释标清：O4 填内核 acl readers 名单过滤来源）；
///    - `filtered`：钩子在数据账号侧插件后台运行时执行（`canRead`/`canWrite`）。
///      纯逻辑层无法执行 JS 钩子，按规格「插件未运行时该集合**只存不服务**」
///      语义 **fail-closed**——本层不能确认插件运行 + 钩子放行，故回 `denied`
///      降级。宿主把钩子执行接线到插件后台运行时后，此处即真实放行点
///      （O3 工作项 2 的宿主接线，本层只保证「未接线即降级」的安全默认）。
/// 受理一条 orgq 写入记录落库（数据账号侧，§20.5）。`base` 为集合数据键前缀。
///
/// - `value` 非 null → `put_personal` 版本化写入（现状，随复制组扩散）；
/// - `value` 为 null → **删除语义**（Z1）：墓碑 pmeta + org 域 dlog + 记录
///   本体删除，与本地 `tombstone_local` 删除路径同口径（orgd: 键只登 org 域
///   dlog，不污染个人域）。返回 `true` = 落库成功。
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
    // 删除：墓碑 pmeta（vv bump + tombstone）+ org 域 dlog + 记录本体删除。
    // 对齐本地删除路径（tombstone_local）——远端 orgsync 合入墓碑同样补登
    // org 域 dlog（handle_orgsync_data），保证删除在复制组内接力传播。
    let mut meta = crate::sync::get_personal_meta(storage, &key)
        .ok()
        .flatten()
        .unwrap_or_default();
    *meta.vv.entry(ctx.node_id.to_string()).or_insert(0) += 1;
    meta.ts = now;
    meta.node_id = Some(ctx.node_id.to_string());
    meta.tombstone = Some(true);
    if crate::sync::set_personal_meta(storage, &key, &meta).is_err() {
        return false;
    }
    if storage.delete(&key).is_err() {
        return false;
    }
    match crate::sync::orgsync::org_dlog_append_ops(storage, org_id, name, version, &key) {
        Ok((_seq, ops)) => {
            if storage.batch(ops).is_err() {
                return false;
            }
            log::info!("[ORGQ] write tombstone | org={org_id} key={key}");
            true
        }
        Err(_) => false,
    }
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
            org_id,
            collection,
            ..
        }
        | crate::sync::orgsync::OrgqReq::Write {
            org_id,
            collection,
            ..
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
    // 公共前置：from ∈ 成员表
    let Ok(Some(record)) = OrganizationService::get_record(storage, &org_id) else {
        return done(fail_response("rejected"), Vec::new());
    };
    if record.find_member(from).is_none() {
        log::info!("[ORGQ] req rejected: from={from} not in org={org_id}");
        return done(fail_response("rejected"), Vec::new());
    }

    // 集合须为已声明的 org scope 集合
    let decl_key = crate::plugindata::org_decl_key(&org_id, &name, &version);
    let Some(decl) = storage
        .get(&decl_key)
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str::<crate::plugindata::CollectionDeclaration>(&raw).ok())
    else {
        return done(fail_response("collection-not-declared"), Vec::new());
    };

    let now = ctx.now_ms;
    let mut out = Vec::new();
    // confidentiality 分流（§20.5）：
    // - encrypted：内核按当前 acl `readers` 名单过滤（查询非读者 denied 空集、
    //   连元数据都不给；写入不做名单校验——AEAD 在读取方把关）；
    // - filtered：钩子在数据账号侧插件后台运行时执行（canRead/canWrite），
    //   无运行时 fail-closed。
    let is_encrypted = matches!(
        decl.confidentiality,
        crate::plugindata::Confidentiality::Encrypted
    );
    // filtered 钩子可用性：宿主注入钩子且该集合注册了对应读/写钩子（插件运行中）。
    // 读/写能力独立判定（kind），只注册 canRead 的集合写入仍 fail-closed 拒绝。
    let is_filtered = matches!(
        decl.confidentiality,
        crate::plugindata::Confidentiality::Filtered
    );
    let kind = if matches!(req, crate::sync::orgsync::OrgqReq::Query { .. }) {
        "read"
    } else {
        "write"
    };
    let filtered_serving = is_filtered && hook.is_some_and(|h| h.has_runtime(&col_full, kind));
    // encrypted 集合：读取方须为当前 acl readers 成员（O4 填实名单来源）。
    // 无 acl 记录（异常态）→ 非读者（不泄露元数据）。
    let acl_reader = if is_encrypted {
        storage
            .get(&crate::sync::orgsync::acl_key(&org_id, &name, &version))
            .ok()
            .flatten()
            .and_then(|raw| serde_json::from_str::<crate::sync::orgsync::AclRecord>(&raw).ok())
            .is_some_and(|acl| acl.is_reader(from))
    } else {
        false
    };
    match req {
        crate::sync::orgsync::OrgqReq::Query {
            request_id,
            prefix,
            limit,
            cursor,
            ..
        } => {
            // 分流：encrypted 非读者 / filtered 无运行时 fail-closed → denied 空集。
            // encrypted 读者 / filtered 钩子运行中 → 放行采集（encrypted 返回密文，
            // 成员本地解密）。
            let denied = (is_encrypted && !acl_reader) || (is_filtered && !filtered_serving);
            if denied {
                let resp = crate::sync::orgsync::build_orgq_query_resp(
                    &org_id, &col_full, &request_id, &[], true, now, true,
                );
                out.push(OrgsyncOut::OrgqResp {
                    to_root_id: from.to_string(),
                    body: resp,
                });
            } else {
                // 放行：按 limit/cursor 字典序续扫驻留记录。
                // - filtered：逐条过 canRead 过滤（非读者连元数据都不给）；
                // - encrypted：密文对 readers 全量放行（无内容级过滤，ciphertext
                //   对 readers 语义透明；写权限由 AEAD 把关）。
                let hook = if is_filtered {
                    Some(hook.expect("filtered_serving 隐含 hook 存在"))
                } else {
                    None
                };
                let allow = |rel: &str| match hook {
                    Some(h) => h.can_read(from, &col_full, rel),
                    None => true,
                };
                let (records, has_more) =
                    crate::sync::orgsync::collect_orgq_records_page(
                        storage,
                        &org_id,
                        &name,
                        &version,
                        prefix.as_deref(),
                        limit,
                        cursor.as_deref(),
                        allow,
                    );
                // 按 dm 信封体积分批（Z6：complete 仅末批 true；非末批 false）。
                let page_complete = !has_more;
                let batches = crate::sync::orgsync::split_orgq_resp_batches(
                    records,
                    crate::sync::orgsync::ORGSYNC_BATCH_BYTES,
                );
                let last = batches.len();
                for (i, batch) in batches.into_iter().enumerate() {
                    let batch_complete = i == last - 1 && page_complete;
                    let resp = crate::sync::orgsync::build_orgq_query_resp(
                        &org_id, &col_full, &request_id, &batch, batch_complete, now, false,
                    );
                    out.push(OrgsyncOut::OrgqResp {
                        to_root_id: from.to_string(),
                        body: resp,
                    });
                }
            }
        }
        crate::sync::orgsync::OrgqReq::Write {
            request_id,
            records,
            ..
        } => {
            // encrypted 写入**不做名单校验**（§20.5/org-data-sync §5）：集合密钥
            // 同时提供完整性，非密钥持有者构造的写入在读取方解密失败被丢弃；
            // 数据账号侧只存密文、不判名单。filtered 无运行时 fail-closed denied
            //（杜绝未授权写库）；filtered + 钩子运行中 → 逐条 canWrite 裁决。
            let mut accepted = 0usize;
            let mut rejected = 0usize;
            // encrypted 恒受理（写入资格 = 持有集合密钥，读取方 AEAD 把关）；
            // filtered 无运行时 → denied（整批拒绝）。
            let denied = is_filtered && !filtered_serving;
            let base = crate::plugindata::org_data_prefix(&org_id, &name, &version);
            if is_encrypted {
                // encrypted：数据账号只存密文。普通写不判名单（AEAD 在读取方
                // 把关——密钥持有者集合=写权限集合）。**删除（value:null）例外**
                // （O4）：墓碑无密文可验，须要求 from ∈ 当前 acl readers（普通写
                // 维持 AEAD 兜底；删除是权力行为，非读者删除拒绝）。受理删除
                // 落审计日志 `orgq:audit:{orgId}:{collection}:{seq}` = (from,key,ts)。
                for record in &records {
                    let rel = record.key.strip_prefix(&base).unwrap_or(&record.key);
                    if record.value.is_null() && !acl_reader {
                        log::info!(
                            "[ORGQ] encrypted delete denied: from={} non-reader col={col_full} key={}",
                            &from[..std::cmp::min(16, from.len())],
                            &rel[..std::cmp::min(16, rel.len())]
                        );
                        rejected += 1;
                        continue;
                    }
                    if apply_orgq_write(storage, ctx, &org_id, &name, &version, &base, rel, &record.value, now) {
                        // O4：encrypted 删除受理落审计日志（from,key,ts）——
                        // 最简独立审计键族，防删除越权事后追查。
                        if record.value.is_null() {
                            crate::sync::orgsync::orgq_audit_log_delete(
                                storage, &org_id, &col_full, from, rel, now,
                            );
                        }
                        accepted += 1;
                    } else {
                        rejected += 1;
                    }
                }
            } else if !denied {
                let hook = hook.expect("filtered_serving 隐含 hook 存在");
                for record in &records {
                    let rel = record.key.strip_prefix(&base).unwrap_or(&record.key);
                    if hook.can_write(from, &col_full, rel, &record.value) {
                        if apply_orgq_write(storage, ctx, &org_id, &name, &version, &base, rel, &record.value, now) {
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
                &org_id, &col_full, &request_id, accepted, rejected, denied,
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
        profile_applied: false,
        orgkey_unbox: None,
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
    if !crate::org::roles::is_data_account(&record, from) {
        log::info!("[ORGQ] resp rejected: from={from} not a data account of org={org_id}");
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
                storage, &request_id, accepted, rejected, denied, ctx.now_ms,
            );
            // Z4：flush 冲刷的离线队列收到受理回执 → 确认后清理该集合队列条目
            // （send-then-delete 的删除端；超时未回执的队列条目保留，下次冲刷重发）。
            let cleared = crate::sync::orgsync::orgq_queue_clear_by_collection(
                storage, &org_id, &col_full,
            );
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
        profile_applied: false,
        orgkey_unbox: None,
        feed_blob_out: None,
    })
}

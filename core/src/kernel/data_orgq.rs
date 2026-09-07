//! O3 成员侧在线 orgq-req 投递（`Kernel` 内部方法）：data_get/data_query/
//! data_save/data_delete 在 `MemberReadPlan::Orgq`（有在线数据账号）时，把
//! orgq-req 经 p2p dm 直发数据账号，同步等待应答（有界超时）后落缓存/读回执。
//!
//! 这是三通路的**核心投递实现**（Tauri data_ops 与 iframe 桥经本层）。
//! QuickJS 通路（`plugin/host_env_orgq.rs`）共享 `sync::orgsync::orgq_deliver`
//! 的纯逻辑骨架（在途记录生命周期 / 同步等待 / 写入回执）与 orgq 缓存读取；
//! 因插件线程不能 `block_on`，其在线 orgq-req 由同一 orgq-req 机制经本层（桥）
//! 投递，本层只做编排与错误映射。
//!
//! 时序（以 data_query 为例）：
//! 1. 判定有在线数据账号 → 生成 requestId、写在途 `orgq:pending:`；
//! 2. `block_on(node.dm_direct)` 直发 orgq-req（数据账号入站受理回 `ok:true`）；
//! 3. 数据账号侧经 `spawn_orgsync_reply` 异步回 orgq-resp；成员侧事件循环
//!    的 `handle_orgq_resp` 消费在途记录并落缓存；
//! 4. 本方法轮询在途记录消失（bounded 超时）——消失即应答已落缓存/回执，
//!    从缓存读结果返回；超时/投递失败 → 清理在途记录并回退缓存语义。
//!
//! 线程模型：同步方法（Tauri 命令线程 / spawn_blocking 调用），内部以
//! `Handle::block_on` 驱动节点命令通道；事件循环在后台 tokio runtime 独立
//! 跑，本层的同步轮询不阻塞它。p2p 未启动时无法投递 → 直接回退缓存/None。
//!
//! 并发与资源：在途记录**按组织计数**有上限（[`ORGQ_PENDING_MAX`]，F6——
//! 只统计同 orgId，防单组织堆积拖垮他组织），写新在途前做 TTL 清理
//! （[`orgq_pending_cleanup_stale`]，跨组织回收），requestId 每次唯一且含
//! 随机段（Z3 防可预测外推）——同一 collection 并发查询互不串号（应答侧按
//! requestId 关联）。

use super::dm_envelope::KIND_ORGQ_REQ;
use super::{Kernel, Result};
use crate::p2p::PeerNodeInfo;
use crate::p2p::node::system_now_ms;
use crate::plugindata::CollectionDeclaration;
use crate::storage::StorageBackend;
use crate::sync::orgsync::{
    self, build_orgq_query_req, build_orgq_write_req, orgq_pending_cleanup_stale, orgq_pending_put,
    orgq_pending_remove, orgq_resp_take, select_online_data_account,
};

/// orgq-req 同步等待应答的有界超时（任务建议 5-10s，取 8s）。
const ORGQ_DELIVER_TIMEOUT_MS: u64 = 8_000;
/// 轮询在途记录消失的间隔。
const ORGQ_POLL_INTERVAL_MS: u64 = 50;

/// org data-accounts 集合写入路由决策（[`Kernel::data_org_write_route`]）。
pub(crate) enum WriteRoute {
    /// 本地落库（personal / all-members / 本机是数据账号）。
    Local,
    /// 全部数据账号离线 → 已入本地 orgq 队列。
    Enqueued,
    /// 有在线数据账号 → 走 orgq-req 在线受理。
    Online { target_root_id: String },
}

// ── 编排下沉（batch3 §3.2）：共享格自由函数，Kernel facade 与 QuickJS 适配
// 层（plugin/host_env/online.rs）共用一份规则 ──────────────────────────

/// 在线 orgq 投递的共享上下文：Kernel facade（字段直取）与 QuickJS 适配层
/// （`PluginHostShared` 共享格克隆）资源形态不同，投递规则只有一份
/// （同 F3 `OrgkeyDeliverCtx` 模式）。
pub(crate) struct OrgqOnlineCtx {
    /// 存储（版本化句柄克隆；orgq: 键均不受管，直写无版本化副作用）。
    pub storage: crate::kernel::KernelStorage,
    /// 本机 rootId（orgq-req 信封 from）。
    pub my_root_id: String,
    /// 根签名私钥（信封签名）。
    pub signing_key: ed25519_dalek::SigningKey,
    /// p2p 节点命令句柄。
    pub node: std::sync::Arc<crate::p2p::P2pNode>,
    /// 内核 runtime 句柄（同步编排内 block_on 驱动节点命令通道——调用方
    /// 须为同步上下文（Tauri 命令线程 / spawn_blocking），插件 OS 线程不
    /// 直接持有 async 上下文）。
    pub runtime: tokio::runtime::Handle,
    /// 解锁期 BIP39 种子（read-gate §3 查询侧 readAuth：holderProof 用调用方
    /// 域身份私钥签名，域密钥由种子即时派生、不落盘）。None = 未解锁，
    /// 查询不附 readAuth（维持现状语义）。
    pub seed: Option<[u8; 64]>,
    /// 查询发起方域（`plugin:{pluginId}`——holder 身份匹配口径，与
    /// `credential_present_holder_proof` 的签名域同源）。None = 不构造 readAuth。
    pub holder_domain: Option<String>,
}

/// 查询侧 readAuth 构造（read-gate §3）：`read_policy` 为 credential 门禁时，
/// 从本地持有凭证（`cred:held:` 键域）筛选匹配 readPolicy 且 holder 为
/// 调用方域身份的凭证，派生域私钥逐凭证签 holderProof（载荷绑定本次
/// requestId/集合/呈现时刻）。
///
/// 无匹配凭证 / 门禁种类非 credential / 记录损坏 → `None`（查询不附
/// readAuth，维持现状语义——服务端门禁 fail-closed 判 denied；best-effort
/// 不阻塞查询本身）。
pub fn build_query_read_auth<S: StorageBackend>(
    storage: &S,
    seed: &[u8; 64],
    domain: &str,
    read_policy: &crate::plugindata::ReadPolicy,
    request_id: &str,
    col_full: &str,
    now: i64,
) -> Option<crate::credential::ReadAuth> {
    use base64::Engine as _;
    use ed25519_dalek::Signer as _;

    if read_policy.kind != crate::plugindata::ReadPolicyKind::Credential {
        return None;
    }
    let holder = crate::identity::derive_domain_identity(seed, domain);
    let holder_pub = base64::engine::general_purpose::STANDARD
        .encode(holder.signing_key.verifying_key().to_bytes());
    let policy = crate::credential::CredentialReadPolicy {
        cred_types: read_policy.cred_types.clone(),
        verifier_domain: read_policy.verifier_domain.clone(),
    };
    // 持有凭证全量扫描（cred:held: 本地键域；损坏记录跳过，不阻塞其余候选）
    let held: Vec<crate::credential::Credential> = storage
        .scan(&crate::storage::ScanOptions::prefix(
            crate::credential::CRED_HELD_PREFIX,
        ))
        .ok()?
        .into_iter()
        .filter_map(|(_key, raw)| serde_json::from_str(&raw).ok())
        .collect();
    let presentable =
        crate::credential::select_presentable_credentials(held, &policy, &holder_pub);
    crate::credential::build_read_auth(
        &presentable,
        request_id,
        col_full,
        now,
        &|payload| {
            Some(
                base64::engine::general_purpose::STANDARD
                    .encode(holder.signing_key.sign(payload.as_bytes()).to_bytes()),
            )
        },
    )
}

/// 当前是否有在线数据账号（决策复用 `select_online_data_account`，F4 按
/// `col_full` 查 degraded 集避让降级数据账号）。本机是数据账号 → None
/// （本地直读，不投递）。
pub(crate) fn orgq_online_target<S: StorageBackend>(
    storage: &S,
    online_peer_ids: &std::collections::HashSet<String>,
    org_id: &str,
    col_full: &str,
    my_root_id: &str,
) -> Option<String> {
    let record = crate::org::OrganizationService::get_record(storage, org_id)
        .ok()
        .flatten()?;
    if crate::org::roles::is_data_account(&record, my_root_id) {
        return None; // 本机是数据账号 → 本地直读，不投递
    }
    let degraded = crate::sync::orgsync::orgq_degraded_for_collection(storage, org_id, col_full);
    select_online_data_account(&record, online_peer_ids, my_root_id, &degraded)
}

/// 解析目标数据账号成员的连接层 peer（与 `resolve_conv_peer` 的组织空间
/// 回退同口径：取成员端点集首个端点）。无端点信息 → `None`。
pub(crate) fn orgq_target_peer<S: StorageBackend>(
    storage: &S,
    org_id: &str,
    target_root_id: &str,
) -> Option<PeerNodeInfo> {
    let record = crate::org::OrganizationService::get_record(storage, org_id)
        .ok()
        .flatten()?;
    record
        .find_member(target_root_id)?
        .node_info
        .as_ref()?
        .iter()
        .next()
        .map(|info| PeerNodeInfo {
            peer_id: info.peer_id.clone(),
            addresses: info.addresses.clone(),
        })
}

/// 投递 orgq-req 并同步等待应答清除在途记录。返回 `true` = 应答已到达
/// （在途记录已被 `handle_orgq_resp` 消费，缓存/回执已落）；`false` =
/// 超时/投递失败（调用方回退缓存语义/离线入队）。
pub(crate) fn orgq_deliver_and_wait(
    ctx: &OrgqOnlineCtx,
    org_id: &str,
    target_root_id: &str,
    request_id: &str,
    body: serde_json::Value,
) -> Result<bool> {
    let Some(peer) = orgq_target_peer(&ctx.storage, org_id, target_root_id) else {
        return Ok(false);
    };
    let envelope = super::dm_envelope::build_envelope(
        KIND_ORGQ_REQ,
        &ctx.my_root_id,
        target_root_id,
        system_now_ms(),
        body,
        &ctx.signing_key,
    );
    let delivered = ctx
        .runtime
        .block_on(ctx.node.dm_direct(&peer, envelope))
        .ok()
        .flatten()
        .is_some_and(|r| r.get("ok").and_then(serde_json::Value::as_bool) == Some(true));
    if !delivered {
        return Ok(false);
    }
    // 同步等待：轮询在途记录被应答侧（handle_orgq_resp）消费删除
    // （共享骨架 `orgq_wait_cleared`，QuickJS 通路同用）。
    Ok(orgsync::orgq_wait_cleared(
        &ctx.storage,
        request_id,
        ORGQ_POLL_INTERVAL_MS,
        ORGQ_DELIVER_TIMEOUT_MS,
    ))
}

/// 在线查询投递（pending 生命周期 + 投递 + 等待）。返回 `true` = 应答已
/// 到达（缓存已落，调用方读缓存）；`false` = 超时/失败（在途已清，
/// 调用方回退缓存语义）。
pub(crate) fn orgq_online_query(
    ctx: &OrgqOnlineCtx,
    org_id: &str,
    decl: &CollectionDeclaration,
    target_root_id: &str,
    prefix: Option<&str>,
    limit: Option<usize>,
    cursor: Option<&str>,
) -> Result<bool> {
    let now = system_now_ms();
    let col_full = format!("{}@v{}", decl.name, decl.version);
    // 写新在途前做 TTL 清理（防泄漏）
    {
        let mut storage = ctx.storage.clone();
        orgq_pending_cleanup_stale(&mut storage, now);
    }
    let request_id = orgsync::orgq_gen_request_id(now);
    let mut storage = ctx.storage.clone();
    if orgq_pending_put(
        &mut storage,
        &request_id,
        org_id,
        &col_full,
        "query",
        target_root_id,
        now,
    )
    .is_err()
    {
        return Ok(false);
    }
    // read-gate §3：credential 门禁集合在本地持有匹配凭证时附 readAuth
    // 呈现段；无匹配凭证则不带（维持现状语义，服务端 fail-closed denied）。
    let read_auth = match (&ctx.seed, &ctx.holder_domain, decl.read_policy.as_ref()) {
        (Some(seed), Some(domain), Some(read_policy)) => build_query_read_auth(
            &ctx.storage,
            seed,
            domain,
            read_policy,
            &request_id,
            &col_full,
            now,
        ),
        _ => None,
    };
    let body = build_orgq_query_req(
        org_id,
        &col_full,
        prefix,
        limit.unwrap_or(orgsync::ORGQ_LIMIT_DEFAULT),
        cursor,
        &request_id,
        read_auth.as_ref(),
    );
    let arrived = orgq_deliver_and_wait(ctx, org_id, target_root_id, &request_id, body)?;
    if !arrived {
        orgq_pending_remove(&mut storage, &request_id);
    }
    Ok(arrived)
}

/// 在线写入投递（pending + 投递 + 等待 + 读回执）。返回 `Ok(Some(true))` =
/// 受理（accepted，数据账号侧已落库）、`Ok(Some(false))` = 拒绝（denied 或
/// canWrite 拒绝，调用方映射 AccessDenied）、`Ok(None)` = 超时/失败
/// （调用方回退离线入队）。
pub(crate) fn orgq_online_write(
    ctx: &OrgqOnlineCtx,
    org_id: &str,
    decl: &CollectionDeclaration,
    target_root_id: &str,
    key: &str,
    value: &serde_json::Value,
) -> Result<Option<bool>> {
    let now = system_now_ms();
    let col_full = format!("{}@v{}", decl.name, decl.version);
    {
        let mut storage = ctx.storage.clone();
        orgq_pending_cleanup_stale(&mut storage, now);
        // nit：回收未消费的 orgq 写入回执（防 `orgq:resp:` 残留泄漏）
        crate::sync::orgsync::orgq_resp_cleanup_stale(&mut storage, now);
    }
    let request_id = orgsync::orgq_gen_request_id(now);
    let mut storage = ctx.storage.clone();
    if orgq_pending_put(
        &mut storage,
        &request_id,
        org_id,
        &col_full,
        "write",
        target_root_id,
        now,
    )
    .is_err()
    {
        return Ok(None);
    }
    let rel = key
        .strip_prefix(&crate::plugindata::org_data_prefix(
            org_id,
            &decl.name,
            &decl.version,
        ))
        .unwrap_or(key)
        .to_string();
    let body = build_orgq_write_req(
        org_id,
        &col_full,
        &[orgsync::OrgqWriteRecord {
            key: rel,
            value: value.clone(),
        }],
        &request_id,
    );
    let arrived = orgq_deliver_and_wait(ctx, org_id, target_root_id, &request_id, body)?;
    if !arrived {
        orgq_pending_remove(&mut storage, &request_id);
        return Ok(None);
    }
    // 应答到达：读并消费写入回执。单条写入被受理（accepted ≥ 1）→ 成功；
    // 整批 denied 或 canWrite 拒绝（accepted=0）→ 失败（调用方映射 AccessDenied）。
    let mut storage = ctx.storage.clone();
    let receipt = orgq_resp_take(&mut storage, &request_id);
    Ok(receipt.map(|(accepted, _rejected, _denied)| accepted >= 1))
}

impl Kernel {
    /// O3 org data-accounts 写入路由决策：本机对 org data-accounts 集合的
    /// 写是本地落库 / 离线入队 / 在线 orgq-req 投递。personal 集合 / all-members
    /// 集合 / 本机是数据账号 → `Local`（本地写）。
    pub(crate) fn data_org_write_route(
        &self,
        decl: &crate::plugindata::CollectionDeclaration,
        org_id: Option<&str>,
        key: &str,
        value: &serde_json::Value,
    ) -> Result<WriteRoute> {
        let Some(oid) = org_id else {
            return Ok(WriteRoute::Local);
        };
        if decl.space != Some(crate::plugindata::Space::Org)
            || decl.accounts != crate::plugindata::Accounts::DataAccounts
        {
            return Ok(WriteRoute::Local);
        }
        let storage = self.require_storage()?;
        let my_root = self.require_current_root_id()?;
        let Some(record) = crate::org::OrganizationService::get_record(storage, oid)
            .ok()
            .flatten()
        else {
            return Ok(WriteRoute::Local);
        };
        // 本机是数据账号 → 直接落库（本地驻留）
        if crate::org::roles::is_data_account(&record, &my_root) {
            return Ok(WriteRoute::Local);
        }
        // 在线数据账号集合（连接层 peer 在线，与读路由同口径）→ 在线投递
        let online_peer_ids = self.online_peer_ids();
        let col_full = format!("{}@v{}", decl.name, decl.version);
        let degraded = crate::sync::orgsync::orgq_degraded_for_collection(storage, oid, &col_full);
        if let Some(target) =
            select_online_data_account(&record, &online_peer_ids, &my_root, &degraded)
        {
            return Ok(WriteRoute::Online {
                target_root_id: target,
            });
        }
        // 全部数据账号离线 → 本地排队
        self.data_org_enqueue(oid, decl, key, value);
        Ok(WriteRoute::Enqueued)
    }

    /// org data-accounts 写请求本地排队（全部数据账号离线时的兜底落点）。
    pub(crate) fn data_org_enqueue(
        &self,
        oid: &str,
        decl: &crate::plugindata::CollectionDeclaration,
        key: &str,
        value: &serde_json::Value,
    ) {
        let col_full = format!("{}@v{}", decl.name, decl.version);
        let relative = key
            .strip_prefix(&crate::plugindata::org_data_prefix(
                oid,
                &decl.name,
                &decl.version,
            ))
            .unwrap_or(key);
        // F7：存储不可用时静默跳过（best-effort 入队，不 unwrap）
        let Ok(mut storage) = self.require_storage().map(|s| s.clone()) else {
            return;
        };
        let _ = crate::sync::orgsync::orgq_queue_put(&mut storage, oid, &col_full, relative, value);
    }

    /// 装配在线投递上下文（p2p/签名/身份就绪才给 Some；batch3 §3.2 编排
    /// 下沉后，Kernel facade 只是把字段装进共享 ctx 再调自由函数）。
    /// holder_domain 由调用方按查询发起插件域补设（本层不感知域）。
    fn orgq_online_ctx(&self) -> Option<OrgqOnlineCtx> {
        Some(OrgqOnlineCtx {
            storage: self.require_storage().ok()?.clone(),
            my_root_id: self.require_current_root_id().ok()?,
            signing_key: self.unlocked.as_ref()?.identity.signing_key.clone(),
            node: self.p2p.clone()?,
            runtime: self.runtime.handle().clone(),
            seed: self.unlocked.as_ref().map(|u| u.seed),
            holder_domain: None,
        })
    }

    /// 当前是否有在线数据账号（决策复用 `select_online_data_account`，F4 按
    /// `col_full` 查 degraded 集避让降级数据账号）。
    pub(crate) fn orgq_online_target(&self, org_id: &str, col_full: &str) -> Option<String> {
        let my_root = self.require_current_root_id().ok()?;
        orgq_online_target(
            self.require_storage().ok()?,
            &self.online_peer_ids(),
            org_id,
            col_full,
            &my_root,
        )
    }

    /// 成员在线查询投递（data_query 的 Orgq 分支）：发 orgq-req 查询 → 等待
    /// 应答 → 从缓存返回分页。超时/失败回退成员侧缓存（无缓存空页）。
    /// `domain` 为查询发起插件域（read-gate §3：credential 门禁集合据此
    /// 匹配本机持有凭证的 holder 域身份并构造 readAuth 呈现段）。
    pub(crate) fn data_orgq_query(
        &self,
        domain: &str,
        org_id: &str,
        decl: &CollectionDeclaration,
        target_root_id: &str,
        prefix: Option<&str>,
        limit: Option<usize>,
        cursor: Option<&str>,
    ) -> Result<crate::plugindata::QueryPage> {
        if let Some(mut ctx) = self.orgq_online_ctx() {
            ctx.holder_domain = Some(domain.to_string());
            let _ = orgq_online_query(&ctx, org_id, decl, target_root_id, prefix, limit, cursor)?;
        }
        // 无论超时与否都从缓存读（应答到达已落缓存；超时则读旧缓存/空页）
        let storage = self.require_storage()?;
        self.data_orgq_cached_query(storage, decl, prefix, limit, cursor)
    }

    /// 成员在线写入投递（data_save/data_delete 的在线分支）：发 orgq-req 写入
    /// → 等待回执 → 读 `orgq:resp:` 结果。返回 `Ok(Some(true))` = 受理（accepted，
    /// 数据账号侧已落库）、`Ok(Some(false))` = 拒绝（denied 或 canWrite 拒绝，
    /// 调用方映射 AccessDenied）、`Ok(None)` = 超时/失败（调用方回退离线入队）。
    pub(crate) fn data_orgq_write(
        &self,
        org_id: &str,
        decl: &CollectionDeclaration,
        target_root_id: &str,
        key: &str,
        value: &serde_json::Value,
    ) -> Result<Option<bool>> {
        match self.orgq_online_ctx() {
            Some(ctx) => orgq_online_write(&ctx, org_id, decl, target_root_id, key, value),
            None => Ok(None),
        }
    }
}

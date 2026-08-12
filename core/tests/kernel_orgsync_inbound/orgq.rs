//! orgq（O3）入站编排 kernel 级集成测试：按需查询/写入受理 + 成员侧响应
//! 缓存 + 在线数据账号目录。
//!
//! 对应 org-orgsync.md §20.5 与 org-data-sync.md §8 O3 工作项。

use super::*;
use spark_core::kernel::{OrgqPermHook, handle_inbound_dm_with_orgq_hooks};
use spark_core::sync::orgsync::{
    ORGQ_CACHE_MAX_KEYS_PER_COLLECTION, build_orgq_query_req, build_orgq_query_resp,
    build_orgq_write_req, build_orgq_write_resp, orgq_cache_evict, orgq_cache_key,
    orgq_da_online_key, orgq_queue_key, orgq_queue_put, orgq_wipe_org_local,
};

/// 测试用假权限钩子：可控的 can_read/can_write 裁决与 runtime 可用性。
/// `runtime` 控制 has_runtime（模拟插件是否运行 + 是否注册该集合钩子）。
struct FakeHook {
    runtime: bool,
    read: bool,
    write: bool,
}

impl OrgqPermHook for FakeHook {
    fn has_runtime(&self, _collection: &str, _kind: &str) -> bool {
        self.runtime
    }
    fn can_read(&self, _member: &str, _collection: &str, _key: &str) -> bool {
        self.read
    }
    fn can_write(
        &self,
        _member: &str,
        _collection: &str,
        _key: &str,
        _value: &serde_json::Value,
    ) -> bool {
        self.write
    }
}

/// F9-2：按 key 部分放行的假钩子——`allow_read` 集合内的 key 放行，其余拒绝。
/// 用于防「整批放行短路」回归：orgq 查询的 canRead 过滤须逐条按 key 裁决，
/// 不得因钩子返回恒 true/false 而漏过滤或误整批。
struct FakeHookPartial {
    allow_read: std::collections::HashSet<String>,
}

impl OrgqPermHook for FakeHookPartial {
    fn has_runtime(&self, _collection: &str, _kind: &str) -> bool {
        true
    }
    fn can_read(&self, _member: &str, _collection: &str, key: &str) -> bool {
        self.allow_read.contains(key)
    }
    fn can_write(
        &self,
        _member: &str,
        _collection: &str,
        _key: &str,
        _value: &serde_json::Value,
    ) -> bool {
        false
    }
}

/// 构造并投递 orgq-req（数据账号侧），注入测试权限钩子。
fn deliver_orgq_req_with_hook(
    s: &mut MemoryStorage,
    my_root: &str,
    from_key: &SigningKey,
    from_root: &str,
    to_root: &str,
    body: serde_json::Value,
    hook: &dyn OrgqPermHook,
) -> spark_core::kernel::InboundDmResult {
    let envelope = dm_envelope::build_envelope(
        dm_envelope::KIND_ORGQ_REQ,
        from_root,
        to_root,
        NOW,
        body,
        from_key,
    );
    handle_inbound_dm_with_orgq_hooks(
        s,
        my_root,
        "B",
        envelope,
        "peer-from",
        &HashSet::new(),
        NOW,
        "node-b",
        None,
        Some(hook),
    )
    .unwrap()
}

/// 构造并投递一个 orgq-req（成员 → 数据账号）。
fn deliver_orgq_req(
    s: &mut MemoryStorage,
    my_root: &str,
    from_key: &SigningKey,
    from_root: &str,
    to_root: &str,
    body: serde_json::Value,
) -> spark_core::kernel::InboundDmResult {
    deliver_orgsync(
        s,
        my_root,
        "B",
        from_key,
        from_root,
        to_root,
        dm_envelope::KIND_ORGQ_REQ,
        body,
        "peer-from",
        "node-b",
    )
}

/// 构造并投递一个 orgq-resp（数据账号 → 成员）。
fn deliver_orgq_resp(
    s: &mut MemoryStorage,
    my_root: &str,
    from_key: &SigningKey,
    from_root: &str,
    to_root: &str,
    body: serde_json::Value,
) -> spark_core::kernel::InboundDmResult {
    deliver_orgsync(
        s,
        my_root,
        "M",
        from_key,
        from_root,
        to_root,
        dm_envelope::KIND_ORGQ_RESP,
        body,
        "peer-da",
        "node-m",
    )
}

/// 成员侧发送 orgq-req 后在途记录（复刻 data_ops 出站写入；含 targetRootId 供
/// 应答侧 Z3 关联校验——应答 `from` 须 == targetRootId 才被接受；`op` 与应答
/// 类型匹配：query 应答 / write 回执）。
fn mark_pending(s: &mut MemoryStorage, request_id: &str, org_id: &str, target: &str, op: &str) {
    s.put(
        &format!("orgq:pending:{request_id}"),
        &serde_json::json!({ "orgId": org_id, "collection": format!("{NAME}@v{VERSION}"), "op": op, "targetRootId": target }).to_string(),
    )
    .unwrap();
}

// ── 1. orgq-req：授权 + confidentiality 分流（数据账号侧） ────────────────

/// 非成员发 orgq-req → rejected。
#[test]
fn orgq_req_rejects_non_member() {
    let (_a_key, a_root) = self_identity(1); // 数据账号（本机 B 侧接收方视角其实是 A）
    let (x_key, x_root) = self_identity(9); // 非成员
    let (_self_key, self_root) = self_identity(2);
    let mut s = MemoryStorage::new();
    save_org(
        &mut s,
        ORG_ID,
        vec![
            (a_root.as_str(), OrganizationRole::Admin),
            (self_root.as_str(), OrganizationRole::Admin),
        ],
        &[a_root.as_str(), self_root.as_str()],
    );
    declare_org_collection(&mut s, "node-b", ORG_ID, NAME, VERSION, Accounts::DataAccounts, &self_root, NOW);

    let body = build_orgq_query_req(ORG_ID, &format!("{NAME}@v{VERSION}"), None, 10, None, "req-1");
    let r = deliver_orgq_req(&mut s, &self_root, &x_key, &x_root, &self_root, body);
    assert_eq!(r.response["ok"], false);
    assert_eq!(r.response["reason"], json!("rejected"), "非成员 orgq-req 拒绝");
    assert!(r.orgsync_out.is_empty(), "被拒 req 无应答输出");
}

/// 合法成员对 filtered 集合发 orgq-req → 插件未运行降级（denied）。
/// （O3 工作项 2：纯逻辑层不能执行 JS 钩子 → fail-closed「只存不服务」）
#[test]
fn orgq_req_filtered_degrades_when_plugin_not_serving() {
    let (m_key, m_root) = self_identity(3); // 普通成员（查询方）
    let (_self_key, self_root) = self_identity(2); // 本机 B（数据账号）
    let mut s = MemoryStorage::new();
    save_org(
        &mut s,
        ORG_ID,
        vec![
            (m_root.as_str(), OrganizationRole::Member),
            (self_root.as_str(), OrganizationRole::Admin),
        ],
        &[self_root.as_str()],
    );
    declare_org_collection(&mut s, "node-b", ORG_ID, NAME, VERSION, Accounts::DataAccounts, &self_root, NOW);

    // 查询
    let body = build_orgq_query_req(ORG_ID, &format!("{NAME}@v{VERSION}"), None, 10, None, "req-f1");
    let r = deliver_orgq_req(&mut s, &self_root, &m_key, &m_root, &self_root, body);
    assert_eq!(r.response["ok"], true, "数据账号受理（应答走 orgsync_out）");
    assert_eq!(r.orgsync_out.len(), 1, "回一条 orgq-resp");
    let resp = r.orgsync_out[0].body();
    assert_eq!(resp["denied"], json!(true), "filtered 插件未运行 → denied 空集降级");
    assert_eq!(resp["records"], json!([]));
}

/// encrypted 集合（O4 实装）：
/// - 查询按当前 acl `readers` 名单过滤——非读者 denied 空集（连元数据都不给）；
/// - 写入不做名单校验（AEAD 在读取方把关），数据账号直接受理落库。
#[test]
fn orgq_req_encrypted_reader_filtering() {
    use spark_core::plugindata::Confidentiality;
    use spark_core::sync::orgsync::AclRecord;
    let (m_key, m_root) = self_identity(3); // 成员（非读者）
    let (_self_key, self_root) = self_identity(2); // 本机 B（数据账号）
    let mut s = MemoryStorage::new();
    save_org(
        &mut s,
        ORG_ID,
        vec![
            (m_root.as_str(), OrganizationRole::Member),
            (self_root.as_str(), OrganizationRole::Admin),
        ],
        &[self_root.as_str()],
    );
    // 声明 encrypted 集合（须 data-accounts）
    declare_org_collection(&mut s, "node-b", ORG_ID, NAME, VERSION, Accounts::DataAccounts, &self_root, NOW);
    let mut d = spark_core::plugindata::get_declaration_org(&s, ORG_ID, NAME, VERSION)
        .unwrap()
        .unwrap();
    d.confidentiality = Confidentiality::Encrypted;
    s.put(
        &org_decl_key(ORG_ID, NAME, VERSION),
        &serde_json::to_string(&d).unwrap(),
    )
    .unwrap();

    // 写入 → 受理（encrypted 写入不做名单校验，AEAD 在读取方把关）
    let wbody = build_orgq_write_req(
        ORG_ID,
        &format!("{NAME}@v{VERSION}"),
        &[spark_core::sync::orgsync::OrgqWriteRecord {
            key: "k1".to_string(),
            value: json!({"a": 1}),
        }],
        "req-e2",
    );
    let r = deliver_orgq_req(&mut s, &self_root, &m_key, &m_root, &self_root, wbody);
    let resp = r.orgsync_out[0].body();
    assert_eq!(
        resp["accepted"], json!(1),
        "encrypted 写入被数据账号受理（AEAD 读取方把关）"
    );
    assert_eq!(resp["denied"], json!(false));

    // 无 acl 记录 → 非读者（m 不在 readers）→ 查询 denied 空集，元数据不给
    let body = build_orgq_query_req(ORG_ID, &format!("{NAME}@v{VERSION}"), None, 10, None, "req-e1");
    let r = deliver_orgq_req(&mut s, &self_root, &m_key, &m_root, &self_root, body);
    let resp = r.orgsync_out[0].body();
    assert_eq!(
        resp["denied"], json!(true),
        "encrypted 非读者查询 denied 空集"
    );
    assert_eq!(resp["records"], json!([]));

    // 建立 acl，m 加入 readers（读者）→ 查询返回驻留密文记录
    let acl = AclRecord {
        owners: vec![self_root.clone()],
        readers: vec![m_root.clone()],
        epoch: 1,
        updated_at: NOW,
        reset_by: None,
        sig: String::new(),
    };
    s.put(
        &spark_core::sync::orgsync::acl_key(ORG_ID, NAME, VERSION),
        &serde_json::to_string(&acl).unwrap(),
    )
    .unwrap();
    let body = build_orgq_query_req(ORG_ID, &format!("{NAME}@v{VERSION}"), None, 10, None, "req-e3");
    let r = deliver_orgq_req(&mut s, &self_root, &m_key, &m_root, &self_root, body);
    let resp = r.orgsync_out[0].body();
    assert_eq!(resp["denied"], json!(false), "读者查询放行");
    assert!(!resp["records"].as_array().unwrap().is_empty(), "读者拿到驻留密文");
}

/// 数据账号侧处理 orgq-req 后产生出站 orgq-resp（orgsync_out 携带 to_root_id
/// = 原请求方成员 rootId）。
#[test]
fn orgq_req_outbound_resp_carries_member_to_root() {
    let (m_key, m_root) = self_identity(3);
    let (_self_key, self_root) = self_identity(2);
    let mut s = MemoryStorage::new();
    save_org(
        &mut s,
        ORG_ID,
        vec![
            (m_root.as_str(), OrganizationRole::Member),
            (self_root.as_str(), OrganizationRole::Admin),
        ],
        &[self_root.as_str()],
    );
    declare_org_collection(&mut s, "node-b", ORG_ID, NAME, VERSION, Accounts::DataAccounts, &self_root, NOW);
    let body = build_orgq_query_req(ORG_ID, &format!("{NAME}@v{VERSION}"), None, 10, None, "req-to");
    let r = deliver_orgq_req(&mut s, &self_root, &m_key, &m_root, &self_root, body);
    assert_eq!(r.orgsync_out.len(), 1);
    assert_eq!(
        r.orgsync_out[0].to_root_id(),
        m_root,
        "orgq-resp 回发目标 = 原请求方成员 rootId（B1 透传）"
    );
}

// ── 2. orgq-resp：请求-应答关联 + 成员侧缓存（成员侧） ───────────────────

/// 非数据账号发 orgq-resp → rejected。
#[test]
fn orgq_resp_rejects_non_data_account() {
    let (m_key, m_root) = self_identity(3); // 普通成员（伪造数据账号应答）
    let (_self_key, self_root) = self_identity(2); // 本机 M（成员）
    let mut s = MemoryStorage::new();
    save_org(
        &mut s,
        ORG_ID,
        vec![
            (m_root.as_str(), OrganizationRole::Member),
            (self_root.as_str(), OrganizationRole::Member),
        ],
        &[], // 无显式数据账号 → 缺省全体管理员，self 是 member → 非数据账号
    );
    mark_pending(&mut s, "req-1", ORG_ID, &m_root, "query");
    let body = build_orgq_query_resp(ORG_ID, &format!("{NAME}@v{VERSION}"), "req-1", &[], true, NOW, false);
    let r = deliver_orgq_resp(&mut s, &self_root, &m_key, &m_root, &self_root, body);
    assert_eq!(r.response["ok"], false);
    assert_eq!(r.response["reason"], json!("rejected"), "非数据账号 resp 拒绝");
}

/// 未知 requestId（未发出过对应 orgq-req）→ 请求-应答关联拒绝。
#[test]
fn orgq_resp_unknown_request_id_rejected() {
    let (da_key, da_root) = self_identity(1); // 数据账号
    let (_self_key, self_root) = self_identity(2); // 本机 M（成员）
    let mut s = MemoryStorage::new();
    save_org(
        &mut s,
        ORG_ID,
        vec![
            (da_root.as_str(), OrganizationRole::Admin),
            (self_root.as_str(), OrganizationRole::Member),
        ],
        &[da_root.as_str()],
    );
    // 无在途记录
    let body = build_orgq_query_resp(ORG_ID, &format!("{NAME}@v{VERSION}"), "req-ghost", &[], true, NOW, false);
    let r = deliver_orgq_resp(&mut s, &self_root, &da_key, &da_root, &self_root, body);
    assert_eq!(r.response["ok"], false);
    assert_eq!(r.response["reason"], json!("unknown-request"), "未知 requestId 拒绝");
}

/// 合法查询应答：写入成员侧缓存命名空间 + 清除在途记录。
#[test]
fn orgq_resp_query_writes_member_cache() {
    let (da_key, da_root) = self_identity(1); // 数据账号
    let (_self_key, self_root) = self_identity(2); // 本机 M（成员）
    let mut s = MemoryStorage::new();
    save_org(
        &mut s,
        ORG_ID,
        vec![
            (da_root.as_str(), OrganizationRole::Admin),
            (self_root.as_str(), OrganizationRole::Member),
        ],
        &[da_root.as_str()],
    );
    let col = format!("{NAME}@v{VERSION}");
    mark_pending(&mut s, "req-c1", ORG_ID, &da_root, "query");

    // 应答含一条记录
    let key = format!("orgd:{ORG_ID}:{col}:2026-08");
    let records = vec![spark_core::sync::orgsync::OrgqRespRecord {
        key: key.clone(),
        value: json!({"amt": 100}),
        meta: DocMeta {
            vv: [("node-da".to_string(), 1)].into_iter().collect(),
            ts: NOW,
            node_id: Some("node-da".to_string()),
            ..Default::default()
        },
    }];
    let body = build_orgq_query_resp(ORG_ID, &col, "req-c1", &records, true, NOW, false);
    let r = deliver_orgq_resp(&mut s, &self_root, &da_key, &da_root, &self_root, body);
    assert_eq!(r.response["ok"], true);
    assert_eq!(r.response["denied"], json!(false));

    // 成员侧缓存写入（独立命名空间，相对 key）
    let cache_key = orgq_cache_key(ORG_ID, &col, "2026-08");
    let cached = s.get(&cache_key).unwrap().expect("缓存已写入");
    assert!(
        cached.contains("\"amt\":100"),
        "缓存值含应答记录内容"
    );
    // 在途记录已清除（应答只消费一次）
    assert!(s.get(&format!("orgq:pending:req-c1")).unwrap().is_none());
    // 不在 orgd: 数据键域落库（缓存不算副本）
    assert!(s.get(&key).unwrap().is_none(), "成员侧缓存不落 orgd 副本");
}

/// 写入回执：清除在途记录并回传 accepted/rejected/denied。
#[test]
fn orgq_resp_write_receipt_clears_pending() {
    let (da_key, da_root) = self_identity(1);
    let (_self_key, self_root) = self_identity(2);
    let mut s = MemoryStorage::new();
    save_org(
        &mut s,
        ORG_ID,
        vec![
            (da_root.as_str(), OrganizationRole::Admin),
            (self_root.as_str(), OrganizationRole::Member),
        ],
        &[da_root.as_str()],
    );
    mark_pending(&mut s, "req-w1", ORG_ID, &da_root, "write");
    let col = format!("{NAME}@v{VERSION}");
    let body = build_orgq_write_resp(ORG_ID, &col, "req-w1", 2, 0, false);
    let r = deliver_orgq_resp(&mut s, &self_root, &da_key, &da_root, &self_root, body);
    assert_eq!(r.response["accepted"], json!(2));
    assert_eq!(r.response["denied"], json!(false));
    assert!(s.get(&format!("orgq:pending:req-w1")).unwrap().is_none());
}

// ── 3. 在线数据账号目录：hello roles 含 "data" 时标记 ────────────────────

/// orgsync-hello 携带 roles=["data"] → 入站标记该 from 为在线数据账号。
#[test]
fn hello_with_data_role_marks_online_data_account() {
    let (da_key, da_root) = self_identity(1); // 对端数据账号
    let (_self_key, self_root) = self_identity(2); // 本机
    let mut s = MemoryStorage::new();
    save_org(
        &mut s,
        ORG_ID,
        vec![
            (da_root.as_str(), OrganizationRole::Admin),
            (self_root.as_str(), OrganizationRole::Admin),
        ],
        &[da_root.as_str()],
    );
    declare_org_collection(&mut s, "node-b", ORG_ID, NAME, VERSION, Accounts::AllMembers, &self_root, NOW);
    // 构造 roles=["data"] 的 hello
    let mut collections = serde_json::Map::new();
    collections.insert(
        format!("{NAME}@v{VERSION}"),
        json!({ "vv": {}, "dlogAck": 0 }),
    );
    let body = build_orgsync_hello(ORG_ID, collections, &["data".to_string()], "pc");
    let r = deliver_orgsync(
        &mut s,
        &self_root,
        "B",
        &da_key,
        &da_root,
        &self_root,
        dm_envelope::KIND_ORGSYNC_HELLO,
        body,
        "peer-da",
        "node-b",
    );
    assert_eq!(r.response["ok"], true);
    assert!(
        s.get(&orgq_da_online_key(ORG_ID, &da_root)).unwrap().is_some(),
        "hello roles=[data] 标记在线数据账号"
    );
}

// ── 4. O3 权限钩子（filtered）：钩子通过 / 钩子拒绝 / 插件未运行降级 ──────

/// 数据账号侧组织成员 + data-accounts 集合（filtered）就绪的公共 setup。
fn setup_filtered_collection(
    self_root: &str,
    member_root: &str,
) -> MemoryStorage {
    let mut s = MemoryStorage::new();
    save_org(
        &mut s,
        ORG_ID,
        vec![
            (member_root, OrganizationRole::Member),
            (self_root, OrganizationRole::Admin),
        ],
        &[self_root],
    );
    // 集合默认 filtered（confidentiality 缺省即 filtered）+ data-accounts
    let d = declare_org_collection(
        &mut s, "node-b", ORG_ID, NAME, VERSION, Accounts::DataAccounts, &self_root, NOW,
    );
    assert_eq!(d.confidentiality, spark_core::plugindata::Confidentiality::Filtered);
    s
}

/// 钩子通过（query）：filtered 集合查询经 canRead 放行 → 返回驻留记录。
#[test]
fn orgq_req_filtered_query_hook_pass_serves_records() {
    let (m_key, m_root) = self_identity(3);
    let (_self_key, self_root) = self_identity(2);
    let mut s = setup_filtered_collection(&self_root, &m_root);
    // 数据账号驻留两条记录
    write_org_data(&mut s, "node-b", ORG_ID, NAME, VERSION, "k1", "\"v1\"", NOW);
    write_org_data(&mut s, "node-b", ORG_ID, NAME, VERSION, "k2", "\"v2\"", NOW + 1);

    let body = build_orgq_query_req(ORG_ID, &format!("{NAME}@v{VERSION}"), None, 10, None, "req-hp");
    let r = deliver_orgq_req_with_hook(
        &mut s, &self_root, &m_key, &m_root, &self_root, body,
        &FakeHook { runtime: true, read: true, write: false },
    );
    assert_eq!(r.response["ok"], true);
    let resp = r.orgsync_out[0].body();
    assert_eq!(resp["denied"], json!(false), "钩子通过 → 受理（非降级）");
    let records = resp["records"].as_array().unwrap();
    assert_eq!(records.len(), 2, "canRead 放行两条驻留记录");
    assert_eq!(records[0]["key"], json!(format!("{}k1", org_data_prefix(ORG_ID, NAME, VERSION))));
}

/// F9-2：canRead 按 key **部分放行**——驻留多条，仅放行部分 key，其余被过滤
/// （防整批放行/整批拒绝短路）。逐条按 key 裁决而非恒 true/false。
#[test]
fn orgq_req_filtered_query_hook_partial_allow_by_key() {
    let (m_key, m_root) = self_identity(3);
    let (_self_key, self_root) = self_identity(2);
    let mut s = setup_filtered_collection(&self_root, &m_root);
    write_org_data(&mut s, "node-b", ORG_ID, NAME, VERSION, "k1", "\"v1\"", NOW);
    write_org_data(&mut s, "node-b", ORG_ID, NAME, VERSION, "k2", "\"v2\"", NOW + 1);
    write_org_data(&mut s, "node-b", ORG_ID, NAME, VERSION, "k3", "\"v3\"", NOW + 2);

    let hook = FakeHookPartial {
        allow_read: ["k1".to_string(), "k3".to_string()].into_iter().collect(),
    };
    let body = build_orgq_query_req(ORG_ID, &format!("{NAME}@v{VERSION}"), None, 10, None, "req-part");
    let r = deliver_orgq_req_with_hook(&mut s, &self_root, &m_key, &m_root, &self_root, body, &hook);
    let resp = r.orgsync_out[0].body();
    assert_eq!(resp["denied"], json!(false));
    let records = resp["records"].as_array().unwrap();
    // 仅 k1、k3 放行；k2 被逐条过滤
    let rels: Vec<String> = records
        .iter()
        .map(|rec| {
            rec["key"]
                .as_str()
                .unwrap()
                .strip_prefix(&org_data_prefix(ORG_ID, NAME, VERSION))
                .unwrap()
                .to_string()
        })
        .collect();
    assert_eq!(rels, vec!["k1".to_string(), "k3".to_string()], "canRead 按 key 逐条部分放行");
}

/// 钩子拒绝（query）：canRead 拒绝全部 → 空集（非读者连元数据都不给）。
#[test]
fn orgq_req_filtered_query_hook_reject_returns_empty() {
    let (m_key, m_root) = self_identity(3);
    let (_self_key, self_root) = self_identity(2);
    let mut s = setup_filtered_collection(&self_root, &m_root);
    write_org_data(&mut s, "node-b", ORG_ID, NAME, VERSION, "k1", "\"v1\"", NOW);

    let body = build_orgq_query_req(ORG_ID, &format!("{NAME}@v{VERSION}"), None, 10, None, "req-hr");
    let r = deliver_orgq_req_with_hook(
        &mut s, &self_root, &m_key, &m_root, &self_root, body,
        &FakeHook { runtime: true, read: false, write: false },
    );
    let resp = r.orgsync_out[0].body();
    assert_eq!(resp["denied"], json!(false));
    assert_eq!(resp["records"], json!([]), "canRead 拒绝 → 空集（元数据不给）");
}

/// 钩子拒绝（write）：canWrite 拒绝 → rejected 计数，数据不落库。
#[test]
fn orgq_req_filtered_write_hook_reject_no_land() {
    let (m_key, m_root) = self_identity(3);
    let (_self_key, self_root) = self_identity(2);
    let mut s = setup_filtered_collection(&self_root, &m_root);

    let wbody = build_orgq_write_req(
        ORG_ID, &format!("{NAME}@v{VERSION}"),
        &[spark_core::sync::orgsync::OrgqWriteRecord {
            key: "k1".to_string(), value: json!({"a": 1}),
        }],
        "req-wr",
    );
    let r = deliver_orgq_req_with_hook(
        &mut s, &self_root, &m_key, &m_root, &self_root, wbody,
        &FakeHook { runtime: true, read: false, write: false },
    );
    let resp = r.orgsync_out[0].body();
    assert_eq!(resp["denied"], json!(false));
    assert_eq!(resp["accepted"], json!(0));
    assert_eq!(resp["rejected"], json!(1), "canWrite 拒绝 → 全部 rejected");
    let data_key = format!("{}k1", org_data_prefix(ORG_ID, NAME, VERSION));
    assert!(s.get(&data_key).unwrap().is_none(), "被拒写入不落库");
}

/// 钩子通过（write）：canWrite 放行 → accepted 并落库（随复制组扩散）。
#[test]
fn orgq_req_filtered_write_hook_pass_lands() {
    let (m_key, m_root) = self_identity(3);
    let (_self_key, self_root) = self_identity(2);
    let mut s = setup_filtered_collection(&self_root, &m_root);

    let wbody = build_orgq_write_req(
        ORG_ID, &format!("{NAME}@v{VERSION}"),
        &[spark_core::sync::orgsync::OrgqWriteRecord {
            key: "k1".to_string(), value: json!({"a": 1}),
        }],
        "req-wp",
    );
    let r = deliver_orgq_req_with_hook(
        &mut s, &self_root, &m_key, &m_root, &self_root, wbody,
        &FakeHook { runtime: true, read: false, write: true },
    );
    let resp = r.orgsync_out[0].body();
    assert_eq!(resp["denied"], json!(false));
    assert_eq!(resp["accepted"], json!(1), "canWrite 放行 → accepted");
    assert_eq!(resp["rejected"], json!(0));
    let data_key = format!("{}k1", org_data_prefix(ORG_ID, NAME, VERSION));
    assert!(s.get(&data_key).unwrap().is_some(), "放行写入已落库");
}

// ── 5. O3 缓存淘汰与成员移除擦除 ────────────────────────────────────────

/// 缓存淘汰（最简条数上限）：超过上限后按 meta.ts 淘汰最旧条目。
#[test]
fn orgq_cache_evict_trims_over_capacity() {
    let mut s = MemoryStorage::new();
    let col = format!("{NAME}@v{VERSION}");
    // 塞入超过上限的条目（每条不同 meta.ts，递增）
    let over = ORGQ_CACHE_MAX_KEYS_PER_COLLECTION + 10;
    for i in 0..over {
        let rec = spark_core::sync::orgsync::OrgqRespRecord {
            key: format!("k{i}"),
            value: json!(i),
            meta: DocMeta {
                ts: NOW + i as i64,
                ..Default::default()
            },
        };
        s.put(
            &orgq_cache_key(ORG_ID, &col, &format!("k{i}")),
            &serde_json::to_string(&rec).unwrap(),
        )
        .unwrap();
    }
    let removed = orgq_cache_evict(&mut s, ORG_ID, &col);
    assert_eq!(removed, 10, "淘汰超限的最旧 10 条");
    let remaining: Vec<_> = s
        .scan(&spark_core::storage::ScanOptions::prefix(&format!("orgq:cache:{ORG_ID}:{col}:")))
        .unwrap()
        .into_iter()
        .collect();
    assert_eq!(remaining.len(), ORGQ_CACHE_MAX_KEYS_PER_COLLECTION);
    // 最旧的 k0..k9 已被淘汰
    for i in 0..10 {
        assert!(s.get(&orgq_cache_key(ORG_ID, &col, &format!("k{i}"))).unwrap().is_none());
    }
    // 最新的仍在
    assert!(s.get(&orgq_cache_key(ORG_ID, &col, &format!("k{}", over - 1))).unwrap().is_some());
}

/// 成员移除擦除：缓存 + 离线队列 + 在线目录全部清除。
#[test]
fn orgq_wipe_org_local_clears_cache_queue_online_dir() {
    let mut s = MemoryStorage::new();
    let col = format!("{NAME}@v{VERSION}");
    // 造缓存
    s.put(&orgq_cache_key(ORG_ID, &col, "k1"), "\"v\"").unwrap();
    s.put(&orgq_cache_key(ORG_ID, &col, "k2"), "\"v\"").unwrap();
    // 造离线写入队列
    s.put(
        &orgq_queue_key(ORG_ID, &col, "qk"),
        &serde_json::json!({ "value": "qv" }).to_string(),
    )
    .unwrap();
    // 造在线数据账号目录
    s.put(&orgq_da_online_key(ORG_ID, "da-a"), "123").unwrap();

    let removed = orgq_wipe_org_local(&mut s, ORG_ID);
    assert!(removed >= 4, "缓存/队列/在线目录全部擦除");
    assert!(s.get(&orgq_cache_key(ORG_ID, &col, "k1")).unwrap().is_none());
    assert!(s.get(&orgq_queue_key(ORG_ID, &col, "qk")).unwrap().is_none());
    assert!(s.get(&orgq_da_online_key(ORG_ID, "da-a")).unwrap().is_none());
    // 其他组织的缓存不受影响（跨组织隔离）
    s.put(&orgq_cache_key("org_other", &col, "kx"), "\"v\"").unwrap();
    assert!(s.get(&orgq_cache_key("org_other", &col, "kx")).unwrap().is_some());
}

// ── 6. O3 全离线排队-上线收敛 ──────────────────────────────────────────

/// 成员写 data-accounts 且全部数据账号离线 → 本地排队；数据账号上线
/// （hello roles 含 data）→ 自动冲刷为 orgq-req 写入信封。
#[test]
fn orgq_offline_queue_flushes_on_data_account_online() {
    let (da_key, da_root) = self_identity(1); // 数据账号
    let (_self_key, self_root) = self_identity(2); // 本机 M（普通成员）
    let mut s = MemoryStorage::new();
    save_org(
        &mut s,
        ORG_ID,
        vec![
            (da_root.as_str(), OrganizationRole::Admin),
            (self_root.as_str(), OrganizationRole::Member),
        ],
        &[da_root.as_str()],
    );
    declare_org_collection(&mut s, "node-b", ORG_ID, NAME, VERSION, Accounts::DataAccounts, &self_root, NOW);

    // 成员离线写入排队（O3 工作项 3：全部数据账号离线 → data_ops 落本地队列，
    // 经 orgq_queue_put 写入 `{collection,key,value}`）
    let col = format!("{NAME}@v{VERSION}");
    orgq_queue_put(&mut s, ORG_ID, &col, "k1", &serde_json::json!({ "amt": 1 })).unwrap();
    assert!(spark_core::sync::orgsync::orgq_queue_has_data(&s, ORG_ID));

    // 数据账号上线（hello roles=["data"]）→ 自动冲刷
    let mut collections = serde_json::Map::new();
    collections.insert(col.clone(), json!({ "vv": {}, "dlogAck": 0 }));
    let hello = build_orgsync_hello(ORG_ID, collections, &["data".to_string()], "pc");
    let r = deliver_orgsync(
        &mut s, &self_root, "M", &da_key, &da_root, &self_root,
        dm_envelope::KIND_ORGSYNC_HELLO, hello, "peer-da", "node-m",
    );
    assert_eq!(r.response["ok"], true);
    // 冲刷出 orgq-req 写入信封（body op=write 即 orgq-req 形态）
    let reqs: Vec<_> = r
        .orgsync_out
        .iter()
        .map(|o| o.body())
        .filter(|b| b.get("op").and_then(serde_json::Value::as_str) == Some("write"))
        .collect();
    assert_eq!(reqs.len(), 1, "上线冲刷产生一条 orgq-req 写入");
    let body = reqs[0];
    assert_eq!(body["collection"], json!(col));
    assert_eq!(body["records"][0]["key"], json!("k1"));
    assert_eq!(body["records"][0]["value"]["amt"], json!(1));
    // Z4 send-then-delete：冲刷只读不删，队列条目保留到受理回执（不丢写）。
    // 投递失败/超时保留，下次上线再冲刷重新投递。
    assert!(
        spark_core::sync::orgsync::orgq_queue_has_data(&s, ORG_ID),
        "send-then-delete：冲刷后队列保留（回执确认才删）"
    );
    // 冲刷的 requestId 已写 pending（回执可关联）
    let req_id = body["requestId"].as_str().unwrap();
    assert!(
        s.get(&format!("orgq:pending:{req_id}")).unwrap().is_some(),
        "flush 的 requestId 写 pending 供回执关联"
    );
    // 模拟数据账号受理回执 → 成员侧 handle_orgq_resp 消费 → 清理该集合队列
    let write_resp = build_orgq_write_resp(ORG_ID, &col, req_id, 1, 0, false);
    let resp_r = deliver_orgq_resp(&mut s, &self_root, &da_key, &da_root, &self_root, write_resp);
    assert_eq!(resp_r.response["accepted"], json!(1));
    assert!(
        !spark_core::sync::orgsync::orgq_queue_has_data(&s, ORG_ID),
        "受理回执确认后清理该集合离线队列（send-then-delete 删除端）"
    );
}

// ── 7. Z1 orgq 写路径删除语义（value:null → 墓碑 + org dlog） ────────────

/// Z1 在线删除：数据账号受理 value:null 的 orgq-req 写请求（canWrite 放行）→
/// 删除语义（记录本体删除 + pmeta 墓碑 + org 域 dlog 有序号），对齐本地删除
/// 路径，而非落 "null" 字符串（永不删除）。
#[test]
fn orgq_online_delete_writes_tombstone_and_org_dlog() {
    let (m_key, m_root) = self_identity(3);
    let (_self_key, self_root) = self_identity(2);
    let mut s = setup_filtered_collection(&self_root, &m_root);
    // 数据账号先驻留一条记录（vv=1）
    write_org_data(&mut s, "node-b", ORG_ID, NAME, VERSION, "k-del", "\"v1\"", NOW);
    let record_key = format!("{}k-del", org_data_prefix(ORG_ID, NAME, VERSION));
    assert!(s.get(&record_key).unwrap().is_some(), "删除前记录驻留");

    // 在线写删除：value:null，canWrite 放行
    let wbody = build_orgq_write_req(
        ORG_ID, &format!("{NAME}@v{VERSION}"),
        &[spark_core::sync::orgsync::OrgqWriteRecord {
            key: "k-del".to_string(), value: json!(null),
        }],
        "req-del-online",
    );
    let r = deliver_orgq_req_with_hook(
        &mut s, &self_root, &m_key, &m_root, &self_root, wbody,
        &FakeHook { runtime: true, read: true, write: true },
    );
    let resp = r.orgsync_out[0].body();
    assert_eq!(resp["accepted"], json!(1), "删除受理 accepted");
    // 记录本体已删除
    assert!(s.get(&record_key).unwrap().is_none(), "记录本体已删（不落 null 字符串）");
    // 墓碑 pmeta 已写（tombstone: true, vv bump）
    let meta = get_personal_meta(&s, &record_key).unwrap().expect("墓碑 pmeta 存在");
    assert_eq!(meta.tombstone, Some(true), "pmeta 墓碑标记");
    // org 域 dlog 有序号
    let entries = org_dlog_entries_after(&s, ORG_ID, NAME, VERSION, 0).unwrap();
    assert_eq!(entries.len(), 1, "org dlog 有条目");
    assert_eq!(entries[0].1, record_key, "dlog 条目指向删除键");
}

/// Z1 离线删除：value:null 经离线队列重放 → 数据账号受理 → 删除语义（记录本体
/// 删除 + pmeta 墓碑 + org dlog）。与在线删除同路径（apply_orgq_write 共用）。
#[test]
fn orgq_offline_delete_replay_writes_tombstone() {
    let (da_key, da_root) = self_identity(1); // 数据账号（受理方）
    let (self_key, self_root) = self_identity(2); // 本机 M（普通成员，写入方）
    let mut s = MemoryStorage::new();
    save_org(
        &mut s,
        ORG_ID,
        vec![
            (da_root.as_str(), OrganizationRole::Admin),
            (self_root.as_str(), OrganizationRole::Member),
        ],
        &[da_root.as_str()],
    );
    declare_org_collection(&mut s, "node-b", ORG_ID, NAME, VERSION, Accounts::DataAccounts, &self_root, NOW);
    let col = format!("{NAME}@v{VERSION}");
    // 数据账号侧先驻留记录
    write_org_data(&mut s, "node-b", ORG_ID, NAME, VERSION, "k-del", "\"v1\"", NOW);
    let record_key = format!("{}k-del", org_data_prefix(ORG_ID, NAME, VERSION));

    // 成员离线写删除入队（value:null）
    orgq_queue_put(&mut s, ORG_ID, &col, "k-del", &json!(null)).unwrap();
    // 上线冲刷 → orgq-req 写入信封（value:null）
    let mut collections = serde_json::Map::new();
    collections.insert(col.clone(), json!({ "vv": {}, "dlogAck": 0 }));
    let hello = build_orgsync_hello(ORG_ID, collections, &["data".to_string()], "pc");
    let r = deliver_orgsync(
        &mut s, &self_root, "M", &da_key, &da_root, &self_root,
        dm_envelope::KIND_ORGSYNC_HELLO, hello, "peer-da", "node-m",
    );
    let reqs: Vec<_> = r
        .orgsync_out
        .iter()
        .map(|o| o.body())
        .filter(|b| b.get("op").and_then(serde_json::Value::as_str) == Some("write"))
        .collect();
    assert_eq!(reqs.len(), 1, "上线冲刷产生一条 orgq-req 写入");
    // 数据账号侧受理该 orgq-req（canWrite 放行）→ 删除语义落库
    let req = reqs[0].clone();
    let rec_key = req["records"][0]["key"].as_str().unwrap();
    assert_eq!(rec_key, "k-del");
    let ar = deliver_orgq_req_with_hook(
        &mut s, &da_root, &self_key, &self_root, &da_root, req,
        &FakeHook { runtime: true, read: true, write: true },
    );
    let resp = ar.orgsync_out[0].body();
    assert_eq!(resp["accepted"], json!(1), "离线删除受理 accepted");
    // 记录本体删除 + 墓碑 pmeta + org dlog 有序号
    assert!(s.get(&record_key).unwrap().is_none(), "离线删除记录本体已删");
    let meta = get_personal_meta(&s, &record_key).unwrap().expect("墓碑 pmeta 存在");
    assert_eq!(meta.tombstone, Some(true), "pmeta 墓碑标记");
    let entries = org_dlog_entries_after(&s, ORG_ID, NAME, VERSION, 0).unwrap();
    assert_eq!(entries.len(), 1, "org dlog 有条目");
    assert_eq!(entries[0].1, record_key);
}

/// Z6 分页 + 分批：数据账号侧按 limit/cursor 字典序续扫驻留记录，filtered
/// canRead 放行；应答按信封体积分批，complete 仅末批 true。成员凭末批
/// complete=false 带 cursor 续扫下一页。
#[test]
fn orgq_query_paginates_with_cursor() {
    let (m_key, m_root) = self_identity(3);
    let (_self_key, self_root) = self_identity(2);
    let mut s = setup_filtered_collection(&self_root, &m_root);
    // 数据账号驻留 5 条（字典序 k1..k5）
    for i in 1..=5 {
        write_org_data(&mut s, "node-b", ORG_ID, NAME, VERSION, &format!("k{i}"), &format!("\"v{i}\""), NOW + i);
    }
    let col = format!("{NAME}@v{VERSION}");

    // 第一页：limit=2
    let body = build_orgq_query_req(ORG_ID, &col, None, 2, None, "req-page1");
    let r = deliver_orgq_req_with_hook(
        &mut s, &self_root, &m_key, &m_root, &self_root, body,
        &FakeHook { runtime: true, read: true, write: true },
    );
    assert_eq!(r.orgsync_out.len(), 1, "一页 2 条单批");
    let resp = r.orgsync_out[0].body();
    assert_eq!(resp["denied"], json!(false));
    let recs = resp["records"].as_array().unwrap();
    assert_eq!(recs.len(), 2, "limit=2 返回 2 条");
    assert_eq!(resp["complete"], json!(false), "还有更多 → complete=false");
    let last_key = recs[1]["key"].as_str().unwrap();
    let last_rel = last_key.strip_prefix(&org_data_prefix(ORG_ID, NAME, VERSION)).unwrap();

    // 第二页：cursor = 上一页末相对 key → 续扫剩余 3 条
    let body2 = build_orgq_query_req(ORG_ID, &col, None, 2, Some(last_rel), "req-page2");
    let r2 = deliver_orgq_req_with_hook(
        &mut s, &self_root, &m_key, &m_root, &self_root, body2,
        &FakeHook { runtime: true, read: true, write: true },
    );
    let recs2 = r2.orgsync_out[0].body();
    assert_eq!(recs2["records"].as_array().unwrap().len(), 2, "续扫返回 2 条");
    assert_eq!(recs2["complete"], json!(false), "仍有余量");

    // 第三页：续扫剩余 1 条 → complete=true（末页）
    let third = {
        let last = recs2["records"].as_array().unwrap().last().unwrap();
        last["key"].as_str().unwrap().strip_prefix(&org_data_prefix(ORG_ID, NAME, VERSION)).unwrap().to_string()
    };
    let body3 = build_orgq_query_req(ORG_ID, &col, None, 2, Some(&third), "req-page3");
    let r3 = deliver_orgq_req_with_hook(
        &mut s, &self_root, &m_key, &m_root, &self_root, body3,
        &FakeHook { runtime: true, read: true, write: true },
    );
    let recs3 = r3.orgsync_out[0].body();
    assert_eq!(recs3["records"].as_array().unwrap().len(), 1, "末页 1 条");
    assert_eq!(recs3["complete"], json!(true), "末页 complete=true");
}

/// Z3 应答关联：应答 from != 在途记录 targetRootId（数据账号错配）→ 静默丢弃，
/// 在途记录保留（不消费）。
#[test]
fn orgq_resp_mismatched_target_dropped() {
    let (_da_key, da_root) = self_identity(1);
    let (x_key, x_root) = self_identity(9); // 另一数据账号（错配 target）
    let (_self_key, self_root) = self_identity(2);
    let mut s = MemoryStorage::new();
    save_org(
        &mut s,
        ORG_ID,
        vec![
            (da_root.as_str(), OrganizationRole::Admin),
            (x_root.as_str(), OrganizationRole::Admin),
            (self_root.as_str(), OrganizationRole::Member),
        ],
        &[da_root.as_str(), x_root.as_str()],
    );
    // 在途记录 target = da_root；应答却来自 x_root → 丢弃
    mark_pending(&mut s, "req-mis", ORG_ID, &da_root, "query");
    let col = format!("{NAME}@v{VERSION}");
    let body = build_orgq_query_resp(ORG_ID, &col, "req-mis", &[], true, NOW, false);
    let r = deliver_orgq_resp(&mut s, &self_root, &x_key, &x_root, &self_root, body);
    assert_eq!(r.response["ok"], false, "错配应答被丢弃");
    assert_eq!(r.response["reason"], json!("unknown-request"));
    // 在途记录保留（错配应答不消费）
    assert!(
        s.get(&format!("orgq:pending:req-mis")).unwrap().is_some(),
        "错配应答不消费在途记录"
    );
}

/// O4：encrypted 集合 orgq **删除**（value:null）须 from ∈ 当前 acl readers——
/// 墓碑无密文可验（AEAD 无法为删除提供完整性），非读者删除拒绝。普通写维持
/// AEAD 兜底（不判名单）。受理删除落审计日志 (from,key,ts)。
#[test]
fn orgq_encrypted_delete_requires_reader_and_audits() {
    use spark_core::plugindata::Confidentiality;
    use spark_core::sync::orgsync::AclRecord;
    let (m_key, m_root) = self_identity(3); // 成员（先非读者，后读者）
    let (_da_key, da_root) = self_identity(1); // 数据账号（本机 B）
    let (_self_key, self_root) = self_identity(2);
    let mut s = MemoryStorage::new();
    save_org(
        &mut s,
        ORG_ID,
        vec![
            (da_root.as_str(), OrganizationRole::Admin),
            (m_root.as_str(), OrganizationRole::Member),
            (self_root.as_str(), OrganizationRole::Admin),
        ],
        &[self_root.as_str()],
    );
    declare_org_collection(&mut s, "node-b", ORG_ID, NAME, VERSION, Accounts::DataAccounts, &self_root, NOW);
    // 声明为 encrypted
    let mut d = spark_core::plugindata::get_declaration_org(&s, ORG_ID, NAME, VERSION)
        .unwrap()
        .unwrap();
    d.confidentiality = Confidentiality::Encrypted;
    s.put(
        &org_decl_key(ORG_ID, NAME, VERSION),
        &serde_json::to_string(&d).unwrap(),
    )
    .unwrap();
    // 先落一条密文记录（删除对象）
    let data_key = format!("{}k9", org_data_prefix(ORG_ID, NAME, VERSION));
    put_personal(&mut s, "node-b", &data_key, r#"{"epoch":1,"nonce":"n","ct":"c"}"#, NOW).unwrap();

    let col = format!("{NAME}@v{VERSION}");
    // (a) 无 acl → m 非读者 → 删除拒绝（rejected=1，墓碑不落）
    let del_body = build_orgq_write_req(
        ORG_ID,
        &col,
        &[spark_core::sync::orgsync::OrgqWriteRecord {
            key: "k9".to_string(),
            value: serde_json::Value::Null,
        }],
        "req-o1",
    );
    let r = deliver_orgq_req(&mut s, &self_root, &m_key, &m_root, &self_root, del_body);
    let resp = r.orgsync_out[0].body();
    assert_eq!(resp["accepted"], json!(0), "非读者删除被拒");
    assert_eq!(resp["rejected"], json!(1));
    assert!(s.get(&data_key).unwrap().is_some(), "非读者删除不落墓碑");
    // 审计日志为空（被拒删除不审计）
    assert!(
        s.scan(&ScanOptions::prefix("orgq:audit:")).unwrap().is_empty(),
        "被拒删除不写审计日志"
    );

    // (b) m 加入 readers → 删除受理 + 审计日志 (from,key,ts)
    let acl = AclRecord {
        owners: vec![self_root.clone()],
        readers: vec![m_root.clone()],
        epoch: 1,
        updated_at: NOW,
        reset_by: None,
        sig: String::new(),
    };
    s.put(
        &spark_core::sync::orgsync::acl_key(ORG_ID, NAME, VERSION),
        &serde_json::to_string(&acl).unwrap(),
    )
    .unwrap();
    let del_body = build_orgq_write_req(
        ORG_ID,
        &col,
        &[spark_core::sync::orgsync::OrgqWriteRecord {
            key: "k9".to_string(),
            value: serde_json::Value::Null,
        }],
        "req-o2",
    );
    let r = deliver_orgq_req(&mut s, &self_root, &m_key, &m_root, &self_root, del_body);
    let resp = r.orgsync_out[0].body();
    assert_eq!(resp["accepted"], json!(1), "读者删除受理");
    assert_eq!(resp["rejected"], json!(0));
    assert!(s.get(&data_key).unwrap().is_none(), "读者删除落墓碑");
    let audits: Vec<_> = s
        .scan(&ScanOptions::prefix("orgq:audit:"))
        .unwrap()
        .into_iter()
        .collect();
    assert_eq!(audits.len(), 1, "受理删除写审计日志");
    let entry: serde_json::Value = serde_json::from_str(&audits[0].1).unwrap();
    assert_eq!(entry["from"], json!(m_root), "审计记录 from");
    assert_eq!(entry["key"], json!("k9"), "审计记录 key");
}

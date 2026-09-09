//! O3 成员侧在线 orgq-req 投递（kernel 级集成）：真实双 kernel + 真实 P2P，
//! B 成员在线查询/写入 data-accounts 集合经 orgq-req 直发 A 数据账号，同步
//! 等待应答后落缓存/回执。
//!
//! 场景：A 为数据账号（运行真实插件后台运行时注册 onReadFilter/onWriteFilter）；
//! B 为普通成员，持组织记录与集合声明，在线连接 A。B 的 data_query/data_save
//! 走 `data_orgq_query`/`data_orgq_write` 投递链。

use spark_core::kernel::Kernel;
use spark_core::org::OrganizationService;
use spark_core::org::service::CreateOrganizationInput;
use spark_core::plugindata::{Accounts, DeclareInput, Scope, Space};
use spark_core::storage::StorageBackend;
use spark_core::sync::orgsync::orgq_cache_key;

use crate::common::*;

/// Z7：四例真实双 kernel + P2P 投递共享宿主机 P2P 端口/身份 seed，并行跑会
/// 撞资源与 dm 按 from 1s 限流窗口。用**全局互斥锁强制串行**——锁在同一
/// 测试进程内对所有 deliver 用例排他，消除并行不稳（相对 serial_test 依赖
/// 更轻，无需新增 crate）。
static DELIVER_SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// 串行执行守卫：每个 deliver 测试开头调用，持有全局锁至测试结束。
fn serial_guard() -> std::sync::MutexGuard<'static, ()> {
    DELIVER_SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

/// 声明 org data-accounts（filtered）集合的公共输入。
fn data_accounts_declare() -> DeclareInput {
    DeclareInput {
        name: "orgq:deliver".to_string(),
        version: Some("1.0.0".to_string()),
        space: Some(Space::Org),
        accounts: Some(Accounts::DataAccounts),
        scope: Some(Scope::Sync),
        ..Default::default()
    }
}

/// 过滤插件脚本：canRead/canWrite 由 `read`/`write` 布尔决定。
fn filter_plugin(read: bool, write: bool) -> String {
    format!(
        "spark.data.onReadFilter(\"orgq:deliver\", function (m, k) {{ return {read}; }});\n\
         spark.data.onWriteFilter(\"orgq:deliver\", function (m, k, v) {{ return {write}; }});"
    )
}

/// 建双 kernel + A 建组织（A 数据账号，B 普通成员，A 的 nodeInfo 回填）
/// 返回 (kernel_a, kernel_b, org_id, root_a)。
fn setup() -> (Kernel, Kernel, String, String) {
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    let mut kernel_a = fresh_kernel(dir_a.path());
    let mut kernel_b = fresh_kernel(dir_b.path());
    let (root_a, _) = init_identity(&mut kernel_a);
    let (root_b, _) = init_identity(&mut kernel_b);
    kernel_a.start_p2p().unwrap();
    kernel_b.start_p2p().unwrap();

    let view = kernel_a
        .create_org(CreateOrganizationInput {
            name: "orgq 组织".to_string(),
            description: None,
            avatar: None,
            base_plugin_domain: Some("plugin:orgq".to_string()),
            ..Default::default()
        })
        .unwrap();
    let org_id = view.record.org_id.clone();
    let b_node = spark_core::org::OrganizationNodeInfo {
        device_uid: None,
        peer_id: kernel_b.p2p_status().unwrap().unwrap().peer_id,
        addresses: dialable_addrs(&kernel_b),
    };
    kernel_a
        .org_add_member(&org_id, &root_b, Some(&b_node))
        .unwrap();
    //（A14 全员数据节点：A 成员即数据节点，无需指定）
    // 回填 A 自身成员 nodeInfo（B 据此选在线数据节点）
    {
        let mut raw = kernel_a.__test_storage().unwrap();
        let mut record = OrganizationService::get_record(&raw, &org_id)
            .unwrap()
            .unwrap();
        let a_node = spark_core::org::OrganizationNodeInfo {
            device_uid: None,
            peer_id: Some(kernel_a.p2p_status().unwrap().unwrap().peer_id.unwrap()),
            addresses: dialable_addrs(&kernel_a),
        };
        if let Some(m) = record.members.iter_mut().find(|m| m.root_id == root_a) {
            m.node_info = Some(spark_core::org::OrganizationDeviceSet::from_single(a_node));
        }
        // 阶段四A P1-a 双写口径：夹具此处绕过原子段直写，须 whole + 成员
        // 条目同步记账（put_personal 复刻中间件 bump）——只直写 whole 时
        // vv 不推进、whole 更新不随 orgsync 流出，而 create 双写的旧条目
        // （无 nodeInfo）会以「条目权威」在对端装配视图盖住 whole 回填。
        // 生产路径全部走原子段双写，无此分叉。
        let a_entry = record
            .members
            .iter()
            .find(|m| m.root_id == root_a)
            .unwrap()
            .clone();
        let node_id = kernel_a.p2p_status().unwrap().unwrap().peer_id.unwrap();
        let now = spark_core::p2p::node::system_now_ms();
        spark_core::sync::put_personal(
            &mut raw,
            &node_id,
            &spark_core::org::types::organization_key(&org_id),
            &serde_json::to_string(&record).unwrap(),
            now,
        )
        .unwrap();
        spark_core::sync::put_personal(
            &mut raw,
            &node_id,
            &spark_core::org::types::org_member_key(&org_id, &root_a),
            &serde_json::to_string(&a_entry).unwrap(),
            now,
        )
        .unwrap();
    }
    (kernel_a, kernel_b, org_id, root_a)
}

/// 让 B 加入组织 + 声明同一集合 + 等待 A/B 连接就绪。
///
/// 阶段四A P3：org-pull 回拉出站停发——预录成员的组织到达改走**邀请流**
/// （A 发 DM 邀请 → B 应答 accept 走 P2 join：stub 自举 + orgsync 收敛）。
fn connect_member(kernel_a: &mut Kernel, kernel_b: &mut Kernel, org_id: &str) {
    let root_b = kernel_b.current_root_id().unwrap().unwrap();
    kernel_a
        .org_send_invite(
            org_id,
            &root_b,
            kernel_b.p2p_status().unwrap().unwrap().peer_id.as_deref(),
            &dialable_addrs(kernel_b),
            None,
        )
        .unwrap();
    // 等 B 收到入站邀请记录（dm 投递 + 入站落库）
    wait_until(
        || {
            kernel_b
                .org_invite_records(org_id)
                .map(|rs| {
                    rs.iter()
                        .any(|r| r.direction == spark_core::org::OrgInviteDirection::Incoming)
                })
                .unwrap_or(false)
        },
        15_000,
        "B 收到 org-invite",
    );
    let invite = kernel_b
        .org_invite_records(org_id)
        .unwrap()
        .into_iter()
        .find(|r| r.direction == spark_core::org::OrgInviteDirection::Incoming)
        .expect("入站邀请已落库");
    // accept → P2 join（stub + 即时 hello → 收敛），返回即已加入
    kernel_b.org_respond_invite(&invite.id, true).unwrap();
    kernel_b
        .data_declare_collection("plugin:orgq", data_accounts_declare(), Some(org_id))
        .unwrap();
    let a_peer = kernel_a.p2p_status().unwrap().unwrap().peer_id.unwrap();
    wait_until(
        || {
            kernel_b
                .p2p_status()
                .unwrap()
                .unwrap()
                .connected_peers
                .iter()
                .any(|p| *p == a_peer)
        },
        15_000,
        "B 连接 A",
    );
}

/// B 在线查询：A 数据账号驻留记录 + 插件放行 → B 收到并落缓存。
#[test]
/// A14 全员数据节点：成员读 data-accounts 集合不经 orgq 在线投递——A 写入
/// 后随复制组（全体成员）orgsync 收敛到 B 本地，B 本地直读（无成员侧缓存）。
#[test]
fn member_query_converges_via_replication_group() {
    let _serial = serial_guard();
    let (mut kernel_a, mut kernel_b, org_id, _root_a) = setup();
    kernel_a
        .data_declare_collection("plugin:orgq", data_accounts_declare(), Some(&org_id))
        .unwrap();
    // A 写入驻留记录（A 成员即数据节点 → 本地落库）
    kernel_a
        .data_save(
            "plugin:orgq",
            "orgq:deliver",
            "k1",
            serde_json::json!({"amt": 42}),
            Some("1.0.0"),
            Some(&org_id),
        )
        .unwrap();
    connect_member(&mut kernel_a, &mut kernel_b, &org_id);

    // 复制组（全体成员）收敛：k1 到达 B 本地 orgd（测试配置无周期 tick，
    // 手动泵 orgsync hello：B hello → A 服务 diff）
    let data_key = format!("orgd:{org_id}:orgq:deliver@v1.0.0:k1");
    wait_until(
        || {
            let _ = kernel_b.org_keepalive_once();
            kernel_b
                .__test_storage()
                .unwrap()
                .get(&data_key)
                .unwrap()
                .is_some()
        },
        10_000,
        "B 本地收敛 k1",
    );
    let page = kernel_b
        .data_query(
            "plugin:orgq",
            "orgq:deliver",
            None,
            Some(10),
            None,
            Some("1.0.0"),
            Some(&org_id),
        )
        .unwrap();
    assert_eq!(page.items.len(), 1, "B 本地直读收敛的记录");
    assert!(page.items[0].1.contains("\"amt\":42"), "记录内容正确");
    let cache = kernel_b
        .__test_storage()
        .unwrap()
        .get(&orgq_cache_key(&org_id, "orgq:deliver@v1.0.0", "k1"))
        .unwrap();
    assert!(cache.is_none(), "不经 orgq 在线投递，无成员侧缓存");

    kernel_a.shutdown().unwrap();
    kernel_b.shutdown().unwrap();
}

/// B 在线查询：A 的 canRead 钩子拒绝 → B 收到空集（非读者连元数据都不给）。
#[test]
fn member_online_query_hook_reject_returns_empty() {
    let _serial = serial_guard();
    let (mut kernel_a, mut kernel_b, org_id, _root_a) = setup();
    kernel_a
        .data_declare_collection("plugin:orgq", data_accounts_declare(), Some(&org_id))
        .unwrap();
    kernel_a
        .data_save(
            "plugin:orgq",
            "orgq:deliver",
            "k1",
            serde_json::json!({"amt": 42}),
            Some("1.0.0"),
            Some(&org_id),
        )
        .unwrap();
    // canRead 拒绝
    kernel_a
        .plugin_start_background(
            "orgq",
            &filter_plugin(false, false),
            &["data:write".to_string()],
        )
        .unwrap();
    connect_member(&mut kernel_a, &mut kernel_b, &org_id);

    // F9-1 对照段：A 是数据账号 → 本地直读 k1 应命中（数据确已驻留）；B 经
    // orgq 查询却被 canRead 拒绝 → 空集。对照说明 B 空集是权限拒绝而非数据
    // 不存在。
    let local = kernel_a
        .data_get(
            "plugin:orgq",
            "orgq:deliver",
            "k1",
            Some("1.0.0"),
            Some(&org_id),
        )
        .unwrap();
    assert!(
        local.is_some(),
        "对照：A 数据账号本地直读命中 k1（数据驻留）"
    );

    let page = kernel_b
        .data_query(
            "plugin:orgq",
            "orgq:deliver",
            None,
            Some(10),
            None,
            Some("1.0.0"),
            Some(&org_id),
        )
        .unwrap();
    assert_eq!(page.items.len(), 0, "钩子拒绝 → 空集（元数据不给）");

    kernel_a.shutdown().unwrap();
    kernel_b.shutdown().unwrap();
}

/// A14 全员数据节点：B 写 data-accounts 集合 → 本地落库（B 成员即数据
/// 节点），随复制组（全体成员）orgsync 收敛到 A。
#[test]
fn member_write_local_converges_to_a() {
    let _serial = serial_guard();
    let (mut kernel_a, mut kernel_b, org_id, _root_a) = setup();
    kernel_a
        .data_declare_collection("plugin:orgq", data_accounts_declare(), Some(&org_id))
        .unwrap();
    connect_member(&mut kernel_a, &mut kernel_b, &org_id);
    // B 本地持有声明后写入（声明随 orgsync 收敛；同名声明幂等）
    kernel_b
        .data_declare_collection("plugin:orgq", data_accounts_declare(), Some(&org_id))
        .unwrap();
    // B 写入 → 本地驻留落库（成员即数据节点，不经 orgq 在线投递）
    kernel_b
        .data_save(
            "plugin:orgq",
            "orgq:deliver",
            "k9",
            serde_json::json!({"who": "b"}),
            Some("1.0.0"),
            Some(&org_id),
        )
        .unwrap();
    let b_key = format!("orgd:{org_id}:orgq:deliver@v1.0.0:k9");
    assert!(
        kernel_b
            .__test_storage()
            .unwrap()
            .get(&b_key)
            .unwrap()
            .is_some(),
        "B 本地落库"
    );
    // 复制组收敛：A 侧落库（无周期 tick，手动泵 orgsync：A hello → B 服务 diff）
    let data_key = format!("orgd:{org_id}:orgq:deliver@v1.0.0:k9");
    wait_until(
        || {
            let _ = kernel_a.org_keepalive_once();
            kernel_a
                .__test_storage()
                .unwrap()
                .get(&data_key)
                .unwrap()
                .is_some()
        },
        10_000,
        "A 侧收敛落库",
    );
    let raw = kernel_a
        .__test_storage()
        .unwrap()
        .get(&data_key)
        .unwrap()
        .unwrap();
    assert!(raw.contains("\"b\""), "A 侧记录 = B 写入值");

    kernel_a.shutdown().unwrap();
    kernel_b.shutdown().unwrap();
}

//（A14：成员写拒绝场景随「数据账号侧 canWrite 投递」退役——写方本地直通
// 无对端钩子；filtered 写钩子执行点变为写方本地（插件 data.save 规则集，
// 由 host env 既有用例覆盖），城门接线归 A15 复查。）

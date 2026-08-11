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
        })
        .unwrap();
    let org_id = view.record.org_id.clone();
    let b_node = spark_core::org::OrganizationNodeInfo {
        device_uid: None,
        peer_id: kernel_b.p2p_status().unwrap().unwrap().peer_id,
        addresses: dialable_addrs(&kernel_b),
    };
    kernel_a.org_add_member(&org_id, &root_b, Some(&b_node)).unwrap();
    kernel_a
        .org_set_data_accounts(&org_id, &[root_a.clone()])
        .unwrap();
    // 回填 A 自身成员 nodeInfo（B 据此选在线数据账号）
    {
        let mut raw = kernel_a.__test_storage().unwrap();
        let mut record = OrganizationService::get_record(&raw, &org_id).unwrap().unwrap();
        let a_node = spark_core::org::OrganizationNodeInfo {
            device_uid: None,
            peer_id: Some(kernel_a.p2p_status().unwrap().unwrap().peer_id.unwrap()),
            addresses: dialable_addrs(&kernel_a),
        };
        if let Some(m) = record.members.iter_mut().find(|m| m.root_id == root_a) {
            m.node_info = Some(spark_core::org::OrganizationDeviceSet::from_single(a_node));
        }
        OrganizationService::save_record(&mut raw, &record).unwrap();
    }
    (kernel_a, kernel_b, org_id, root_a)
}

/// 让 B 拉取组织 + 声明同一集合 + 等待 A/B 连接就绪。
fn connect_member(kernel_a: &Kernel, kernel_b: &mut Kernel, org_id: &str) {
    let a_node = spark_core::org::OrganizationNodeInfo {
        device_uid: None,
        peer_id: kernel_a.p2p_status().unwrap().unwrap().peer_id,
        addresses: dialable_addrs(kernel_a),
    };
    let result = kernel_b.sync_peer_organizations(&a_node).unwrap();
    assert!(result.pull_checked >= 1, "B 拉到组织");
    kernel_b
        .data_declare_collection("plugin:orgq", data_accounts_declare(), Some(org_id))
        .unwrap();
    let a_peer = kernel_a.p2p_status().unwrap().unwrap().peer_id.unwrap();
    wait_until(
        || kernel_b.p2p_status().unwrap().unwrap().connected_peers.iter().any(|p| *p == a_peer),
        15_000,
        "B 连接 A",
    );
}

/// B 在线查询：A 数据账号驻留记录 + 插件放行 → B 收到并落缓存。
#[test]
fn member_online_query_delivers_and_caches() {
    let _serial = serial_guard();
    let (mut kernel_a, mut kernel_b, org_id, _root_a) = setup();
    kernel_a
        .data_declare_collection("plugin:orgq", data_accounts_declare(), Some(&org_id))
        .unwrap();
    // A 写入驻留记录（A 是数据账号 → 本地落库）
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
    kernel_a
        .plugin_start_background("orgq", &filter_plugin(true, false), &["data:write".to_string()])
        .unwrap();
    connect_member(&kernel_a, &mut kernel_b, &org_id);

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
    assert_eq!(page.items.len(), 1, "B 收到 A 驻留的记录");
    assert!(page.items[0].1.contains("\"amt\":42"), "记录内容正确");
    let cache = kernel_b
        .__test_storage()
        .unwrap()
        .get(&orgq_cache_key(&org_id, "orgq:deliver@v1.0.0", "k1"))
        .unwrap();
    assert!(cache.is_some(), "应答已落成员侧缓存");

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
        .plugin_start_background("orgq", &filter_plugin(false, false), &["data:write".to_string()])
        .unwrap();
    connect_member(&kernel_a, &mut kernel_b, &org_id);

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
    assert!(local.is_some(), "对照：A 数据账号本地直读命中 k1（数据驻留）");

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

/// B 在线写入：data_save → orgq-req 写 → A 的 canWrite 放行 → accepted 落库，
/// 随复制组（orgd:）扩散。denied 写入映射 AccessDenied。
#[test]
fn member_online_write_accepted_lands_on_a() {
    let _serial = serial_guard();
    let (mut kernel_a, mut kernel_b, org_id, _root_a) = setup();
    kernel_a
        .data_declare_collection("plugin:orgq", data_accounts_declare(), Some(&org_id))
        .unwrap();
    // canWrite 放行
    kernel_a
        .plugin_start_background("orgq", &filter_plugin(true, true), &["data:write".to_string()])
        .unwrap();
    connect_member(&kernel_a, &mut kernel_b, &org_id);

    // B 在线写入 → 受理 → 成功返回
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
    // A 侧已落库（orgd: 数据键，随复制组扩散）
    let data_key = format!("orgd:{org_id}:orgq:deliver@v1.0.0:k9");
    wait_until(
        || kernel_a.__test_storage().unwrap().get(&data_key).unwrap().is_some(),
        10_000,
        "A 侧落库",
    );
    let raw = kernel_a.__test_storage().unwrap().get(&data_key).unwrap().unwrap();
    assert!(raw.contains("\"b\""), "A 侧记录 = B 写入值");

    kernel_a.shutdown().unwrap();
    kernel_b.shutdown().unwrap();
}

/// B 在线写入被 denied（encrypted 占位 / 插件未运行降级）→ 映射 AccessDenied。
#[test]
fn member_online_write_denied_maps_access_denied() {
    let _serial = serial_guard();
    let (mut kernel_a, mut kernel_b, org_id, _root_a) = setup();
    kernel_a
        .data_declare_collection("plugin:orgq", data_accounts_declare(), Some(&org_id))
        .unwrap();
    // canWrite 拒绝 → denied=true
    kernel_a
        .plugin_start_background("orgq", &filter_plugin(true, false), &["data:write".to_string()])
        .unwrap();
    connect_member(&kernel_a, &mut kernel_b, &org_id);

    let err = kernel_b
        .data_save(
            "plugin:orgq",
            "orgq:deliver",
            "k9",
            serde_json::json!({"who": "b"}),
            Some("1.0.0"),
            Some(&org_id),
        )
        .unwrap_err();
    assert!(
        err.to_string().contains("denied"),
        "denied 写入映射 AccessDenied（got: {err}）"
    );
    // A 侧未落库
    let data_key = format!("orgd:{org_id}:orgq:deliver@v1.0.0:k9");
    assert!(
        kernel_a.__test_storage().unwrap().get(&data_key).unwrap().is_none(),
        "被拒写入不落库"
    );

    kernel_a.shutdown().unwrap();
    kernel_b.shutdown().unwrap();
}

// ═════════════════════════════════════════════════════════════════════════
// O4 orgkey-deliver 端到端：grant → 出站投递 → B 入站解包落表 → 可解密
// ═════════════════════════════════════════════════════════════════════════

/// O4 encrypted 集合声明（encrypted + data-accounts）。
fn encrypted_declare() -> DeclareInput {
    use spark_core::plugindata::Confidentiality;
    DeclareInput {
        name: "enc:secret".to_string(),
        version: Some("1.0.0".to_string()),
        space: Some(Space::Org),
        accounts: Some(Accounts::DataAccounts),
        confidentiality: Some(Confidentiality::Encrypted),
        scope: Some(Scope::Sync),
        ..Default::default()
    }
}

/// O4 双节点端到端：A（owner/数据账号）grant B（reader）→ A 生成 epoch-1
/// 密钥并 orgkey-deliver 投递给 B → B 入站解包落 orgkey 表 → A 加密落 orgd
/// 密文 → orgsync 复制到 B → B data_get 解密返回明文（插件读到明文、orgd 只
/// 有密文）。
#[test]
fn orgkey_deliver_reaches_reader_and_enables_decrypt() {
    let _serial = serial_guard();
    use spark_core::sync::orgsync;
    // A 建组织（A 数据账号，B 成员，B nodeInfo 回填）
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
            name: "enc 组织".to_string(),
            description: None,
            avatar: None,
            base_plugin_domain: Some("plugin:enc".to_string()),
        })
        .unwrap();
    let org_id = view.record.org_id.clone();
    let b_node = spark_core::org::OrganizationNodeInfo {
        device_uid: None,
        peer_id: kernel_b.p2p_status().unwrap().unwrap().peer_id,
        addresses: dialable_addrs(&kernel_b),
    };
    kernel_a.org_add_member(&org_id, &root_b, Some(&b_node)).unwrap();
    kernel_a
        .org_set_data_accounts(&org_id, &[root_a.clone()])
        .unwrap();
    // 回填 A 自身 nodeInfo（B 据此选在线数据账号）
    {
        let mut raw = kernel_a.__test_storage().unwrap();
        let mut record = OrganizationService::get_record(&raw, &org_id).unwrap().unwrap();
        let a_node = spark_core::org::OrganizationNodeInfo {
            device_uid: None,
            peer_id: Some(kernel_a.p2p_status().unwrap().unwrap().peer_id.unwrap()),
            addresses: dialable_addrs(&kernel_a),
        };
        if let Some(m) = record.members.iter_mut().find(|m| m.root_id == root_a) {
            m.node_info = Some(spark_core::org::OrganizationDeviceSet::from_single(a_node));
        }
        OrganizationService::save_record(&mut raw, &record).unwrap();
    }
    // A 声明 encrypted 集合
    kernel_a
        .data_declare_collection("plugin:enc", encrypted_declare(), Some(&org_id))
        .unwrap();
    // B 拉组织 + 声明同一集合 + 连接 A
    {
        let a_node = spark_core::org::OrganizationNodeInfo {
            device_uid: None,
            peer_id: kernel_a.p2p_status().unwrap().unwrap().peer_id,
            addresses: dialable_addrs(&kernel_a),
        };
        let result = kernel_b.sync_peer_organizations(&a_node).unwrap();
        assert!(result.pull_checked >= 1, "B 拉到组织");
        kernel_b
            .data_declare_collection("plugin:enc", encrypted_declare(), Some(&org_id))
            .unwrap();
        let a_peer = kernel_a.p2p_status().unwrap().unwrap().peer_id.unwrap();
        wait_until(
            || kernel_b.p2p_status().unwrap().unwrap().connected_peers.iter().any(|p| *p == a_peer),
            15_000,
            "B 连接 A",
        );
    }
    // B 发布本人组织身份访问密钥（accessKey）→ org:structure 全员传播。
    // 测试直接把手动把 B 的 accessKey 回填到 A 的本地 org 记录（模拟 orgsync
    // 反熵已收敛），供 A 出站 orgkey-deliver 取 B 公钥 box。
    kernel_b.org_publish_access_key(&org_id).unwrap();
    {
        let b_raw = kernel_b.__test_storage().unwrap();
        let b_record = OrganizationService::get_record(&b_raw, &org_id).unwrap().unwrap();
        let b_access_key = b_record.find_member(&root_b).and_then(|m| m.access_key.clone());
        let mut a_raw = kernel_a.__test_storage().unwrap();
        let mut a_record = OrganizationService::get_record(&a_raw, &org_id).unwrap().unwrap();
        if let Some(m) = a_record.members.iter_mut().find(|m| m.root_id == root_b) {
            m.access_key = b_access_key;
        }
        OrganizationService::save_record(&mut a_raw, &a_record).unwrap();
    }
    // A 侧确认收到 B 的 accessKey（供出站投递用）
    assert!(
        {
            let raw = kernel_a.__test_storage().unwrap();
            let record = OrganizationService::get_record(&raw, &org_id).unwrap().unwrap();
            record.find_member(&root_b).and_then(|m| m.access_key.as_ref()).is_some()
        },
        "A 侧已持有 B 的 accessKey"
    );
    // 反向：A（owner）也要发布 accessKey——B 入站验 orgkey-deliver 签名需
    // sender（A）的 accessKey 公钥。测试手动把 A 的 accessKey 回填到 B 记录。
    kernel_a.org_publish_access_key(&org_id).unwrap();
    {
        let a_raw = kernel_a.__test_storage().unwrap();
        let a_record = OrganizationService::get_record(&a_raw, &org_id).unwrap().unwrap();
        let a_access_key = a_record.find_member(&root_a).and_then(|m| m.access_key.clone());
        let mut b_raw = kernel_b.__test_storage().unwrap();
        let mut b_record = OrganizationService::get_record(&b_raw, &org_id).unwrap().unwrap();
        if let Some(m) = b_record.members.iter_mut().find(|m| m.root_id == root_a) {
            m.access_key = a_access_key;
        }
        OrganizationService::save_record(&mut b_raw, &b_record).unwrap();
    }
    // A grant B → 生成 epoch-1 密钥 + 出站 orgkey-deliver 投递给 B
    kernel_a
        .data_grant_access(&org_id, "enc:secret", "1.0.0", &[root_b.clone()])
        .unwrap();
    // A 侧 epoch-1 密钥已生成（grant 创世）
    assert!(
        orgsync::get_epoch_key(
            &kernel_a.__test_storage().unwrap(),
            &org_id,
            "enc:secret",
            "1.0.0",
            1,
        )
        .is_some(),
        "A 侧 grant 生成 epoch-1 密钥"
    );
    // 模拟 orgsync 反熵：把 A 的 acl（all-members 系统数据）同步到 B——B 入站
    // orgkey-deliver 验 sender ∈ 当前 acl owners 需要本地已有该 acl（真实
    // 操作中 acl 先随 orgsync 全员同步、orgkey-deliver 定向投递后达）。deliver
    // 带 2s/5s 退避重试，acl 同步先于重试到达即可。
    {
        let a_raw = kernel_a.__test_storage().unwrap();
        let acl_key = orgsync::acl_key(&org_id, "enc:secret", "1.0.0");
        let acl_raw = a_raw.get(&acl_key).unwrap().unwrap();
        let mut b_raw = kernel_b.__test_storage().unwrap();
        b_raw.put(&acl_key, &acl_raw).unwrap();
    }
    // R1 真实 dm 投递端到端：A grant 后出站 orgkey-deliver（**dm 信封以根私钥
    // 签名**，from==rootId 绑定）经真实 p2p 投递到 B；B 入站 verify_envelope
    // 验根公钥→from 绑定 → 验 owner → 验 body org-access 域签名 → 解 box 落
    // orgkey 表。此处**不做手工铺底**——B 的 epoch-1 密钥必须由真实 dm 投递
    // 到达（投递带 2s/5s 退避重试，acl 已先同步）。
    wait_until(
        || {
            orgsync::get_epoch_key(
                &kernel_b.__test_storage().unwrap(),
                &org_id,
                "enc:secret",
                "1.0.0",
                1,
            )
            .is_some()
        },
        15_000,
        "B 经真实 dm 投递收到 epoch-1 密钥（orgkey-deliver 根签名信封到达）",
    );
    // A 加密落 orgd（密文），orgsync 复制到 B，B 解密读到明文
    kernel_a
        .data_save(
            "plugin:enc",
            "enc:secret",
            "k1",
            serde_json::json!({"amount": 999}),
            Some("1.0.0"),
            Some(&org_id),
        )
        .unwrap();
    let data_key = format!("orgd:{org_id}:enc:secret@v1.0.0:k1");
    // A 侧密文
    let stored = kernel_a.__test_storage().unwrap().get(&data_key).unwrap().unwrap();
    assert!(!stored.contains("999"), "A 侧 orgd 为密文（无明文）");
    // 审计测试：orgsync-data 采集 encrypted 集合增量 → 记录 value 为密文
    // `{epoch,nonce,ct}`，无明文——复制组流量只有密文（org-orgsync §20.4）。
    let records = spark_core::sync::orgsync::collect_org_incremental(
        &kernel_a.__test_storage().unwrap(),
        &org_id,
        "enc:secret",
        "1.0.0",
        &Default::default(),
        0,
    )
    .unwrap();
    assert!(
        records
            .iter()
            .filter(|r| r.key == data_key)
            .any(|r| {
                let v = r.value.to_string();
                v.contains("\"epoch\"") && v.contains("\"nonce\"") && v.contains("\"ct\"")
                    && !v.contains("999")
            }),
        "orgsync-data 中 encrypted 集合 value 为密文、无明文"
    );
    // B（普通成员）读明文：B 非数据账号，encrypted 集合读经 orgq 在线查询 A，
    // A 返回密文 → B 本地解密为明文（插件读到明文）。B 持有 epoch-1 密钥。
    let got = kernel_b
        .data_get(
            "plugin:enc",
            "enc:secret",
            "k1",
            Some("1.0.0"),
            Some(&org_id),
        )
        .unwrap()
        .expect("B 读到明文");
    assert_eq!(got["amount"], serde_json::json!(999), "B 解密读到明文");

    kernel_a.shutdown().unwrap();
    kernel_b.shutdown().unwrap();
}

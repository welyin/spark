//! 安装 / 升级 / 启停用例。

use super::*;

#[test]
fn install_from_local_release_copies_package_and_persists() {
    let fixture = Fixture::new();
    write_release(&fixture, &ReleaseOpts::default());
    // 不调 initialize：显式 install 路径（reconcile 已在其他用例覆盖，
    // 若先 initialize，本地 bundle 会被对账直接标记安装）
    let mut service = fixture.service();
    assert!(!service.state.installed.contains_key("spark-example"));

    let installed = service.install("spark-example").unwrap();
    assert_eq!(installed.version, "0.1.0");
    assert!(installed.enabled);
    // 包被复制到 packages_root/<id>/packages/（跨平台：按路径组件比较，不比较字符串分隔符）
    let copied = fixture
        .packages_root
        .join("spark-example")
        .join("packages")
        .join("spark-plugin-spark-example-0.1.0.spkg");
    assert_eq!(PathBuf::from(&installed.package_path), copied);
    assert!(copied.is_file());
    assert_eq!(service.update_probes["spark-example"].reason, "installed");

    // 新实例从状态文件恢复（持久化语义）；reconcile 跳过已安装条目
    let mut reloaded = fixture.service();
    reloaded.initialize().unwrap();
    assert!(reloaded.state.installed.contains_key("spark-example"));
    assert!(reloaded.list_market()[0].installed);
}

#[test]
fn install_normalizes_manifest_permissions() {
    let fixture = Fixture::new();
    write_release(
        &fixture,
        &ReleaseOpts {
            permissions: Some(vec![
                "org:sync".to_string(),
                "bogus".to_string(),
                "identity:sign".to_string(),
                "org:sync".to_string(),
            ]),
            ..Default::default()
        },
    );
    let mut service = fixture.service();
    service.initialize().unwrap();
    let installed = service.install("spark-example").unwrap();
    assert_eq!(
        installed.granted_permissions,
        vec![
            "storage:read",
            "storage:write",
            "org:read",
            "proof:verify",
            "org:sync",
            "identity:sign"
        ]
    );
}

#[test]
fn install_rejects_signature_id_domain_and_digest_problems() {
    // 坏签名
    let fixture = Fixture::new();
    write_release(&fixture, &ReleaseOpts { bad_signature: true, ..Default::default() });
    let mut service = fixture.service();
    service.initialize().unwrap();
    assert_eq!(
        service.install("spark-example").unwrap_err(),
        "Plugin manifest signature verification failed: spark-example"
    );

    // id 不匹配
    let fixture = Fixture::new();
    write_release(
        &fixture,
        &ReleaseOpts { plugin_id: "evil".to_string(), ..Default::default() },
    );
    let mut service = fixture.service();
    service.initialize().unwrap();
    assert_eq!(
        service.install("spark-example").unwrap_err(),
        "Plugin manifest id mismatch: expected spark-example, got evil"
    );

    // domain 不匹配
    let fixture = Fixture::new();
    write_release(
        &fixture,
        &ReleaseOpts { domain: "plugin:evil".to_string(), ..Default::default() },
    );
    let mut service = fixture.service();
    service.initialize().unwrap();
    assert_eq!(
        service.install("spark-example").unwrap_err(),
        "Plugin manifest domain mismatch: expected plugin:spark-example, got plugin:evil"
    );

    // sha256 不匹配
    let fixture = Fixture::new();
    write_release(&fixture, &ReleaseOpts { tamper_sha256: true, ..Default::default() });
    let mut service = fixture.service();
    service.initialize().unwrap();
    assert_eq!(
        service.install("spark-example").unwrap_err(),
        "Plugin package sha256 mismatch for spark-example"
    );
    assert!(!service.state.installed.contains_key("spark-example"));

    // size 不匹配
    let fixture = Fixture::new();
    write_release(&fixture, &ReleaseOpts { tamper_size: true, ..Default::default() });
    let mut service = fixture.service();
    service.initialize().unwrap();
    assert_eq!(
        service.install("spark-example").unwrap_err(),
        "Plugin package size mismatch for spark-example"
    );

    // 未收录插件
    let fixture = Fixture::new();
    let mut service = fixture.service();
    service.initialize().unwrap();
    assert_eq!(
        service.install("nope").unwrap_err(),
        "Plugin not found: nope"
    );
}

/// B1：清单资产 fileName 消毒——拒绝穿越/绝对路径/多段文件名，
/// 防任意路径写盘与跨插件覆盖提权（安装入口即拒，不落任何状态）。
#[test]
fn install_rejects_unsafe_asset_file_name() {
    for bad_name in ["../evil.spkg", "..\\evil.spkg", "C:\\evil.spkg", "a/b.spkg", "/abs.spkg"] {
        let fixture = Fixture::new();
        write_release(
            &fixture,
            &ReleaseOpts {
                file_name: Some(bad_name.to_string()),
                ..Default::default()
            },
        );
        // 不调 initialize：避免对账路径先行消费清单，直测 install 入口
        let mut service = fixture.service();
        assert_eq!(
            service.install("spark-example").unwrap_err(),
            format!("Plugin asset file name invalid: {bad_name}")
        );
        assert!(!service.state.installed.contains_key("spark-example"));
    }
}

#[test]
fn set_enabled_roundtrip_and_upgrade_flow() {
    let fixture = Fixture::new();
    write_release(&fixture, &ReleaseOpts::default());
    // 不调 initialize：先验证"未安装不能启停/升级"，再走显式 install
    let mut service = fixture.service();

    // 未安装不能启停/升级
    assert_eq!(
        service.set_enabled("spark-example", false).unwrap_err(),
        "Plugin is not installed: spark-example"
    );
    assert_eq!(
        service.upgrade("spark-example").unwrap_err(),
        "Plugin is not installed: spark-example"
    );

    service.install("spark-example").unwrap();
    let disabled = service.set_enabled("spark-example", false).unwrap();
    assert!(!disabled.enabled);
    let mut reloaded = fixture.service();
    reloaded.initialize().unwrap();
    assert!(!reloaded.state.installed["spark-example"].enabled);
    assert!(!reloaded.list_market()[0].enabled);

    // 发布 0.2.0 后升级
    write_release(&fixture, &ReleaseOpts { version: "0.2.0".to_string(), ..Default::default() });
    let probes = reloaded.check_for_updates(Some("spark-example")).unwrap();
    assert!(probes[0].update_available);
    assert_eq!(probes[0].reason, "new-version-available");
    assert_eq!(probes[0].latest_version.as_deref(), Some("0.2.0"));

    let upgraded = reloaded.upgrade("spark-example").unwrap();
    assert_eq!(upgraded.version, "0.2.0");
    assert_eq!(reloaded.update_probes["spark-example"].reason, "upgraded");
    assert!(fixture
        .packages_root
        .join("spark-example/packages/spark-plugin-spark-example-0.2.0.spkg")
        .is_file());
}

//! 安装 / 升级 / 启停用例。

use super::*;

#[test]
fn install_from_local_release_copies_package_and_persists() {
    let fixture = Fixture::new();
    write_release(&fixture, &ReleaseOpts::default());
    // 不调 initialize：显式 install 路径（reconcile 已在其他用例覆盖，
    // 若先 initialize，本地 bundle 会被对账直接标记安装）
    let mut service = fixture.service();
    assert!(!service.state.installed.contains_key("weibo-core"));

    let installed = service.install("weibo-core").unwrap();
    assert_eq!(installed.version, "0.1.0");
    assert!(installed.enabled);
    // 包被复制到 packages_root/<id>/packages/
    let copied = fixture
        .packages_root
        .join("weibo-core/packages/spark-plugin-weibo-core-0.1.0.spkg");
    assert_eq!(installed.package_path, copied.to_string_lossy());
    assert!(copied.is_file());
    assert_eq!(service.update_probes["weibo-core"].reason, "installed");

    // 新实例从状态文件恢复（持久化语义）；reconcile 跳过已安装条目
    let mut reloaded = fixture.service();
    reloaded.initialize().unwrap();
    assert!(reloaded.state.installed.contains_key("weibo-core"));
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
    let installed = service.install("weibo-core").unwrap();
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
        service.install("weibo-core").unwrap_err(),
        "Plugin manifest signature verification failed: weibo-core"
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
        service.install("weibo-core").unwrap_err(),
        "Plugin manifest id mismatch: expected weibo-core, got evil"
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
        service.install("weibo-core").unwrap_err(),
        "Plugin manifest domain mismatch: expected plugin:weibo-core, got plugin:evil"
    );

    // sha256 不匹配
    let fixture = Fixture::new();
    write_release(&fixture, &ReleaseOpts { tamper_sha256: true, ..Default::default() });
    let mut service = fixture.service();
    service.initialize().unwrap();
    assert_eq!(
        service.install("weibo-core").unwrap_err(),
        "Plugin package sha256 mismatch for weibo-core"
    );
    assert!(!service.state.installed.contains_key("weibo-core"));

    // size 不匹配
    let fixture = Fixture::new();
    write_release(&fixture, &ReleaseOpts { tamper_size: true, ..Default::default() });
    let mut service = fixture.service();
    service.initialize().unwrap();
    assert_eq!(
        service.install("weibo-core").unwrap_err(),
        "Plugin package size mismatch for weibo-core"
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

#[test]
fn set_enabled_roundtrip_and_upgrade_flow() {
    let fixture = Fixture::new();
    write_release(&fixture, &ReleaseOpts::default());
    // 不调 initialize：先验证"未安装不能启停/升级"，再走显式 install
    let mut service = fixture.service();

    // 未安装不能启停/升级
    assert_eq!(
        service.set_enabled("weibo-core", false).unwrap_err(),
        "Plugin is not installed: weibo-core"
    );
    assert_eq!(
        service.upgrade("weibo-core").unwrap_err(),
        "Plugin is not installed: weibo-core"
    );

    service.install("weibo-core").unwrap();
    let disabled = service.set_enabled("weibo-core", false).unwrap();
    assert!(!disabled.enabled);
    let mut reloaded = fixture.service();
    reloaded.initialize().unwrap();
    assert!(!reloaded.state.installed["weibo-core"].enabled);
    assert!(!reloaded.list_market()[0].enabled);

    // 发布 0.2.0 后升级
    write_release(&fixture, &ReleaseOpts { version: "0.2.0".to_string(), ..Default::default() });
    let probes = reloaded.check_for_updates(Some("weibo-core")).unwrap();
    assert!(probes[0].update_available);
    assert_eq!(probes[0].reason, "new-version-available");
    assert_eq!(probes[0].latest_version.as_deref(), Some("0.2.0"));

    let upgraded = reloaded.upgrade("weibo-core").unwrap();
    assert_eq!(upgraded.version, "0.2.0");
    assert_eq!(reloaded.update_probes["weibo-core"].reason, "upgraded");
    assert!(fixture
        .packages_root
        .join("weibo-core/packages/spark-plugin-weibo-core-0.2.0.spkg")
        .is_file());
}

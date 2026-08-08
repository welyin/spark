//! 更新探测与来源安全（http:// 拒读）用例。

use super::*;

#[test]
fn check_updates_version_compare_and_failure_reasons() {
    // 已装 0.1.0，远端同版 → up-to-date
    // 断言按插件 id 定位（目录含 ai-chat 等多条目，不做位置假设）
    let fixture = Fixture::new();
    write_release(&fixture, &ReleaseOpts::default());
    let mut service = fixture.service();
    service.initialize().unwrap(); // reconcile 已安装 0.1.0
    let probes = service.check_for_updates(None).unwrap();
    let probe = probes
        .iter()
        .find(|p| p.plugin_id == "spark-example")
        .expect("spark-example probe");
    assert!(!probe.update_available);
    assert_eq!(probe.reason, "up-to-date");

    // 未知插件 → Plugin not found
    assert_eq!(
        service.check_for_updates(Some("nope")).unwrap_err(),
        "Plugin not found: nope"
    );

    // 清单缺失 → check-failed（不中断、latestVersion 置空）
    let fixture = Fixture::new();
    let mut service = fixture.service();
    service.initialize().unwrap();
    let probes = service.check_for_updates(None).unwrap();
    let probe = probes
        .iter()
        .find(|p| p.plugin_id == "spark-example")
        .expect("spark-example probe");
    assert!(!probe.update_available);
    assert!(probe.latest_version.is_none());
    assert!(probe.reason.starts_with("check-failed:"));
    // 失败原因进入列表展示
    let example = service
        .list_market()
        .into_iter()
        .find(|item| item.catalog.id == "spark-example")
        .expect("spark-example market item");
    assert!(example.last_check_reason.starts_with("check-failed:"));
}

#[test]
fn http_manifest_url_is_rejected() {
    assert_eq!(
        fetch_text_smart("http://example.com/update-manifest.json").unwrap_err(),
        "Insecure plugin manifest URL is not allowed"
    );
}

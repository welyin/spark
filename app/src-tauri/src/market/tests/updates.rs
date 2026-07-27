//! 更新探测与来源安全（http:// 拒读）用例。

use super::*;

#[test]
fn check_updates_version_compare_and_failure_reasons() {
    // 已装 0.1.0，远端同版 → up-to-date
    let fixture = Fixture::new();
    write_release(&fixture, &ReleaseOpts::default());
    let mut service = fixture.service();
    service.initialize().unwrap(); // reconcile 已安装 0.1.0
    let probes = service.check_for_updates(None).unwrap();
    assert_eq!(probes.len(), 1);
    assert!(!probes[0].update_available);
    assert_eq!(probes[0].reason, "up-to-date");

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
    assert!(!probes[0].update_available);
    assert!(probes[0].latest_version.is_none());
    assert!(probes[0].reason.starts_with("check-failed:"));
    // 失败原因进入列表展示
    assert!(service.list_market()[0].last_check_reason.starts_with("check-failed:"));
}

#[test]
fn http_manifest_url_is_rejected() {
    assert_eq!(
        fetch_text_smart("http://example.com/update-manifest.json").unwrap_err(),
        "Insecure plugin manifest URL is not allowed"
    );
}

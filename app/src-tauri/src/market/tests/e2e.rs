//! 真实产物端到端用例（opt-in，默认跳过）。

use std::path::Path;

use super::*;

/// 验证打包脚本（code/plugins/scripts/build-example-package.mjs）的真实产物
/// 经 file:// 全链路（验签 → 复制 → sha256/size 校验 → 落状态）可安装。
///
/// 运行：
///   SPARK_MARKET_E2E_RELEASE_DIR=<repo>/code/desktop/dist-market/plugins \
///   SPARK_MARKET_E2E_PUBLIC_KEY_PEM="$(cat <repo>/code/desktop/dist-market/plugins/spark-example/update-manifest.pub.pem)" \
///   cargo test e2e_real_release_artifacts
#[test]
fn e2e_real_release_artifacts() {
    let (Ok(release_root), Ok(pem)) = (
        std::env::var("SPARK_MARKET_E2E_RELEASE_DIR"),
        std::env::var("SPARK_MARKET_E2E_PUBLIC_KEY_PEM"),
    ) else {
        eprintln!("skip e2e_real_release_artifacts: SPARK_MARKET_E2E_* env not set");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let paths = MarketPaths {
        state_file: tmp.path().join("data/plugin-market-state.json"),
        packages_root: tmp.path().join("data/plugins"),
        local_release_roots: vec![PathBuf::from(release_root)],
        // 不挂源码目录：隔离验证"已签名 bundle"路径，不混 bundled-dev-source
        local_source_roots: vec![],
        repo_cache_dir: tmp.path().join("data/plugin-repo-cache"),
    };

    // 反：内置默认公钥（与本机 .secrets 签名钥不对应）必须拒装该产物
    let mut untrusted = PluginMarketService::new(
        paths.clone(),
        trust::DEFAULT_PLUGIN_PUBLIC_KEYS_PEM
            .iter()
            .map(|s| s.to_string())
            .collect(),
    );
    untrusted.initialize().unwrap();
    assert!(!untrusted.list_market()[0].installed);
    assert_eq!(
        untrusted.install("spark-example").unwrap_err(),
        "Plugin manifest signature verification failed: spark-example"
    );

    // 正：产物自带公钥（env 覆盖场景同款）→ reconcile 直接标记已安装
    let mut service = PluginMarketService::new(paths.clone(), vec![pem]);
    service.initialize().unwrap();
    let items = service.list_market();
    assert!(items[0].installed, "real bundle should reconcile to installed");
    assert_eq!(items[0].last_check_reason, "bundled");

    // 显式 install（file:// 复制到 packages_root）+ .spkg 内部一致性：
    // 逐文件校验 contentBase64 解码后的 sha256/size 与清单一致
    let installed = service.install("spark-example").unwrap();
    assert!(Path::new(&installed.package_path).is_file());
    let spkg: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(&installed.package_path).unwrap(),
    )
    .unwrap();
    assert_eq!(spkg["pluginId"], "spark-example");
    assert_eq!(spkg["domain"], "plugin:spark-example");
    for file in spkg["files"].as_array().unwrap() {
        use base64::Engine as _;
        let content = base64::engine::general_purpose::STANDARD
            .decode(file["contentBase64"].as_str().unwrap())
            .unwrap();
        assert_eq!(
            hex::encode(sha2::Sha256::digest(&content)),
            file["sha256"].as_str().unwrap(),
            "spkg file {} sha256 mismatch",
            file["path"]
        );
        assert_eq!(content.len() as u64, file["size"].as_u64().unwrap());
    }

    // 升级流：同版本 check → up-to-date；状态文件已落盘
    let probes = service.check_for_updates(Some("spark-example")).unwrap();
    assert_eq!(probes[0].reason, "up-to-date");
    assert!(paths.state_file.is_file());
}

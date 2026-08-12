//! 真实产物端到端用例（opt-in，默认跳过）。
//!
//! 解耦（plugin_decoupling.md §4.1-4.6）：目录驱动 install 已移除，真实产物经
//! .spkg 侧载（sideload）进入。此用例验证侧载导入真实产物的全链路（整包哈希
//! 复核 → 逐文件 sha256/size 校验 → 落状态）。

use std::path::Path;

use super::*;

/// 验证打包脚本（code/plugins/scripts/build-example-package.mjs）的真实产物
/// 经侧载全链路（inspect → import → 逐文件校验 → 落状态）可导入。
///
/// 运行：
///   SPARK_MARKET_E2E_SPKG=<repo>/code/plugins/<id>/dist/*.spkg \
///   cargo test e2e_real_release_artifacts
#[test]
fn e2e_real_release_artifacts() {
    let Ok(spkg_path) = std::env::var("SPARK_MARKET_E2E_SPKG") else {
        eprintln!("skip e2e_real_release_artifacts: SPARK_MARKET_E2E_SPKG not set");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let mut service = PluginMarketService::new(
        MarketPaths {
            state_file: tmp.path().join("data/plugin-market-state.json"),
            packages_root: tmp.path().join("data/plugins"),
            repo_cache_dir: tmp.path().join("data/plugin-repo-cache"),
        },
        vec![],
    );
    service.initialize().unwrap();

    let preview = service
        .inspect_local_package(&spkg_path)
        .expect("inspect real .spkg");
    assert!(preview.size > 0);
    let installed = service
        .import_local_package(&spkg_path, &preview.sha256, false)
        .expect("import real .spkg");
    assert!(Path::new(&installed.package_path).is_file());

    // .spkg 内部一致性：逐文件校验 contentBase64 解码后的 sha256/size
    let spkg: serde_json::Value = serde_json::from_str(&fs::read_to_string(&spkg_path).unwrap()).unwrap();
    assert_eq!(spkg["pluginId"], installed.plugin_id);
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
}

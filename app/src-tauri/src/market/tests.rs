//! 插件市场服务单测：注入临时目录直造服务实例，直调公共 API（不依赖 Tauri）。
//! 夹具与共享辅助在本文件，用例按职责分文件（reconcile/install/updates/wire/e2e）。

use std::fs;
use std::path::PathBuf;

use sha2::Digest;

use super::sources::fetch_text_smart;
use super::state::read_state_file;
use super::trust::tests::{sign_text, test_keypair};
use super::types::{InstalledPluginState, PluginUpdateProbe};
use super::*;

mod e2e;
mod install;
mod reconcile;
mod updates;
mod wire;

/// 测试夹具：临时目录 + 服务实例 + 签名密钥。
struct Fixture {
    _tmp: tempfile::TempDir,
    release_root: PathBuf,
    source_root: PathBuf,
    packages_root: PathBuf,
    state_file: PathBuf,
    pem: String,
    signing_key: ed25519_dalek::SigningKey,
}

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let (pem, signing_key) = test_keypair(42);
        Self {
            release_root: tmp.path().join("dist-market/plugins"),
            source_root: tmp.path().join("plugins-src"),
            packages_root: tmp.path().join("data/plugins"),
            state_file: tmp.path().join("data/plugin-market-state.json"),
            pem,
            signing_key,
            _tmp: tmp,
        }
    }

    fn service(&self) -> PluginMarketService {
        PluginMarketService::new(
            MarketPaths {
                state_file: self.state_file.clone(),
                packages_root: self.packages_root.clone(),
                local_release_roots: vec![self.release_root.clone()],
                local_source_roots: vec![self.source_root.clone()],
            },
            vec![self.pem.clone()],
        )
    }

    fn release_dir(&self) -> PathBuf {
        self.release_root.join("weibo-core")
    }
}

struct ReleaseOpts {
    version: String,
    plugin_id: String,
    domain: String,
    permissions: Option<Vec<String>>,
    tamper_sha256: bool,
    tamper_size: bool,
    bad_signature: bool,
}

impl Default for ReleaseOpts {
    fn default() -> Self {
        Self {
            version: "0.1.0".to_string(),
            plugin_id: "weibo-core".to_string(),
            domain: "plugin:weibo-core".to_string(),
            permissions: None,
            tamper_sha256: false,
            tamper_size: false,
            bad_signature: false,
        }
    }
}

/// 在 release_root 下造一份与打包脚本同构的发布产物（spkg + 清单 + 签名）。
fn write_release(fixture: &Fixture, opts: &ReleaseOpts) {
    let dir = fixture.release_dir();
    fs::create_dir_all(&dir).unwrap();

    let file_name = format!("spark-plugin-weibo-core-{}.spkg", opts.version);
    let package_payload = serde_json::json!({
        "pluginId": opts.plugin_id,
        "domain": opts.domain,
        "version": opts.version,
        "files": [{
            "path": "manifest.ts",
            "sha256": "00",
            "size": 1,
            "contentBase64": "AA=="
        }]
    });
    let package_text = format!("{}\n", serde_json::to_string_pretty(&package_payload).unwrap());
    fs::write(dir.join(&file_name), &package_text).unwrap();
    let digest = if opts.tamper_sha256 {
        "ff".repeat(32)
    } else {
        hex::encode(sha2::Sha256::digest(package_text.as_bytes()))
    };
    let size = if opts.tamper_size {
        package_text.len() as u64 + 1
    } else {
        package_text.len() as u64
    };

    let mut manifest = serde_json::json!({
        "pluginId": opts.plugin_id,
        "domain": opts.domain,
        "manifestVersion": 1,
        "version": opts.version,
        "releaseTime": "2026-07-22T00:00:00.000Z",
        "assets": [{
            "kind": "package",
            "fileName": file_name,
            "url": format!("file://{}", dir.join(&file_name).to_string_lossy()),
            "sha256": digest,
            "size": size
        }]
    });
    if let Some(permissions) = &opts.permissions {
        manifest["permissions"] = serde_json::json!(permissions);
    }
    let manifest_text = format!("{}\n", serde_json::to_string_pretty(&manifest).unwrap());
    fs::write(dir.join("update-manifest.json"), &manifest_text).unwrap();

    let signature = if opts.bad_signature {
        let (_, other_key) = test_keypair(7);
        sign_text(&other_key, &manifest_text)
    } else {
        sign_text(&fixture.signing_key, &manifest_text)
    };
    fs::write(dir.join("update-manifest.sig"), format!("{signature}\n")).unwrap();
}

fn write_dev_source(fixture: &Fixture) {
    let dir = fixture.source_root.join("weibo-core");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("manifest.ts"), "export {};\n").unwrap();
}

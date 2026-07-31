//! .spkg 侧载导入单测（sideload.rs）：inspect 预览 / import 落状态与权限 / 拒绝矩阵。

use base64::Engine;
use sha2::Digest;

use super::*;

/// 造一个合法 .spkg 容器文本（per-file sha256/size 真实计算，与打包脚本同构）。
fn spkg_text(plugin_id: &str, files: &[(&str, &[u8])]) -> String {
    let entries: Vec<_> = files
        .iter()
        .map(|(path, content)| {
            serde_json::json!({
                "path": path,
                "sha256": hex::encode(sha2::Sha256::digest(content)),
                "size": content.len(),
                "contentBase64": base64::engine::general_purpose::STANDARD.encode(content),
            })
        })
        .collect();
    serde_json::json!({
        "pluginId": plugin_id,
        "domain": format!("plugin:{plugin_id}"),
        "version": "1.0.0",
        "files": entries,
    })
    .to_string()
}

fn write_spkg(fixture: &Fixture, name: &str, text: &str) -> PathBuf {
    let dir = fixture.release_root.join("sideload");
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    fs::write(&path, text).unwrap();
    path
}

/// 包内 manifest.json：含一个高级权限 org:sync 与一个非法权限（应被过滤）。
const INNER_MANIFEST: &str = r#"{"id":"todo-local","domain":"plugin:todo-local","name":"本地待办","version":"1.0.0","permissions":["org:sync","bogus:perm"]}"#;

fn good_spkg(fixture: &Fixture) -> PathBuf {
    write_spkg(
        fixture,
        "spark-plugin-todo-local-1.0.0.spkg",
        &spkg_text(
            "todo-local",
            &[
                ("manifest.json", INNER_MANIFEST.as_bytes()),
                ("views/main.js", b"hello"),
            ],
        ),
    )
}

#[test]
fn inspect_reads_container_and_inner_manifest() {
    let fixture = Fixture::new();
    let spkg = good_spkg(&fixture);
    let preview = fixture
        .service()
        .inspect_local_package(spkg.to_str().unwrap())
        .unwrap();
    assert_eq!(preview.plugin_id, "todo-local");
    assert_eq!(preview.name, "本地待办");
    // 非法权限被过滤，仅保留合法声明
    assert_eq!(preview.permissions, vec!["org:sync".to_string()]);
    assert_eq!(
        preview.sha256,
        hex::encode(sha2::Sha256::digest(fs::read(&spkg).unwrap()))
    );
    assert_eq!(preview.file_name, "spark-plugin-todo-local-1.0.0.spkg");
}

#[test]
fn import_installs_and_marks_sideloaded_trust() {
    let fixture = Fixture::new();
    let spkg = good_spkg(&fixture);
    let mut service = fixture.service();
    let preview = service.inspect_local_package(spkg.to_str().unwrap()).unwrap();
    let state = service
        .import_local_package(spkg.to_str().unwrap(), &preview.sha256, false)
        .unwrap();
    assert_eq!(state.plugin_id, "todo-local");
    assert_eq!(state.version, "1.0.0");
    assert_eq!(state.trust.as_deref(), Some("sideloaded"));
    assert!(state.enabled);
    // granted = 基础 ∪ 声明∩高级（org:sync）
    for permission in ["storage:read", "storage:write", "org:read", "proof:verify", "org:sync"] {
        assert!(state.granted_permissions.contains(&permission.to_string()));
    }
    assert!(!state.granted_permissions.contains(&"bogus:perm".to_string()));
    // 包已复制进 packages 目录且持久化状态可读
    assert!(PathBuf::from(&state.package_path).is_file());
    let persisted = read_state_file(&fixture.state_file);
    assert_eq!(
        persisted.installed["todo-local"].trust.as_deref(),
        Some("sideloaded")
    );
}

#[test]
fn import_rejects_hash_change_after_preview() {
    // 真实换文件：inspect 之后源 .spkg 被替换为另一份合法包，import 必须拒
    let fixture = Fixture::new();
    let spkg = good_spkg(&fixture);
    let mut service = fixture.service();
    let preview = service.inspect_local_package(spkg.to_str().unwrap()).unwrap();
    fs::write(
        &spkg,
        spkg_text("todo-local", &[("views/main.js", b"swapped")]),
    )
    .unwrap();
    assert_eq!(
        service
            .import_local_package(spkg.to_str().unwrap(), &preview.sha256, false)
            .unwrap_err(),
        "Sideload package changed since preview: sha256 mismatch"
    );
    assert!(read_state_file(&fixture.state_file).installed.is_empty());
}

#[test]
fn import_rejects_reserved_plugin_id() {
    let fixture = Fixture::new();
    let mut service = fixture.service();
    // system 与内置目录 id（spark-example）不允许侧载顶替
    for reserved in ["system", "spark-example"] {
        let spkg = write_spkg(
            &fixture,
            &format!("{reserved}.spkg"),
            &spkg_text(reserved, &[("views/main.js", b"x")]),
        );
        let preview = service.inspect_local_package(spkg.to_str().unwrap()).unwrap();
        assert_eq!(
            service
                .import_local_package(spkg.to_str().unwrap(), &preview.sha256, false)
                .unwrap_err(),
            format!("Sideload import refused: reserved plugin id {reserved}")
        );
    }
    assert!(read_state_file(&fixture.state_file).installed.is_empty());
}

#[test]
fn import_overwrite_higher_trust_requires_confirmation() {
    let fixture = Fixture::new();
    let mut service = fixture.service();
    // 预置 repo-anchored 安装（更高信任）
    service.state.installed.insert(
        "todo-local".to_string(),
        InstalledPluginState {
            plugin_id: "todo-local".to_string(),
            version: "1.0.0".to_string(),
            package_path: String::new(),
            sha256: String::new(),
            size: 0,
            installed_at: 0,
            enabled: true,
            granted_permissions: vec![],
            trust: Some("repo-anchored".to_string()),
            supported_spaces: None,
        },
    );
    let spkg = good_spkg(&fixture);
    let preview = service.inspect_local_package(spkg.to_str().unwrap()).unwrap();
    // 未确认 → 拒（结构化前缀供前端识别后弹确认框）
    assert_eq!(
        service
            .import_local_package(spkg.to_str().unwrap(), &preview.sha256, false)
            .unwrap_err(),
        "Sideload overwrite requires confirmation: existing install trust=repo-anchored for todo-local"
    );
    // 确认后 → 覆盖为 sideloaded
    let state = service
        .import_local_package(spkg.to_str().unwrap(), &preview.sha256, true)
        .unwrap();
    assert_eq!(state.trust.as_deref(), Some("sideloaded"));

    // 同级（sideloaded 覆盖 sideloaded）无需确认
    let spkg = write_spkg(
        &fixture,
        "spark-plugin-todo-local-1.0.1.spkg",
        &spkg_text("todo-local", &[("views/main.js", b"v2")]),
    );
    let preview = service.inspect_local_package(spkg.to_str().unwrap()).unwrap();
    assert!(
        service
            .import_local_package(spkg.to_str().unwrap(), &preview.sha256, false)
            .is_ok()
    );
}

#[test]
fn import_rejects_tampered_file_entry() {
    let fixture = Fixture::new();
    // 条目记录的 sha256 与内容不符（容器被篡改）
    let mut text = spkg_text("todo-local", &[("views/main.js", b"hello")]);
    text = text.replacen(
        &hex::encode(sha2::Sha256::digest(b"hello")),
        &"ff".repeat(32),
        1,
    );
    let spkg = write_spkg(&fixture, "tampered.spkg", &text);
    let mut service = fixture.service();
    let digest = hex::encode(sha2::Sha256::digest(fs::read(&spkg).unwrap()));
    assert_eq!(
        service
            .import_local_package(spkg.to_str().unwrap(), &digest, false)
            .unwrap_err(),
        "Sideload package invalid: file views/main.js: sha256 mismatch"
    );
}

#[test]
fn reject_matrix() {
    let fixture = Fixture::new();
    let service = fixture.service();
    // 非 .spkg 扩展名
    let txt = write_spkg(&fixture, "a.txt", &spkg_text("todo-local", &[("a.js", b"x")]));
    assert_eq!(
        service.inspect_local_package(txt.to_str().unwrap()).unwrap_err(),
        "Sideload package invalid: not a .spkg file"
    );
    // 坏 JSON
    let bad = write_spkg(&fixture, "bad.spkg", "{ not json");
    assert!(service
        .inspect_local_package(bad.to_str().unwrap())
        .unwrap_err()
        .starts_with("Sideload package invalid: "));
    // 非法 pluginId（穿越 / 大写 / 空段 / Windows 保留设备名）
    for id in ["../evil", "Todo-Local", "a//b", "a/./b", "con", "a/com1/b", "nul.txt"] {
        let spkg = write_spkg(&fixture, "badid.spkg", &spkg_text(id, &[("a.js", b"x")]));
        assert_eq!(
            service.inspect_local_package(spkg.to_str().unwrap()).unwrap_err(),
            "Sideload package invalid: pluginId invalid"
        );
    }
    // 空文件清单
    let empty = write_spkg(&fixture, "empty.spkg", &spkg_text("todo-local", &[]));
    assert_eq!(
        service.inspect_local_package(empty.to_str().unwrap()).unwrap_err(),
        "Sideload package invalid: files empty"
    );
}

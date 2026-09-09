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
    for permission in ["storage:read", "storage:write", "org:read", "proof:verify", "identity:verify", "org:sync"] {
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
    // system 会话 id 不允许侧载顶替；内置目录 id（spark-example）解耦后不再保留，
    // 可正常侧载（见下方正常路径断言）
    let spkg = write_spkg(
        &fixture,
        "system.spkg",
        &spkg_text("system", &[("views/main.js", b"x")]),
    );
    let preview = service.inspect_local_package(spkg.to_str().unwrap()).unwrap();
    assert_eq!(
        service
            .import_local_package(spkg.to_str().unwrap(), &preview.sha256, false)
            .unwrap_err(),
        "Sideload import refused: reserved plugin id system"
    );
    assert!(read_state_file(&fixture.state_file).installed.is_empty());

    // 解耦后内置目录为空：spark-example 不再是保留 id，可正常侧载
    let spkg = write_spkg(
        &fixture,
        "spark-example.spkg",
        &spkg_text("spark-example", &[("views/main.js", b"x")]),
    );
    let preview = service.inspect_local_package(spkg.to_str().unwrap()).unwrap();
    let state = service
        .import_local_package(spkg.to_str().unwrap(), &preview.sha256, false)
        .unwrap();
    assert_eq!(state.plugin_id, "spark-example");
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
            requires: None,
            window: None,
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

#[test]
fn inspect_reads_supported_spaces_and_initialize_backfills_legacy_records() {
    // 包内 manifest 声明 supportedSpaces → inspect 预览可见
    let fixture = Fixture::new();
    let manifest = r#"{"id":"todo-local","name":"本地待办","supportedSpaces":["personal","team"]}"#;
    let spkg = write_spkg(
        &fixture,
        "spark-plugin-todo-local-1.0.0.spkg",
        &spkg_text(
            "todo-local",
            &[("manifest.json", manifest.as_bytes()), ("views/main.js", b"hello")],
        ),
    );
    let mut service = fixture.service();
    let preview = service.inspect_local_package(spkg.to_str().unwrap()).unwrap();
    // 非法值丢弃（"team"），合法值保留
    assert_eq!(preview.supported_spaces, Some(vec!["personal".to_string()]));
    service
        .import_local_package(spkg.to_str().unwrap(), &preview.sha256, false)
        .unwrap();

    // 包内未声明 supportedSpaces 的对照插件
    let plain = write_spkg(
        &fixture,
        "spark-plugin-plain-local-1.0.0.spkg",
        &spkg_text(
            "plain-local",
            &[("manifest.json", INNER_MANIFEST.replace("todo-local", "plain-local").as_bytes()), ("views/main.js", b"hi")],
        ),
    );
    let plain_preview = service.inspect_local_package(plain.to_str().unwrap()).unwrap();
    assert_eq!(plain_preview.supported_spaces, None);
    service
        .import_local_package(plain.to_str().unwrap(), &plain_preview.sha256, false)
        .unwrap();

    // 模拟旧版状态文件：抹掉 supportedSpaces 字段（旧记录无此字段，serde default → None）
    let raw = fs::read_to_string(&fixture.state_file).unwrap();
    let mut value: serde_json::Value = serde_json::from_str(&raw).unwrap();
    for id in ["todo-local", "plain-local"] {
        value["installed"][id]
            .as_object_mut()
            .unwrap()
            .remove("supportedSpaces");
    }
    fs::write(&fixture.state_file, value.to_string()).unwrap();

    // 启动对账：旧侧载记录重解析包内 manifest 回填；包内未声明的保持 None
    let mut reloaded = fixture.service();
    reloaded.initialize().unwrap();
    assert_eq!(
        reloaded.state.installed["todo-local"].supported_spaces,
        Some(vec!["personal".to_string()])
    );
    assert_eq!(reloaded.state.installed["plain-local"].supported_spaces, None);
    // 回填已落盘
    let persisted = read_state_file(&fixture.state_file);
    assert_eq!(
        persisted.installed["todo-local"].supported_spaces,
        Some(vec!["personal".to_string()])
    );
}

#[test]
fn import_persists_window_and_initialize_backfills_legacy_records() {
    use crate::market::catalog::PluginWindow;

    // 包内 manifest 声明合法 window → import 落库；非法值归一化为未声明
    let fixture = Fixture::new();
    let manifest = r#"{"id":"todo-local","name":"本地待办","window":{"defaultWidth":480,"defaultHeight":680}}"#;
    let spkg = write_spkg(
        &fixture,
        "spark-plugin-todo-local-1.0.0.spkg",
        &spkg_text(
            "todo-local",
            &[("manifest.json", manifest.as_bytes()), ("views/main.js", b"hello")],
        ),
    );
    let mut service = fixture.service();
    let preview = service.inspect_local_package(spkg.to_str().unwrap()).unwrap();
    let state = service
        .import_local_package(spkg.to_str().unwrap(), &preview.sha256, false)
        .unwrap();
    assert_eq!(state.window, Some(PluginWindow { default_width: 480, default_height: 680 }));
    // 市场合成条目回落安装态落库值（侧载插件无声明缓存）
    let entry = service
        .list_market()
        .into_iter()
        .find(|i| i.catalog.id == "todo-local")
        .unwrap();
    assert_eq!(
        entry.catalog.window,
        Some(PluginWindow { default_width: 480, default_height: 680 })
    );

    let bad = write_spkg(
        &fixture,
        "spark-plugin-badwin-local-1.0.0.spkg",
        &spkg_text(
            "badwin-local",
            &[
                (
                    "manifest.json",
                    r#"{"id":"badwin-local","name":"坏窗口","window":{"defaultWidth":100,"defaultHeight":680}}"#
                        .as_bytes(),
                ),
                ("views/main.js", b"hi"),
            ],
        ),
    );
    let bad_preview = service.inspect_local_package(bad.to_str().unwrap()).unwrap();
    let bad_state = service
        .import_local_package(bad.to_str().unwrap(), &bad_preview.sha256, false)
        .unwrap();
    assert_eq!(bad_state.window, None);

    // 模拟旧版状态文件：抹掉 window 字段，启动对账应从包内 manifest 回填
    let raw = fs::read_to_string(&fixture.state_file).unwrap();
    let mut value: serde_json::Value = serde_json::from_str(&raw).unwrap();
    for id in ["todo-local", "badwin-local"] {
        value["installed"][id]
            .as_object_mut()
            .unwrap()
            .remove("window");
    }
    fs::write(&fixture.state_file, value.to_string()).unwrap();

    let mut reloaded = fixture.service();
    reloaded.initialize().unwrap();
    assert_eq!(
        reloaded.state.installed["todo-local"].window,
        Some(PluginWindow { default_width: 480, default_height: 680 })
    );
    // 包内声明非法的记录回填后仍为 None（未声明口径）
    assert_eq!(reloaded.state.installed["badwin-local"].window, None);
}

/// 当前平台之外的另一个平台（测试环境恒为 desktop/mobile 二值口径）。
fn other_platform() -> &'static str {
    if crate::market::catalog::current_platform() == "desktop" {
        "mobile"
    } else {
        "desktop"
    }
}

#[test]
fn import_rejects_verification_class_sideload() {
    // L1 强制（community-model §十）：验证类插件（声明 credentials:*）侧载 = L0，
    // 不满足「L1 源码公开可审计」，import 即拒
    let fixture = Fixture::new();
    let manifest = r#"{"id":"verify-local","name":"本地验证","permissions":["credentials:read"]}"#;
    let spkg = write_spkg(
        &fixture,
        "spark-plugin-verify-local-1.0.0.spkg",
        &spkg_text(
            "verify-local",
            &[("manifest.json", manifest.as_bytes()), ("views/main.js", b"hello")],
        ),
    );
    let mut service = fixture.service();
    let preview = service.inspect_local_package(spkg.to_str().unwrap()).unwrap();
    assert_eq!(
        service
            .import_local_package(spkg.to_str().unwrap(), &preview.sha256, false)
            .unwrap_err(),
        "Plugin trust requirement unmet: verify-local is verification-class (declares credentials:* permission) and requires L1 open-source-auditable install, current trust is L0 (sideloaded)"
    );
    assert!(read_state_file(&fixture.state_file).installed.is_empty());
}

#[test]
fn import_enforces_platform_requires() {
    // 包内 manifest requires.platforms 不含当前平台 → 侧载同样拒装（安装通路不豁免）
    let fixture = Fixture::new();
    let manifest = format!(
        r#"{{"id":"todo-local","name":"本地待办","requires":{{"platforms":["{}"]}}}}"#,
        other_platform()
    );
    let spkg = write_spkg(
        &fixture,
        "spark-plugin-todo-local-1.0.0.spkg",
        &spkg_text(
            "todo-local",
            &[("manifest.json", manifest.as_bytes()), ("views/main.js", b"hello")],
        ),
    );
    let mut service = fixture.service();
    let preview = service.inspect_local_package(spkg.to_str().unwrap()).unwrap();
    // 预览透出平台约束（前端展示用）
    assert_eq!(
        preview.requires.as_ref().unwrap().platforms,
        vec![other_platform().to_string()]
    );
    assert_eq!(
        service
            .import_local_package(spkg.to_str().unwrap(), &preview.sha256, false)
            .unwrap_err(),
        format!(
            "Plugin platform unsupported: todo-local requires platforms [{}], current platform is {}",
            other_platform(),
            crate::market::catalog::current_platform()
        )
    );
    assert!(read_state_file(&fixture.state_file).installed.is_empty());
}

#[test]
fn import_persists_requires_and_initialize_backfills_legacy_records() {
    // requires 宽进归一化：非法平台值丢弃，合法值保留并落库
    let fixture = Fixture::new();
    let current = crate::market::catalog::current_platform();
    let manifest = format!(
        r#"{{"id":"todo-local","name":"本地待办","requires":{{"platforms":["{}","watch"]}}}}"#,
        current
    );
    let spkg = write_spkg(
        &fixture,
        "spark-plugin-todo-local-1.0.0.spkg",
        &spkg_text(
            "todo-local",
            &[("manifest.json", manifest.as_bytes()), ("views/main.js", b"hello")],
        ),
    );
    let mut service = fixture.service();
    let preview = service.inspect_local_package(spkg.to_str().unwrap()).unwrap();
    let state = service
        .import_local_package(spkg.to_str().unwrap(), &preview.sha256, false)
        .unwrap();
    assert_eq!(
        state.requires.as_ref().unwrap().platforms,
        vec![current.to_string()]
    );

    // 模拟旧版状态文件：抹掉 requires 字段，启动对账应从包内 manifest 回填
    let raw = fs::read_to_string(&fixture.state_file).unwrap();
    let mut value: serde_json::Value = serde_json::from_str(&raw).unwrap();
    value["installed"]["todo-local"]
        .as_object_mut()
        .unwrap()
        .remove("requires");
    fs::write(&fixture.state_file, value.to_string()).unwrap();

    let mut reloaded = fixture.service();
    reloaded.initialize().unwrap();
    assert_eq!(
        reloaded.state.installed["todo-local"]
            .requires
            .as_ref()
            .unwrap()
            .platforms,
        vec![current.to_string()]
    );
    // 市场合成条目回落安装态落库值（侧载插件无声明缓存）
    let entry = reloaded
        .list_market()
        .into_iter()
        .find(|i| i.catalog.id == "todo-local")
        .unwrap();
    assert_eq!(
        entry.catalog.requires.as_ref().unwrap().platforms,
        vec![current.to_string()]
    );
    // 侧载信任级 = L0
    assert_eq!(entry.trust_level.as_deref(), Some("L0"));
}

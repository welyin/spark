//! 插件市场服务（Rust 移植自 TS desktop/src/main/plugin-market/service.ts，488 行原版）。
//!
//! 语义对齐要点：
//! - 清单解析优先级：本地 dist-market 发布目录优先（update-manifest.json + .sig 齐备），
//!   否则回退目录条目声明的远端 URL；`file://` 与 `/` 开头按本地文件读，
//!   `http://` 一律拒绝，其余按 https 下载（reqwest blocking + native-tls 系统信任库）。
//! - 安装 = 验签（Ed25519 detached，见 trust.rs）→ 下载/复制 .spkg → 校验
//!   sha256/size → 落状态（grantedPermissions = 基础 ∪ 声明∩高级，见 permissions.rs）。
//! - 启动对账（reconcile）：本地 bundle 验签通过 → 标记已安装；
//!   插件源码目录（code/plugins/<id>/ 含 manifest.ts/js）→ 标记 bundled-dev-source。
//! - 状态文件：<app_data_dir>/plugin-market-state.json（TS PersistedPluginState 同构）；
//!   更新探测（updateProbes）与 TS 一致仅驻留内存，不持久化。
//!
//! 与 TS 的有意差异（语义等价）：错误文案不含 JS `String(error)` 的 `Error: ` 前缀；
//! sha256 整文件读入计算（包体小，TS 为流式）。
//!
//! 子模块划分：catalog（目录）/ permissions（权限）/ semver（版本比较）/ trust（验签）/
//! types（线形）为纯数据与算法；sources（http/file 来源）、state（状态持久化）、
//! service（初始化与启动对账）、install（安装链路）、updates（更新探测与列表聚合）
//! 为服务实现；单测在 tests/ 下按职责对应分文件。

pub mod catalog;
mod install;
pub mod permissions;
pub mod semver;
mod service;
mod sources;
mod state;
#[cfg(test)]
mod tests;
pub mod trust;
pub mod types;
mod updates;

use std::path::{Component, Path, PathBuf};

use state::PLUGIN_STATE_FILE;

pub use service::PluginMarketService;

/// 市场服务路径配置（注入式，测试用临时目录直造）。
#[derive(Clone, Debug)]
pub struct MarketPaths {
    /// 状态文件：<app_data_dir>/plugin-market-state.json
    pub state_file: PathBuf,
    /// 已安装包落盘根目录：<app_data_dir>/plugins（包在 <root>/<id>/packages/）
    pub packages_root: PathBuf,
    /// 本地发布目录候选根（各 root/<pluginId>/ 下找 update-manifest.json/.sig）
    pub local_release_roots: Vec<PathBuf>,
    /// 插件源码目录候选根（各 root/<pluginId>/ 下找 manifest.ts/js）
    pub local_source_roots: Vec<PathBuf>,
}

/// 词法归一化路径（折叠 `.`/`..`，TS path.normalize 同款；不做 symlink 解析）。
fn normalize_path(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

impl MarketPaths {
    /// 生产默认：状态/包目录在 app_data_dir 下；本地发布目录与源码目录按
    /// 编译期 crate 位置（code/app/src-tauri）与运行时 cwd 双候选
    /// （对齐 TS 的 appPath/cwd 候选语义；打包安装后这些目录不存在即自动走远端 URL）。
    pub fn for_app(app_data_dir: &Path) -> Self {
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let cwd = std::env::current_dir().unwrap_or_default();
        let release_roots = [
            manifest_dir.join("../dist-market/plugins"),
            cwd.join("dist-market/plugins"),
        ];
        let source_roots = [manifest_dir.join("../../plugins"), cwd.join("../plugins")];
        let dedupe = |dirs: &[PathBuf; 2]| {
            let mut unique: Vec<PathBuf> = Vec::new();
            for dir in dirs {
                let normalized = normalize_path(dir);
                if !unique.contains(&normalized) {
                    unique.push(normalized);
                }
            }
            unique
        };
        Self {
            state_file: app_data_dir.join(PLUGIN_STATE_FILE),
            packages_root: app_data_dir.join("plugins"),
            local_release_roots: dedupe(&release_roots),
            local_source_roots: dedupe(&source_roots),
        }
    }
}

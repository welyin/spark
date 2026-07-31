//! 卸载：移除状态记录 + 删除 app_data 插件目录内的包文件 + 落卸载墓碑。
//!
//! 安全口径：只允许删除 packages_root/<id>/ 内的文件。记录中的 packagePath
//! 若指向目录外（bundled-dev-source 的开发者源码目录、本地发布目录的发布产物）
//! 一律不动文件、仅移除记录——源码目录是开发者的代码，绝不可动。
//!
//! 墓碑（tombstone）：卸载把 id 写入持久化的 uninstalled 集合，启动对账跳过
//! 该 id，防 bundled / bundled-dev-source 插件卸载后重启复活；显式安装清除。

use std::fs;
use std::path::Path;

use super::sideload::plugin_id_valid;
use super::{normalize_path, PluginMarketService};

/// 符号链接逃逸加固：canonicalize packages_root 与目标路径最近的已存在祖先
/// （package_path 或其父目录可能已被手动删除，对目录部分而非文件 canonicalize），
/// 真实落点必须仍在 packages_root 内，覆盖「packages/ 为指向外部的 symlink」
/// 「插件目录整体为 symlink」两种逃逸。解析失败一律判不安全：只删记录不动文件。
///
/// 盘符/大小写说明：两侧同经 fs::canonicalize 解析，Windows 下统一为 \\?\ 前缀
/// 与盘上真实大小写，starts_with 比较无盘符形态/大小写歧义；词法归一的
/// starts_with（调用处）两侧同源自 packages_root.join，亦不引入大小写差。
fn deletion_target_safe(packages_root: &Path, package_path: &Path) -> bool {
    // packages_root 不存在 → 其下无文件可删，跳过文件删除即可（不影响清记录）
    let Ok(root) = fs::canonicalize(packages_root) else {
        return false;
    };
    let mut probe = Some(package_path);
    while let Some(path) = probe {
        if path.exists() {
            return match fs::canonicalize(path) {
                Ok(real) => real.starts_with(&root),
                Err(_) => false,
            };
        }
        probe = path.parent();
    }
    false
}

impl PluginMarketService {
    /// 卸载插件：删除包文件（仅限 app_data 插件目录内）→ 移除状态记录 →
    /// 写卸载墓碑 → 持久化。
    ///
    /// bundled-dev-source / 本地发布目录的记录同样可卸载：其 packagePath 指向
    /// packages_root 之外，只移除记录不动文件；墓碑阻止对账（reconcile）下次
    /// 启动重新登记，显式安装（install / install_from_repo / 侧载导入）可复活。
    pub fn uninstall(&mut self, plugin_id: &str) -> Result<(), String> {
        // id 段校验与 sideload/repo 同规则（拒空段、`.`/`..`、非法字符）：
        // 落盘路径逐段 join，非法 id 必须先拒，防穿越删除
        if !plugin_id_valid(plugin_id) {
            return Err(format!("Plugin id invalid: {plugin_id}"));
        }
        let Some(record) = self.state.installed.get(plugin_id).cloned() else {
            return Err(format!("Plugin is not installed: {plugin_id}"));
        };

        // 先删文件后改状态：删除失败即报错并保留记录（可重试）；
        // 反向顺序会在持久化后留下既无记录又删不掉的残留文件
        let plugin_dir = normalize_path(&self.paths.packages_root.join(plugin_id));
        let package_path = normalize_path(Path::new(&record.package_path));
        if package_path.starts_with(&plugin_dir)
            && deletion_target_safe(&self.paths.packages_root, &package_path)
        {
            match fs::remove_file(&package_path) {
                Ok(()) => {}
                // 文件已不存在视为成功（盘与状态本就不一致，照常清记录）
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    return Err(format!("remove package {}: {e}", package_path.display()));
                }
            }
            // 顺带清理 packages/ 与插件目录：remove_dir 只删空目录，
            // 目录内若有意外内容则保留（不连带删除）
            let _ = fs::remove_dir(plugin_dir.join("packages"));
            let _ = fs::remove_dir(&plugin_dir);
            // repo 多段 id（如 github.com/acme/todo）：向上清空空父目录直到
            // packages_root（同为 remove_dir 只删空目录，遇非空即停）；
            // 词法比较前两侧同经 normalize_path，避免 root 含 `.`/`..` 段时比较失配
            let packages_root = normalize_path(&self.paths.packages_root);
            let mut dir = plugin_dir.parent();
            while let Some(d) = dir {
                if d == packages_root.as_path() {
                    break;
                }
                if fs::remove_dir(d).is_err() {
                    break;
                }
                dir = d.parent();
            }
        }
        // 目录外 packagePath（dev-source 源码目录 / 本地发布产物）：不动任何文件

        self.state.installed.remove(plugin_id);
        self.update_probes.remove(plugin_id);
        // 卸载墓碑：阻止启动对账把 bundled / bundled-dev-source 插件重新登记
        self.state.uninstalled.insert(plugin_id.to_string());
        // persist 失败时内存已删记录但落盘未生效：下次启动从旧状态文件恢复
        // （记录与文件不一致，重试卸载可自愈）；但墓碑未落盘会致 bundled 记录
        // 复活——与既有 persist 失败语义同属可重试范畴，显式注释说明
        self.persist()?;
        Ok(())
    }
}

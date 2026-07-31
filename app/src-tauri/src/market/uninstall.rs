//! 卸载：移除状态记录 + 删除 app_data 插件目录内的包文件。
//!
//! 安全口径：只允许删除 packages_root/<id>/ 内的文件。记录中的 packagePath
//! 若指向目录外（bundled-dev-source 的开发者源码目录、本地发布目录的发布产物）
//! 一律不动文件、仅移除记录——源码目录是开发者的代码，绝不可动。

use std::fs;
use std::path::Path;

use super::sideload::plugin_id_valid;
use super::{normalize_path, PluginMarketService};

impl PluginMarketService {
    /// 卸载插件：删除包文件（仅限 app_data 插件目录内）→ 移除状态记录 → 持久化。
    ///
    /// bundled-dev-source / 本地发布目录的记录同样可卸载：其 packagePath 指向
    /// packages_root 之外，只移除记录不动文件；对账（reconcile）下次启动会重新登记。
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
        if package_path.starts_with(&plugin_dir) {
            match fs::remove_file(&package_path) {
                Ok(()) => {}
                // 文件已不存在视为成功（盘与状态本就不一致，照常清记录）
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(format!("{e}")),
            }
            // 顺带清理 packages/ 与插件目录：remove_dir 只删空目录，
            // 目录内若有意外内容则保留（不连带删除）
            let _ = fs::remove_dir(plugin_dir.join("packages"));
            let _ = fs::remove_dir(&plugin_dir);
        }
        // 目录外 packagePath（dev-source 源码目录 / 本地发布产物）：不动任何文件

        self.state.installed.remove(plugin_id);
        self.update_probes.remove(plugin_id);
        self.persist()?;
        Ok(())
    }
}

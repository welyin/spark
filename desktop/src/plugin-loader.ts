/**
 * 插件视图自动加载器。
 *
 * 通过 Vite 的 import.meta.glob 扫描 code/plugins 下的插件入口，
 * 让每个插件在自身入口模块中调用 registerPluginView 完成注册。
 * 内核（App.vue / main.ts）无需感知具体插件。
 *
 * 插件目录在 src 之外（code/plugins，与 code/desktop 平级）：
 * - dev：依赖 vite.config.ts 的 server.fs.allow 放开上级目录；
 * - build：import.meta.glob 由 Vite 编译期展开，不受 fs.allow 限制。
 */
const pluginEntries = import.meta.glob('../../plugins/*/index.ts', { eager: true });

export function initializePlugins(): void {
  // 插件入口模块在被 import 时已自行完成注册。
  // 这里仅保留一个显式的初始化入口，便于后续加入校验/日志。
  void pluginEntries;
}

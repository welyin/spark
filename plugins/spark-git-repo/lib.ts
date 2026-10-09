/**
 * 代码仓库应用（spark-git-repo）· 库包入口（档二-3 组合纪律·库件形态）。
 *
 * 「项目」等组合者插件构建期依赖本件时引入本入口：导出纯逻辑（model）、
 * 协议线形（wire）、服务层（GitRepoService）与视图组件（RepoView / PrCard），
 * 供组合者以组件 props / 参数注入仓库上下文（项目议题 affairId、本地仓库
 * 路径等——注入接口是组件 props / 方法参数，不是插件间契约）。
 *
 * 纪律（同 spark-kanban §5.1 口径）：
 * - 本入口不握手、不挂载、不自执行——库是纯代码，没有自己的数据域、不单独
 *   运行；运行时数据写到组合者命名空间（sdk.domain 为组合者域），库内代码
 *   不假设自身插件域身份；
 * - **组合者构造 GitRepoService 必须传自身插件 id 作为 namespace**：
 *   `new GitRepoService(sdk, undefined, 'spark-project')`——内核 plugindata
 *   强制集合名前缀 == 调用方插件 id，缺省 'spark-git-repo' 只在独立安装形态
 *   成立；gitRepoCollections(namespace) 为同一约定的集合名工厂；
 * - 纯逻辑 / 视图分离：model.ts 与 wire.ts 不依赖 SDK 运行时与 Vue，可独立
 *   单测与复用；GitRepoService 集中全部 SDK 调用；视图组件只编排。
 */
export * from './model';
export * from './wire';
export {
  GitRepoService,
  gitRepoCollections,
  GIT_REPO_COLLECTIONS,
  probeCapabilities,
  REQUIRED_AFFAIRS_METHODS,
  REQUIRED_CONTENT_METHODS,
  AFFAIRS_MODULE_MISSING,
  CONTENT_MODULE_MISSING,
  type GitRepoCollections,
  type PluginCapabilities,
  type ProjectBinding,
  type MirrorView,
  type PrSummaryView
} from './service';
export { default as RepoView } from './RepoView.vue';
export { default as PrCard } from './PrCard.vue';

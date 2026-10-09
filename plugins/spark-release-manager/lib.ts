/**
 * 发布管理插件（spark-release-manager）· 库包入口（release-management.md §5.1
 * 库包形态）。
 *
 * 「项目」等组合者插件构建期依赖本件时引入本入口：导出纯逻辑（model）、服务层
 * （ReleaseManagerService）与发布管理视图组件（ReleaseManagerView），供组合者以
 * 组件 props / 参数注入发布上下文（组织 orgId、当前用户身份等——注入接口是组件
 * props，不是插件间契约）。
 *
 * 纪律（§5.1，同 spark-kanban）：
 * - 本入口不握手、不挂载、不自执行——库是纯代码，没有自己的数据域、不单独
 *   运行；运行时数据写到组合者命名空间（sdk.domain 为组合者域——sdk.docs
 *   集合由内核按调用方域隔离，sdk.data 送达台账名由 namespace 参数构造）；
 * - **组合者构造 ReleaseManagerService 必须传自身插件 id 作为 namespace**：
 *   `new ReleaseManagerService(sdk, 'spark-project')`——内核 plugindata 强制
 *   集合名前缀 == 调用方插件 id，缺省 'spark-release-manager' 只在独立安装
 *   形态成立；
 * - 纯逻辑 / 视图分离：model.ts 不依赖 SDK 运行时与 Vue，可独立单测与复用；
 *   ReleaseManagerService 集中全部 SDK 调用；视图组件只编排。
 */
export * from './model';
export { ReleaseManagerService, RELEASE_COLLECTIONS, type ReleaseDeliveryStats } from './service';
export { default as ReleaseManagerView } from './ReleaseManagerView.vue';

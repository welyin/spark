/**
 * 任务看板插件（spark-kanban）· 库包入口（kanban.md §5.1 库包形态）。
 *
 * 「项目」等组合者插件构建期依赖本件时引入本入口：导出纯逻辑（model）、
 * 协议线形（wire）、服务层（KanbanService）与看板视图组件（KanbanView），
 * 供组合者以组件 props / 参数注入看板上下文（项目 affairId、当前用户阶梯
 * 状态等——注入接口是组件 props，不是插件间契约）。
 *
 * 纪律（§5.1）：
 * - 本入口不握手、不挂载、不自执行——库是纯代码，没有自己的数据域、不单独
 *   运行；运行时数据写到组合者命名空间（sdk.domain 为组合者域），库内代码
 *   不假设自身插件域身份；
 * - **组合者构造 KanbanService 必须传自身插件 id 作为 namespace**：
 *   `new KanbanService(sdk, 'spark-project')`——内核 plugindata 强制集合名
 *   前缀 == 调用方插件 id，缺省 'spark-kanban' 只在独立安装形态成立；
 *   kanbanCollections(namespace) 为同一约定的集合名工厂；
 * - 纯逻辑 / 视图分离：model.ts 与 wire.ts 不依赖 SDK 运行时与 Vue，可独立
 *   单测与复用；KanbanService 集中全部 SDK 调用；视图组件只编排。
 */
export * from './model';
export * from './wire';
export { KanbanService, kanbanCollections, KANBAN_COLLECTIONS, REQUIRED_AFFAIRS_METHODS, AFFAIRS_MODULE_MISSING } from './service';
export { default as KanbanView } from './KanbanView.vue';

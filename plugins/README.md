# Spark 插件目录

本目录存放官方插件源码（与 `code/app/` 平级）。每个插件一个子目录：

```
plugins/<id>/
  manifest.json      ← 声明式清单（唯一事实源）
  spark-plugin.json  ← 仓库声明文件（分发信任锚点雏形）
  index.ts           ← 主入口：connectPluginBridge 握手 + 按 __sparkPluginView 多视图分发
  *.vue / *.ts       ← 视图与业务逻辑（主视图 + 可选 message-card 卡片视图）
  vite.config.ts     ← 多入口构建配置（dist/views/main.js + views/<viewId>.js）
  dist/              ← 构建产物（gitignored，npm run build:example 生成）
  tests/             ← 插件单测（随 code/app 的 vitest 一起执行）
```

完整示例见 `spark-example/`（插件体系参考实现：多视图、应用通知、签名演示）。

C11 参考插件三件套（共同体/公共事务线，见 wiki/architecture/community-affairs.md §9 C11）：

- `spark-affairs/`：公共议题客户端（事务墙/议题详情/发起/贡献与投票，sdk.affairs + message-card 卡片）；
- `spark-verify-hoa/`：验证插件示例·业主场景（申请人材料引导 + 验证人签发凭证，sdk.credentials，强制 L1 开源语义见 spark-plugin.json 注释）；
- `spark-threshold-vouch/`：门槛插件示例·担保链（N 名参与者签名担保 → 产出「是否满足门槛」的签名证明，内核只验证产物）。

构建：`npm run build:affairs` / `build:verify-hoa` / `build:threshold-vouch`（vite 构建 + `scripts/copy-plugin-dist.mjs` 收尾自检）；单测随 `code/app` vitest 一起执行。

## 边界约定

- 插件**只依赖**独立 SDK 包 `@spark/plugin-sdk`（`code/packages/plugin-sdk`，相对路径引用），禁止 import 壳层（`app/src`）任何模块；
- 壳层不得 import 任何具体插件模块；新增插件只需创建目录与入口文件，无需修改内核。

## 完整文档

开发环境搭建、SDK 接口、同步策略、打包发布全流程见 wiki：[插件开发指南](https://github.com/welyin/spark.wiki/blob/master/dev/plugin_development.md)（设计背景：[插件体系](https://github.com/welyin/spark.wiki/blob/master/design/plugin_system.md)）。

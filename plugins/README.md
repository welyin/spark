# Spark 插件目录

本目录存放官方插件源码（与 `code/app/` 平级）。每个插件一个子目录：

```
plugins/<id>/
  manifest.json      ← 声明式清单（唯一事实源）
  spark-plugin.json  ← 仓库声明文件（分发信任锚点雏形）
  index.ts           ← 入口：export default definePlugin({ manifest, setup })
  *.vue / *.ts       ← 视图与业务逻辑
  tests/             ← 插件单测（随 code/app 的 vitest 一起执行）
```

## 边界约定

- 插件**只依赖**独立 SDK 包 `@spark/plugin-sdk`（`code/packages/plugin-sdk`，相对路径引用），禁止 import 壳层（`app/src`）任何模块；
- 壳层不得 import 任何具体插件模块；新增插件只需创建目录与入口文件，无需修改内核。

## 完整文档

开发环境搭建、SDK 接口、同步策略、打包发布全流程见 wiki：[插件开发指南](https://github.com/welyin/spark.wiki/blob/master/dev/plugin_development.md)（设计背景：[插件体系](https://github.com/welyin/spark.wiki/blob/master/design/plugin_system.md)）。

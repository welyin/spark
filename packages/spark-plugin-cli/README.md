# spark-plugin-cli

Spark 插件工具链（plugin-dist §9）：manifest `kind`/`libraries` 线形校验、vendor 树哈希锁定（lock）、双产物构建（build：安装包 .spkg + SBOM + 锚定签名材料 / 库包目录）、构建期核验（verify）。

## 用法

```bash
node packages/spark-plugin-cli/bin/spark-plugin-cli.mjs lock   --pluginId <id>     # 或 --dir <插件工程目录>
node packages/spark-plugin-cli/bin/spark-plugin-cli.mjs verify --pluginId <id>
node packages/spark-plugin-cli/bin/spark-plugin-cli.mjs build  --pluginId <id> --mode app|library [--outputDir <dir>]
```

样例工程见 `examples/sample-app`（安装包形态）与 `examples/sample-lib`（库包形态）。

## 测试前置（重要）

本包**无独立 devDependencies**：测试脚本硬依赖主前端工程的依赖目录——

```bash
cd app && npm install          # 前置：app/node_modules 须已安装（vitest 在其中）
cd packages/spark-plugin-cli && npm test
# 等价于：cd app && node node_modules/vitest/vitest.mjs run spark-plugin-cli
```

测试经 `app/vitest.config.ts` 的 `../packages/**/*.test.ts` include 收编，app 全量单测（`cd app && npm run test:unit`）亦覆盖本包。

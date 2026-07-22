# Spark 插件

插件源码与打包发布工具。当前插件：

- `weibo-core/` —— 组织微博基础插件（`plugin:weibo-core`）

## 市场链路总览

```
打包(本目录) → 签名产物 → 上传 GitHub Release(workflow) → 市场服务(src-tauri) → 安装/升级/启停 → 使用
```

各环节落点：

| 环节 | 位置 |
|---|---|
| 打包脚本 | `plugins/scripts/build-weibo-package.mjs`（`npm run build:weibo`） |
| 本地发布产物 | `desktop/dist-market/plugins/<pluginId>/`（gitignored） |
| 发布 workflow | `code/.github/workflows/release-plugin-weibo.yml` |
| 市场服务（Rust） | `desktop/src-tauri/src/market/`（命令层 `commands/market.rs`） |
| 前端适配层 | `desktop/src/api/index.ts` 的 `pluginMarket.*`（AppsPage 零改动） |

## 1. 打包与签名

```bash
cd code/plugins
npm run build:weibo          # 等价于 node scripts/build-weibo-package.mjs
# 常用参数：--pluginId weibo-core --version 0.2.0 \
#           --repository owner/repo --releaseTag v0.2.0 --outputDir desktop/dist-market/plugins/weibo-core
```

产物（输出到 `desktop/dist-market/plugins/weibo-core/`）：

- `spark-plugin-weibo-core-<version>.spkg` —— JSON 包：`{pluginId, domain, version, files:[{path, sha256, size, contentBase64}]}`
- `update-manifest.json` —— 更新清单（市场服务消费；未传 repository/releaseTag 时资产 URL 为 `file://` 本地路径）
- `update-manifest.sig` —— 清单的 Ed25519 分离签名（base64）
- `update-manifest.pub.pem` —— 签名公钥（SPKI PEM，核对信任链用）
- `plugin-checksums.txt` —— sha256 校验清单

签名私钥按以下优先级取：

1. 环境变量 `SPARK_PLUGIN_SIGNING_PRIVATE_KEY`（PEM 全文）
2. `<workspace>/.secrets/spark-update-signing-private-key.pem`
3. `<workspace>/desktop/.secrets/spark-update-signing-private-key.pem`（旧仓库约定，只读沿用）

## 2. 信任链（验签）

市场服务（`src-tauri/src/market/trust.rs`）持有信任公钥：

- 内置默认公钥（与旧 TS 主进程 `plugin-market/trust.ts` 同一枚）
- 环境变量 `SPARK_PLUGIN_UPDATE_PUBLIC_KEY_PEM` 可整体覆盖，`@@` 分隔多枚，任一通过即可

> 注意：当前内置公钥与本机 `desktop/.secrets` 里的签名私钥**不对应**。本地用
> `.secrets` 私钥打包的产物做安装验证时，需把 `update-manifest.pub.pem` 的内容经
> `SPARK_PLUGIN_UPDATE_PUBLIC_KEY_PEM` 注入市场服务（或在测试里显式注入信任钥）。
> 正式发布前应统一密钥对并更新内置公钥。

验签复用内核原语 `spark_core::identity::verify_ed25519_signature`（Ed25519 detached）；
PEM→原始公钥的 SPKI 解析在市场服务内完成。

## 3. 上传发布（GitHub Release）

`code/.github/workflows/release-plugin-weibo.yml`（code/ 尚未 git init，启用后生效）：

- 触发：发布 Release（`release: published`）自动跑，或 `workflow_dispatch` 手动指定 tag/version
- 步骤：checkout → 打包签名（私钥来自仓库 secret `SPARK_PLUGIN_SIGNING_PRIVATE_KEY`）→
  重命名为 `spark-plugin-<id>-manifest.json/.sig/.pub.pem/-checksums.txt` → 上传 Release 资产
- 上传后远端清单地址即目录里的 `updateManifestUrl`（`.../releases/latest/download/spark-plugin-weibo-core-manifest.json`）

## 4. 市场安装/升级/启停

前端 `AppsPage` → `window.electronAPI.pluginMarket.*` → Tauri 命令
`plugin_market_{list,check_updates,install,upgrade,set_enabled}` → `market::PluginMarketService`。

- **清单解析优先级**：本地 `desktop/dist-market/plugins/<id>/`（`update-manifest.json` + `.sig` 齐备）
  优先 → 否则目录声明的远端 URL。`file://` 与 `/` 开头按本地文件读；`http://` 一律拒绝；
  其余按 https 下载（reqwest blocking + native-tls 系统信任库）。
- **安装**：验签 → 下载/复制 .spkg 到 `<app_data_dir>/plugins/<id>/packages/` →
  校验 sha256/size → 落状态（`grantedPermissions = 基础权限 ∪ 声明∩高级权限`）。
- **状态文件**：`<app_data_dir>/plugin-market-state.json`；更新探测仅驻留内存。
- **启动对账（reconcile）**：本地 bundle 验签通过 → 标记已安装；否则插件源码目录
  `code/plugins/<id>/`（含 `manifest.ts`）→ 标记 `bundled-dev-source`。

## 5. 端到端验证（file://）

```bash
# 1) 打包（产出真实 .spkg + 签名产物）
cd code/plugins && npm run build:weibo

# 2) 市场服务单测 + 真实产物 e2e（opt-in）
cd ../desktop/src-tauri
cargo test --lib
SPARK_MARKET_E2E_RELEASE_DIR=$PWD/../dist-market/plugins \
SPARK_MARKET_E2E_PUBLIC_KEY_PEM="$(cat ../dist-market/plugins/weibo-core/update-manifest.pub.pem)" \
  cargo test --lib e2e_real_release_artifacts
```

e2e 覆盖：默认公钥拒装（反）→ env 公钥 reconcile 标装（正）→ file:// 复制安装 →
.spkg 内逐文件 sha256/size 一致性 → 同版本 check 为 up-to-date。

## 6. 发版 checklist（插件版本号有三处需同步）

1. `plugins/weibo-core/manifest.ts` 的 `version` 与 `package.packageName`
2. `desktop/src-tauri/src/market/catalog.rs` 的目录条目（`version` / `package.*`）
3. `desktop/src/api/index.ts` 的 `PLUGIN_CATALOG`（vendored 静态目录）

随后 `npm run build:weibo -- --version <x.y.z>` 打包，发 Release 触发 workflow 上传。

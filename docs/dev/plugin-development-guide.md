# 插件开发指南

> 面向新 monorepo（[welyin/spark](https://github.com/welyin/spark)）的插件开发指南。设计背景见 `插件体系`（wiki architecture/plugins/plugin_system.md）；同步策略语义见 `同步与存证`（wiki architecture/sync/sync-and-evidence.md）；分发协议见 [插件分发规格](../protocol/plugins/plugin-dist.md)；应用消息协议见 [p2p 消息规格](../protocol/)。
>
> **本文档是活体文档**：随插件体系里程碑逐步补全，只描述当前真实可用的能力。

## 1. 目标与原则

- 插件按容器（个人空间/组织空间）隔离运行，只能访问自身实例的数据；
- 默认策略：插件数据同步行为由**声明式 API（`declareCollection`）**显式声明（`scope`/`accounts`/`devices`），不再靠 payload 里有无 `orgId` 自动推导；旧通道 `sdk.docs.defineCollection` + `syncStrategy` 仍可用，但新开发应以 `插件数据 API`（wiki architecture/plugins/plugin-data-api.md） 为准；
- 插件只专注业务逻辑——签名、存证、同步、副本都由内核与 SDK 处理；
- **边界双向收敛**：内核代码（App.vue、main.ts 等）不得 import 任何具体插件模块；插件不得 import 壳层（`app/src`）任何模块，只依赖独立 SDK 包 `@spark/plugin-sdk`（`code/packages/plugin-sdk`，monorepo 内相对路径引用）。

## 2. 开发环境

- 依赖：Node.js（>= 18）、Rust 工具链、Tauri 2 系统依赖（见 Tauri 官方文档）；
- 安装与启动：
  ```bash
  cd code/app
  npm install
  npm run dev          # 纯前端预览（浏览器，无内核，mock 数据）
  npm run tauri:mock   # Tauri 窗口 + mock 模式
  npx tauri dev        # Tauri 窗口 + 真实内核（完整链路）
  ```
- 插件构建：插件以 **dist 构建产物** 在 iframe 沙箱内运行，改动源码后需重新构建（以 spark-example 为例）：
  ```bash
  cd code/plugins
  npm run build:example    # vite 多入口构建 → spark-example/dist/
  ```
  dev 链路经 vite 中间件直读 dist，重新构建后重开插件视图即生效，无需重启壳；
- 单测：`cd code/app && npm run test:unit`（vitest 同时收集 `src/**`、`../plugins/**`、`../packages/**` 下的测试，插件测试随 app 工程一起跑）。

## 3. 目录结构与入口契约

插件源码在 `code/plugins/<id>/`（与 `code/app/` 平级），以 spark-example 为例：

```
plugins/spark-example/
  manifest.json      ← 声明式清单（唯一事实源）
  spark-plugin.json  ← 仓库声明文件（分发信任锚点雏形）
  index.ts           ← 主入口：connectPluginBridge 握手 + 按 __sparkPluginView 多视图分发
  post-card.ts       ← message-card 视图引导（独立 bundle）
  ExampleView.vue    ← 主视图（app）
  PostCard.vue       ← 消息卡片视图（message-card）
  model.ts           ← 数据模型与约束（如 260 字截断）
  service.ts         ← 业务逻辑（经 SDK 读写）
  vite.config.ts     ← 多入口构建配置
  dist/              ← 构建产物（gitignored）：views/main.js + views/post-card.js + chunks/ + assets/
  tests/             ← 插件单测
```

入口契约（插件只依赖 SDK 包，**不 import 壳层任何模块**）。iframe 沙箱形态下壳层不再编译期装载插件：插件 bundle 在沙箱 iframe 内经 `connectPluginBridge` 握手拿到 SDK 与运行上下文，写入全局注入点 `window.__sparkPluginSDK` 后**自行挂载**到宿主提供的 `#app` 容器——插件自挂载，宿主只负责握手与容器：

```typescript
// plugins/spark-example/index.ts（精简）
import { connectPluginBridge } from '../../packages/plugin-sdk/src/bridge/client';
import manifestJson from './manifest.json';

const manifest = manifestJson as PluginManifest;

// 宿主 srcdoc 固定加载 views/main.js；所有视图共用此入口，
// 按宿主注入的 window.__sparkPluginView 分发到主视图 / 卡片视图
async function bootstrapMainView(): Promise<void> {
  const { sdk } = await connectPluginBridge({
    pluginId: manifest.id,
    viewId: manifest.entryView,
    sdkVersion: manifest.sdkVersion
  });
  window.__sparkPluginSDK = sdk;
  createApp(ExampleView).mount(document.getElementById('app')!);
}
```

构建约定（参考 `spark-example/vite.config.ts`）：vite lib 模式多入口产出 ESM bundle——`dist/views/main.js` 为固定主入口，`dist/views/<viewId>.js` 为各附加视图（如 post-card.js），共享代码自动切为 `dist/chunks/*.js`；vue/element-plus/SDK 全部打进 bundle（框架自包含，CSP 禁止远程 script-src），无 external。

> 旧编译期加载（`import.meta.glob` + `definePlugin` 注册）已退役，iframe 沙箱是唯一加载路径；市场安装的 .spkg 同样经 plugin:// 源服务动态加载（安装包优先、内置 dist 兜底）。`definePlugin` 契约保留在 SDK 包中作为第三方插件约定。

## 4. 插件清单（manifest.json）

声明式 JSON（不再是 TS），以 spark-example 为例：

```json
{
  "id": "spark-example",
  "domain": "plugin:spark-example",
  "name": "示例插件",
  "version": "0.1.3",
  "entryView": "default",
  "description": "Spark 插件体系参考实现：组织微博（管理员发帖、成员评论），演示存储/签名/应用消息/消息卡片等核心能力。",
  "supportedSpaces": ["org"],
  "category": "social",
  "requires": {
    "platforms": ["desktop"]
  },
  "views": [
    { "id": "default", "type": "app", "title": "微博" },
    { "id": "post-card", "type": "message-card", "title": "帖子卡片" }
  ],
  "permissions": ["storage:read", "storage:write", "org:read", "org:sync", "message:app", "identity:sign"],
  "sdkVersion": "1",
  "package": {
    "updateManifestUrl": "https://github.com/welyin/spark/releases/latest/download/spark-plugin-spark-example-manifest.json",
    "packageName": "spark-plugin-spark-example-0.1.3.spkg"
  }
}
```

- `supportedSpaces`：插件支持的容器类型（`'personal'` / `'org'`），未声明默认 `['org']`；
- `category`：插件分类，用于市场展示分组，可选值：`"ai-assistant"`（AI 助手）、`"social"`（社交）、`"tool"`（工具）、`"game"`（游戏）、`"foundation"`（基础/框架）。该字段与 `supportedSpaces` 正交——category 决定展示分组，supportedSpaces 决定数据作用域；
- `requires`：运行时平台约束，可选。`requires.platforms` 声明支持的平台列表（`"desktop"` / `"mobile"`），不声明则全平台可用。安装时内核校验：当前平台不在声明列表中则阻止安装；
- `window`：PC 窗口默认尺寸，可选（插件级，不做 per-view）。PC 端插件在可拖动/缩放的桌面窗口中打开（最小夹取 320×220，**插件须响应式**），`window.defaultWidth` / `window.defaultHeight` 声明初始尺寸（px，合法范围宽 320–3840 / 高 220–2160），不声明或越界按壳层默认 880×620；移动端全屏打开，忽略本字段。声明示例（窄高形的 AI 聊天插件）：`"window": { "defaultWidth": 480, "defaultHeight": 680 }`。仓库锚定分发的插件在 `spark-plugin.json` 声明同名字段（[插件分发规格](../protocol/plugins/plugin-dist.md) §2.1）；
- `views`：槽位渲染件声明，两种类型均已生效——`app` 主视图（全页 iframe）与 `message-card` 消息卡片（聊天内限定区域 iframe，能力面按 view 裁剪）；`entryView` 必须存在于 `views` 中；
- `permissions`：权限声明，语义见 `插件体系·权限模型`（wiki architecture/plugins/plugin_system.md）（伞权限按容器分解）。`message:app`（应用会话读写）与 `identity:sign`（域身份签名，使用时询问）均已生效，安装授权后落 `grantedPermissions`；
- `sdkVersion`：SDK 契约版本，桥握手时协商（v1 协议下要求与宿主精确一致，不兼容拒绝加载）；
- `id` 将随分发模型迁移为规范化仓库地址（见 `spark-plugin.json` 的 comment 字段与 [插件分发规格](../protocol/plugins/plugin-dist.md) §1）。

`spark-plugin.json`（仓库声明文件）是市场广播/验证的信任锚点，字段规范见 [插件分发规格](../protocol/plugins/plugin-dist.md) §2。

## 5. SDK 接口

视图内从 SDK 包获取实例（桥握手完成时插件入口已把 SDK 写入全局注入点；视图挂载早于握手完成时 `ensurePluginSDK` 挂起等待，超时明确报错）：

```typescript
import { ensurePluginSDK } from '../../packages/plugin-sdk/src';
const sdk = await ensurePluginSDK();
```

- `sdk.domain: string` —— 当前插件域身份；
- `sdk.docs` —— 文档接口（**主用**）：`defineCollection(collection, schema)` / `get` / `put` / `delete` / `query`；
- `sdk.runtime` —— `currentRoot()` / `listMineOrganizations()` / `syncOrganizationData(orgId)`（高级权限 `org:sync`）；
- `sdk.identity` —— `sign(payload)`（高级权限 `identity:sign`，使用时询问；返回 `{domain, domainId, publicKey, signature, payloadHash}`）/ `verify(payload, signature, publicKey)`（纯函数免权限）；
- `sdk.messages` —— 应用会话（高级权限 `message:app`）：`sendAppMessage(payload, card?)` / `listAppMessages()` / `markRead()` / `onCardAction(handler)`，卡片侧 `triggerCardAction(actionId, data?)` / `requestCardHeight(height)`，详见 `应用会话与消息卡片`（wiki architecture/plugins/plugin-app-messages.md）；
- `sdk.p2p` —— `start()` / `stop()` / `broadcast(topic, message)`（高级权限 `network:broadcast`；多数业务不需要，优先用 docs 自动同步）；
- `sdk.evidence` —— `headHash()` / `verify()`（只读核验）；
- `sdk.events` —— `subscribe(event, handler)` / `unsubscribe(event, handler?)`（桥事件订阅）；
- `sdk.sys` —— 系统能力代理（内核外呼，desktop 限定的高危权限）：`exec(program, args, workdir?)`（`system:exec`）/ `fetch(url, opts)`（`network:fetch`）/ `fetchStream(url, opts)`（流式 HTTP，同 `network:fetch`；返回 `FetchStreamHandle`：`onChunk` 逐块回调、`done` 完成 Promise、`cancel` 退订；五层管道与边界见 `插件流式输出管道`（wiki architecture/plugins/plugin-streaming.md））。

`sdk.messages` 与 `sdk.events` 仅 iframe 桥模式注入（`PluginSDK` 上为可选字段）；`sdk.space` 空间上下文（type/id/orgId）已实现，当前 space 经桥握手 ctx（`PluginContext.space`）注入。

SDK 调用经 postMessage 桥到宿主，再经 invoke 适配层桥到内核；权限过滤在壳层桥分发器逐调用执行（三重过滤：grantedPermissions ∩ view 裁剪 ∩ 当前空间），域隔离与持久化在内核侧，渲染端与插件均无法伪造身份。

## 6. 存储与同步声明（必填）

**旧通道（当前可用）**：`sdk.docs.defineCollection` + `syncStrategy`，每个集合写入前必须声明：

```typescript
await sdk.docs.defineCollection('weibo_posts', { syncStrategy: 'append-only' });
```

- `append-only`（默认推荐）：仅追加，自动链式存证；远端重复按载荷哈希幂等去重，覆盖/删除被拒；
- `lww`：可覆盖状态（如组织配置 `weibo_org_config`）；
- `governance: true`：治理数据（投票、成员、账目）强制 append-only + 存证，声明 lww 会被拒绝；
- 声明持久化、不可变更、幂等——插件启动时统一声明一次即可（参考 `spark-example/service.ts` 的 `ensureCollectionsDeclared`）。

**新 API（personal scope 已可用）**：`sdk.data.declareCollection`，详见 `插件数据 API`（wiki architecture/plugins/plugin-data-api.md）。新版以 `scope`（sync/local）、`accounts`（all-members/data-accounts）、`devices`（all/pc-backup/pc-only/mobile-only）、`confidentiality`（filtered/encrypted，默认 `filtered`）和 `merge`（lww-record/append-only/whole，默认 `lww-record`）声明数据同步策略，取代旧三策略模型。**个人空间插件现已可用**（QuickJS 后台与 iframe 视图双通路，2026-08-10）；组织空间的 accounts/confidentiality 轴随 orgsync O2 启用（O2 已于 2026-09 双端/三端联调验收收口，见 wiki testing/org-o2-acceptance.md）。新开发建议参考 `插件数据 API`（wiki architecture/plugins/plugin-data-api.md） 规划数据模型。

## 7. 数据同步机制

**旧通道（当前可用）**：组织数据带上 `orgId` 即自动同步——内核扫 `doc:plugin:` 前缀键，取 `payload.orgId` 与目标组织一致且未标记不同步的文档随 org-share/org-pull 快照同步（见 `组织同步协议`（历史档案，wiki protocol/org/org-sync.md） §11）。

本地数据（草稿、临时 UI 状态）需显式标记，支持以下形式之一：`__sync: false`、`__sync: {disabled:true}`、`__sync: {mode:"local"}`、`__sync: {strategy:"local"}`。

**新设计（personal scope 已落地）**：同步范围由声明决定——`scope: "sync"` 的集合按 `accounts`/`devices` 轴参与同步（个人空间 = 自设备间 pdsync，组织空间 = 复制组 orgsync，O2 已收口落地），`scope: "local"` 的集合保持设备本地（`ldoc:` 命名空间，永不进入同步流量）。详细语义见 `插件数据 API`（wiki architecture/plugins/plugin-data-api.md）。

## 8. 数据设计建议

- 默认选 append-only，确实需要覆盖语义才声明 lww；
- append-only 集合的文档 id 必须全局唯一且可重建（如 `post_<ts>_<rand>`）；同 id 不同内容会各自保留并告警；
- 组织业务数据必带 `orgId`；不要给组织数据加不同步标记；
- 权限控制在业务层实现（如 spark-example：仅主管理员可发帖，全员可评论，均限 260 字）；
- 防抵赖操作（投票、签名）先 `sdk.identity.sign()` 再写入，签名与公钥随文档存储供跨节点验签（spark-example 发帖签名即此模式，用户拒绝授权时降级不阻断）。

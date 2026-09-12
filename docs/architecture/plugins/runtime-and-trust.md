# 架构设计：插件运行时与信任（沙箱、分发与库包）

> 模板：产品目标 → 现状 → 差距 → 目标设计 → 迁移路径 → 验收。对应产品文档：[../../product/plugins/runtime-and-trust.md](../../product/plugins/runtime-and-trust.md)（定稿）。覆盖 product/todo #25 及 #17 的宿主侧。

## 一、产品目标（来自 product/plugins/runtime-and-trust.md）

1. 双运行时隔离：iframe 视图沙箱（禁外部拉取代码、禁伪装系统界面）+ QuickJS 后台沙箱（资源上限、超时可杀）；一插件一实例按容器隔离；
2. 能力最小化：标准能力接口 + 事先声明 + 三重过滤（已授权 ∩ 视图允许 ∩ 空间允许）；私钥永不离开内核；隐私红线；**文件与命名空间隔离**（内容寻址 blob 池，插件只见自己命名空间）；
3. 熔断与可恢复（看门狗 / 崩溃环 / 错误只计数）；可达性不依赖装没装插件（强制纯文本摘要）；分发垃圾防线（PoW / 中继资历 / 懒惰核查 / 有效期）；iOS 平台限制下动态加载降级为"审核后随版本内置"；
4. 分发信任：插件身份 = 开源仓库地址；仓库写权限即签名能力；L0 / L1 / L2 安装通路信任级（验证类 ≥L1 硬规则）；市场只是广播索引、镜像只是通路；**市场界面也是可替换应用**（验签 / 信任级 / 仓库锚定永留内核协议层）；
5. **库包与构建期复用**（product/todo #25）：库包纯代码无数据域；依赖以仓库地址 + commit/哈希为准、全量打进安装包；运行时单实例沙箱；数据域归组合者；SBOM 依赖树安装时展示；工具链双产物（安装包 / 库包）。

## 二、现状（核实日期：2026-09-08）

**wiki**：`plugin_system.md`、`plugin-runtime.md`、`plugin-data-api.md`、`plugin-release.md`、`plugin-background.md`、`plugin-streaming.md`。

**代码**：

- 双运行时已落地：iframe 视图沙箱 + QuickJS 后台沙箱（`rquickjs`，独立实例 + 资源上限）+ bridge（call / subscribe / event，eventPump 转发）；
- 能力系统：能力声明 + 调用校验；`plugin_data` 8 命令（declareCollection 数据域）；
- **信任级已实现**：`app/src-tauri/src/market/trust.rs`（L0 侧载 / L1 仓库锚定 / L2 官方签名，含验证类 ≥L1 判定）、market 12 命令（安装 / 验签 / 仓库锚定核对）；
- **熔断已落地**：`app/src/plugin/watchdog.ts`（ready 后 5s 心跳、连续 3 次超时标无响应）、`disabled.ts`（60s 窗 ready 前错误 ≥3 / 无响应 ≥3 自动停用、入口置灰）；错误上报只计数不参与自动停用——与 product 逐条对应；
- **垃圾防线已落地**：plugin-release（hashcash 前 20bit、TTL 30 天 + LRU、懒惰核查、verified 条目随机排序）；
- spark-example / spark-moments / ai-chat 三个插件在生产运行（另有 spark-affairs / spark-threshold-vouch / spark-verify-hoa 三个在研工程，壳层无引用、catalog 未登记）；
- **没有**：库包角色与依赖机制、SBOM、构建工具链（插件打包目前为手工 / 脚本）、市场界面插件化（市场在壳层）、blob API 的插件命名空间隔离（product runtime-and-trust 能力最小化节）、iOS 降级的形态落实。

## 三、差距

| # | 差距 | 出处 |
| --- | --- | --- |
| G1 | 库包机制（角色、依赖声明、哈希锁定、双产物、SBOM）全缺 | product/todo #25 |
| G2 | 市场界面在壳层，未按"可替换应用"插件化 | product/todo #17 配套 |
| G3 | blob API 的插件命名空间隔离未明确（文件对象只认集合，未见 blob 命名空间约束） | product/runtime-and-trust 能力最小化节 |

## 四、目标设计

### 4.1 库包机制（G1，#25 全部）

**manifest 扩展**：

```jsonc
{
  "kind": "app" | "library",          // 默认 app；library = 纯代码库，不单独运行
  "dependencies": [                    // 仅 app；构建期解析
    { "repo": "github.com/acme/spark-kanban-lib",
      "commit": "<40hex>",             // 精确提交锁定（哈希必验，不接受分支/标签浮动引用）
      "integrity": "<sha256>" }
  ]
}
```

规则（与产品 §五逐条对应）：

1. **library 角色**：无数据域、无实例、不可安装运行（市场只对 app 类目展示）；可含 UI 组件 / 领域逻辑 / schema；
2. **依赖以仓库 + commit 为准**：构建工具从仓库锚定拉取并核对 integrity；**不接受包注册中心名为准的引用**（名字可抢注，URL 不可抢注）——npm registry 只作传输缓存；
3. **全量打进安装包**：构建产物 = app 代码 + 依赖树全量内联 + **SBOM**（`sbom.json`：每个依赖的 repo / commit / integrity）；运行时禁止外部拉取代码的既有红线不变；
4. **数据域归组合者**：library 内代码操作的数据写入宿主 app 的命名空间（library 自身无域）；
5. **安装时 SBOM 展示**：安装确认页列出完整依赖树（各库仓库地址 + commit）——供应链透明；
6. **工具链**：`spark-plugin-cli`（新组件）双产物——`build --app`（安装包 = 内联依赖 + SBOM + manifest + **锚定签名材料**，与 plugin-release 仓库锚定规格衔接：构建产出声明文件与签名，发布时锚定仓库地址）与 `build --library`（库包 = 源码包 + 版本 tag；发布形式为 git 仓库 release 或 npm 包，Spark 不自建包仓库）。安装时 SBOM 展示落点：壳层安装确认页先行，A34 市场插件化后归市场插件呈现。

### 4.2 市场界面插件化（G2）

市场前端从壳层迁为默认内置插件（同聊天 / 通讯录的 #17 路径）；**内核保留**：签名验签、信任级判定（trust.rs）、仓库锚定核对、镜像哈希校验——这些能力的命令面（market 12 命令）即市场插件的数据源，任何第三方市场前端复用同一命令面（信任根不可外包）。**前置工作（本篇登记）**：market 12 命令须先按 communication §4.1"等语义移植"纪律暴露为桥模块（`sdk.market` + 权限位 + 事件）——现状 bridge CALL_PERMISSIONS 表无 market 模块，插件无法经 SDK 触达市场面；该工作入 A34 任务面。

### 4.3 blob 命名空间隔离（G3）

blob API（foundation/personal-data 篇 §4.2 落地后）按插件命名空间隔离：`blob:put` 写入即在本插件命名空间登记簿登记 cid 引用，`blob:get` 仅能读本插件登记簿内的 cid（cid 本身无前缀，隔离落在登记簿）；内核在 API 边界强制（与 declareCollection 的数据域隔离同层实现）；跨插件文件访问只有契约接口一条路（plugin-types 篇）。

## 五、迁移路径

1. manifest 新字段向后兼容（无 kind = app、无 dependencies = 无依赖，行为不变）；
2. 库包机制随 `spark-plugin-cli` 首个版本交付；catalog「项目」插件为第一个组合应用（其 SBOM 即验收样例）；
3. 市场插件化在 A19/A42（聊天、通讯录、组织管理、文件界面插件化）之后（architecture/todo 批次三）；blob 命名空间随 blob 层（A1）一并落地。

## 六、验收

- **向量**：manifest 新字段解析（kind / dependencies / integrity 校验失败必拒）；SBOM 线形；发布物之外的打包格式（安装包内联完整性）；
- **单测**：依赖解析（commit 锁定、浮动引用拒绝、integrity 不符必拒）、library 不可安装 / 无数据域、blob API 命名空间越界拒绝；
- **集成**：`spark-plugin-cli` 构建「项目」组合应用 → SBOM 展示 → 安装运行（单实例沙箱）；第三方市场前端（示例）经 market 命令面完成浏览 / 安装 / 验签全流程；
- **回归**：iframe / QuickJS 隔离、能力三重过滤、trust.rs L0/L1/L2 判定、三个既有插件功能全绿。

---

> 关联：product/todo #25（§4.1）、#17（§4.2）；architecture/plugins/plugin-types.md（契约 = 运行时互通，库包 = 构建期复用，两者互补的完整图景）；architecture/community/communication.md §4.1（界面插件化的 API 面路径复用）；`plugin-release.md`（仓库锚定与签名能力规格权威）。

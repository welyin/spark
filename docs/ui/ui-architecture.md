# 新 UI 架构设计：从产品设计到代码结构的落地方案

> 状态：草稿 v0.7 ｜ 分册：UI 设计（架构与落地篇；…v0.6 卡片栈预览按内存自适应；v0.7 对齐 shell-desktop v0.7 / L12：PC 左栏入口（消息/事务/我的/设置/应用管理）由"整页 tab"改为盖在常驻桌面上的全局模态层——顶级对话框＋遮罩、单实例，桌面窗口只承载业务插件，手机端仍全屏 tab）
>
> 本篇回答一个问题：**[README](README.md) / [domain-space](domain-space.md) / [shell-desktop](shell-desktop.md) / [shell-mobile](shell-mobile.md) 定义的新 UI，在现有代码（`code/app`）上怎么长出来。** 前三篇讲"长什么样"，[visual-style](visual-style.md) 讲"用什么皮"，本篇讲"用什么骨架、按什么顺序改"。开发任务拆解见 [todo](todo.md)。
>
> 事实来源：`code/app/src` 现状（见 §2）、[wiki/ui](../../wiki/ui/README.md) 的现行实现记录、[ark-desktop-main](../../ark-desktop-main) 的窗口模型蓝本（已被 [shell-desktop §3.5](shell-desktop.md) 论证采用）。

---

## 一、架构主张：一次"换导航"而不是"重写前端"

新 UI 的本质变化只有三件，其余都是复用：

1. **一级入口换血**：旧 `消息 · 通讯录 · 应用` → PC 左栏 `搜索 · 消息 · 事务 · 空间`（"我的"在栏底、不占导航位，顺序见 L4）、手机底栏五 Tab `消息 · 事务 · 空间 · 应用 · 设置`（M4/M9）；通讯录、公共议题都是空间插件、不挂进消息 / 事务页签，应用市场收进空间；**PC 点这些左栏入口开全局模态对话框、不切整页（L12，见 §4.1）**；
2. **"空间"入口内长出桌面**：主工作区里多一个"桌面 + 窗口管理器"层，插件从"整页 tab"升级为"窗口 / 全屏 App"；
3. **事务成为一等入口**：新增跨域事务列表页，点击按类型分发到类型插件（外壳不做统一详情页）。

明确不做的事：不重写消息 / 通讯录 / 插件桥 / SDK；不换组件库与样式体系（[visual-style §1](visual-style.md) 五不）；不引入 Vue Router（维持单页 tab 切换架构，见下文 §4.1 论证）。

## 二、现状盘点（代码事实）

### 2.1 已有的、可直接复用的资产

| 资产 | 位置 | 在新 UI 中的角色 |
|---|---|---|
| 外壳骨架（顶栏 44px / rail 64·155 / 768 断点 / 移动底栏 / 栈转场） | `code/app/src/App.vue`、`components/Mobile*.vue`、`styles/app-shell.css` | 原样保留，只换 rail 项与 tab 内容 |
| 插件视图容器（iframe 沙箱 + postMessage 桥 + 权限 + 心跳熔断 + 沉浸式模式） | `components/plugin/PluginIframeHost.vue`、`plugin/`（bridge-dispatcher、sdk-browser、card-actions、watchdog） | **窗口内容的唯一宿主**：新窗口壳套在它外面，桥协议不动 |
| 空间上下文与切换 | `stores/current-space.ts`、`components/SpaceSwitcher.vue`、`MobileSpaceDrawer.vue` | 升级为"域切换器"，语义不变 |
| 移动栈导航与 Android 返回键 | `stores/mobile-nav.ts`（每 tab 一栈）、`MobileBackBar`、`MobilePageTransition` | 手机端"桌面 → 全屏 App"直接走现有 push/pop |
| 消息卡片 → 深链 | `plugin/card-actions.ts`、`components/messages/AppMessageCard.vue` | 扩展为全局深链路由（§4.4） |
| 内置灰度机制（legacy ⇄ 插件版） | `stores/builtin-apps.ts`、`components/plugin/BuiltinAppHost.vue` | 事务页等新旧切换复用同一灰度范式 |
| 设计令牌与双主题 | `styles/tokens.css` 等 | 新 UI 零新色板，只补缺口令牌（[visual-style §10](visual-style.md)） |
| 已存在的业务插件 | `code/plugins/spark-affairs`（含 affair-card 视图）、spark-chat、spark-contacts、spark-moments 等 | 空间桌面的第一批"应用"，无需改造即可进窗口 |

### 2.2 缺口（新 UI 要求、代码里不存在的）

| # | 缺口 | 设计要求出处 | 备注 |
|---|---|---|---|
| G1 | 「事务」一级入口与跨域事务列表 | [README §4.4](README.md) | spark-affairs 现只有"公共议题"插件视图，无全局列表 |
| G2 | 「空间」桌面：图标网格、按空间隔离的布局 | [domain-space §4](domain-space.md) | 现"应用"页是列表，不是桌面 |
| G3 | PC 窗口管理器（多实例、层级、最小化、贴边、iframe 遮罩） | [shell-desktop §3.2/§3.5](shell-desktop.md) | **全仓无任何窗口管理代码**，最大新建块 |
| G4 | 手机端"域桌面 + 全屏 App + 最近任务栈" | [shell-mobile §3](shell-mobile.md) | 可大部分复用 mobile-nav 栈 |
| G5 | 按事务类型分发到类型插件（无统一详情页） | [README §4.4](README.md)、[shell-desktop §2.4](shell-desktop.md) | 需要"事务类型 → 插件"注册表与深链协议 |
| G6 | 当前域上下文条 | [README §6.1](README.md) | TopNavbar 已有 SpaceSwitcher+网络状态，升级为上下文条 |
| G7 | 角标语义：事务 = "等我操作"数 | [README §4.4](README.md) | 需要 affairs 侧提供"待我处理"计数 API |
| G8 | 每空间独立的桌面布局 / 窗口状态本机持久化 | [shell-desktop §3.5(4)](shell-desktop.md) | localStorage 按 `spaceId+设备` 隔离，不跨端 |
| G9 | 与操作系统互拖：文件/文件夹拖入摄取、域文件拖出落盘 | [shell-desktop §3.6](shell-desktop.md) | 走 Tauri 原生 file-drop，Rust 侧读盘/递归/哈希/入库，前端只做落点与进度 |
| G10 | 系统级快捷入口与深链唤起（桌面快捷方式/协议/PWA、单实例路由） | [shell-desktop §3.7](shell-desktop.md) | 复用 §4.4 deep-link；Tauri shortcut + 自定义协议 + 单实例；手机 pin shortcut/Web Clip |
| G11 | 全局组件：全局新建（Cmd/Ctrl+N，随当前域上下文）、快捷键（⌘/Ctrl+1~4/K/F）、通知中心聚合 | [shell-desktop §四](shell-desktop.md) | GlobalSearch 已有 Cmd/Ctrl+K 浮层，新建/快捷键/通知中心为增量 |

### 2.3 与 wiki/ui 现行设计的关系

[wiki/ui](../../wiki/ui/README.md) 记录的是**已上线的现行 UI**（rail：消息/通讯录/应用 + 插件 tab；移动四 tab：消息/通讯录/应用/我的）。新 UI 与它**同构不同形**：外壳骨架、断点、栈导航、插件容器全部继承，差异只在"一级入口集合"与"插件的展示载体"。因此迁移策略定为**渐进替换**（§6），wiki/ui 文档在新 UI 上线前继续作为现行实现的事实来源。

## 三、目标架构总览

```
┌───────────────────────────── 外壳层（稳定骨架，不认识业务） ─────────────────────────────┐
│ RootGate ─► App.vue                                                                     │
│   ├─ 桌面：ContextBar(域/身份/同步)+Rail[搜索·消息·事务·空间 + 栏底我的]+常驻桌面 Main    │
│   └─ 移动：MobileTopBar + MobileTabBar[消息·事务·空间·应用·设置] + 每 tab 栈导航          │
├────────────── 左栏入口层（PC＝全局模态：顶级对话框＋遮罩·单实例 L12；手机＝全屏 tab） ──────────────┤
│   PC：Mine/Messages/Affairs/Settings/AppMgr Dialog（盖在常驻桌面之上，不切整页）                    │
│   手机：同名全屏 tab 页 │ SpacePage(新,内含通讯录/公共议题等插件)                                  │
├───────────────────────────── 桌面层（仅"空间"入口内，新） ───────────────────────────────┤
│   SpaceDesktop                                                                           │
│   ├─ SpaceSwitcher（域切换器，复用改造）                                                   │
│   ├─ IconGrid（图标网格/文件夹/编辑模式，布局按空间持久化）          PC                    │
│   ├─ WindowManager（多窗口：registry + instances + 层级/磁吸/iframe 遮罩）                │
│   ├─ Dock/Taskbar（已开窗口 + 固定项）                            ────────                │
│   └─ 移动形态：AppGrid → 全屏 App（走 mobile-nav 栈）+ 最近任务卡片栈        手机         │
├───────────────────────────── 插件容器层（已有，原样复用） ────────────────────────────────┤
│   PluginIframeHost（iframe 沙箱 + 桥 + 权限 + 熔断 + 沉浸式）◄── plugin-sdk 桥协议不变     │
├───────────────────────────── 全局服务层（新增 2 个，复用其余） ───────────────────────────┤
│   深链路由 deep-link.ts(新) │ 事务注册表 affair-types.ts(新) │ OS 集成 os-integration(Tauri file-drop/shortcut/单实例协议) │ current-space / network… │
└─────────────────────────────────────────────────────────────────────────────────────────┘
```

分层纪律（对应"外壳稳定、内容可换"原则）：

- **外壳层**不 import 任何插件与业务 store 的具体类型，只通过事件 / 注册表交互；
- **桌面层**只依赖"当前空间已装插件清单"这一抽象，不认识插件内容；
- **插件容器层**契约冻结：窗口壳是 `PluginIframeHost` 的**新宿主**，不改桥协议，所有存量插件零改动迁入；
- **全局服务层**沿用 ref 单例 store 惯例（项目无 Pinia），新 store 同风格。

## 四、关键机制设计

### 4.1 导航模型：维持单页 tab，不引入路由库

论证：现行单页 tab 架构（`activeTab` + 移动每 tab 栈）已承载全部页面且被 wiki/ui 定稿；新入口（PC 四入口 ／ 手机五 Tab）只是**替换 tab 集合**。引入 Vue Router 会同时重写 mobile-nav、Android 返回键、深链三处，收益为零。决策：**`activeTab` 取值改为 `mine | messages | space | affairs`（PC；移动端另有 `apps` 与 `settings` 底 Tab，见 M4/M9）**；`contacts` **退出一级、转为各空间内的通讯录插件**（个人好友/组织成员两套人脉分开，消息页只留会话流，发起会话靠搜索/`＋`拉起选人，见 [README §4.2/§八](README.md)）；`apps` **不再降级为 SpacePage 内的"应用市场"图标，而是系统层应用管理**：PC＝rail「应用管理」顶级对话框（L4，与 M4 同一口径），移动＝底栏第四 Tab「应用」（M4/M9 五 Tab 之一）；**空间内（SpacePage / 桌面）的市场入口只承载"为本空间启用"**——从本机已装插件挑选挂到当前空间（组织空间规则类走事务立法、视图类走组织策略），拿到代码 / 装卸 / 更新统一在系统层应用管理完成；`settings / test` 留在 rail 底部（PC）与第五 Tab「设置」内（移动，M9）；PC rail **默认展开 240px**、之后记忆用户折叠选择（[shell-desktop §六决策 1](shell-desktop.md)，现状默认 64px 窄栏需改默认值）。**（v0.7 / L12 修订）** PC 端"消息 / 事务 / 我的 / 系统设置 / 应用管理"不再是 `activeTab` 切出的整页，而是盖在**常驻空间桌面**之上的一层**全局模态（顶级对话框 ＋ 遮罩、单实例、Esc / 点遮罩关）**——桌面与其中的业务插件窗口始终在底层常驻；只有"空间"走桌面、业务插件走桌面窗口（两个例外：点空间项＝切桌面、桌面应用图标＝多窗口，均不套模态）。手机端无此改动，消息 / 事务等仍是全屏 tab 页：同一套页面组件，PC 套模态容器、手机套全屏页。

深链目标地址采用 [shell-desktop §3.7](shell-desktop.md) 定稿的统一寻址语法：`spark://space/<域>/app/<插件>[/object/<对象>]`（另有 https 形态的 Universal/App Link），应用内解析与外部唤起共用同一条 deep-link 服务（§4.4/§4.7），不走 URL 路由库。

### 4.2 PC 窗口管理器（G3，核心新建模块）

直接采用 [shell-desktop §3.5](shell-desktop.md) 已从 ark-desktop-main 论证的**两级状态分离**，落为两个 store：

```
stores/desktop/
  app-registry.ts    // 应用注册表：当前空间已装插件 → Map<appId, AppDef>
                     // AppDef = { id, name, icon, pluginId, view, defaultSize }
                     // 数据源：本机已装清单（AppsPage 的 apps-store）∩ 当前空间启用清单
                     // ——"已安装 ∧ 当前空间已启用"（口径见 domain-space §4.1）
  window-manager.ts  // 窗口实例表：Map<winKey, WinInst>，按空间分桶
                     // winKey = `${spaceId}|${appId}|${seq}`（同应用多实例）
                     // WinInst = { zIndex, minimized, rect? } —— rect 由窗口组件本地持有(参照 ark)
```

行为算法（照抄 ark 已验证项，补齐 ark 未做项）：

- 开窗幂等 + 级联偏移（+20px 阶梯）；单一 `activeId` + 单调 `maxZ`；关/最小化前台窗后激活剩余 z 最大者；最小化用 `v-show` 保活插件运行态；
- **iframe 遮罩**：非激活 / 拖拽中给 iframe 盖透明 mask（ark 已填平的必踩坑）；
- 补齐：右/下边界夹取、贴边磁吸与左右半屏/四分屏、Pointer Events 兼容触屏；**不设同屏窗口硬上限、不自动关窗**（用户自管），仅内存吃紧时非阻断提示 + 非激活窗心跳降频。

组件：`components/desktop/`：`SpaceDesktop.vue`、`DesktopIconGrid.vue`、`WindowFrame.vue`（标题栏 44px 复用 topbar 高，内含 `PluginIframeHost`）、`TaskDock.vue`、`WindowDragResize.ts`（composable）。

持久化：`localStorage["spark:desktop:<spaceId>"] = { wallpaper, dock[], icons[] }`、`localStorage["spark:windows:<spaceId>"] = { open: [...], zOrder }`——**只存本机、按空间隔离、不跨端**（[shell-desktop §3.5(4)](shell-desktop.md)）。**恢复策略已定（§八决策 1）**：重启后只还原"上次开着哪些应用"、每应用恢复为单实例，多实例的 `seq` 不还原。

### 4.3 手机端桌面形态（G4）

不另造机制，复用 mobile-nav 栈：

- `space` tab 的 root = **域列表**（一级）；`pushPage` 进**某域手机桌面**（图标网格/分页/文件夹，长按编辑）；再 `pushPage` 进**全屏 App**（`PluginIframeHost` 全屏 + 沉浸式 chrome，已有能力）；
- 最近任务卡片栈：第一版用"域桌面 → 返回即回栈顶 App"的栈行为近似（App 在栈中保活）；真正的卡片式多任务预览列为二期（概念见 §八问题 3，任务见 [todo 二期](todo.md)）；
- Android 返回键逻辑天然兼容（栈 pop 已有）。

### 4.4 事务入口与类型插件分发（G1/G5）

```
stores/affairs/
  affair-feed.ts     // 跨域事务列表数据：来源内核事务索引，只聚合"与我相关"，含"等我操作"计数（供 tab 角标）
  affair-types.ts    // 类型注册表：Map<affairType, pluginId>，由插件 manifest 新增字段注册；
                     // 构建方式已定（§八决策 2）：宿主扫描已装插件 manifest 自建，内核零改动
services/deep-link.ts // 统一深链（spark://space/<域>/app/<插件>[/object/<对象>]）：
                      // 消息卡片 / 事务列表 / 通知中心 / 系统快捷入口，共用同一条路由
```

- 插件 manifest 增加 `affairTypes: string[]` 声明（SDK 契约扩展，向后兼容：无此字段即不承接事务）；
- 点击事务卡片 → deep-link 查注册表 → 已装：PC 在事务页右栏（或"在所属空间桌面中打开"为桌面窗口）打开该插件视图、移动 push 全屏 App；未装 / 未启用分开给空状态：本机未装→引导去系统层应用管理安装（拿到代码），已装但未在该空间启用→引导启用（[README §4.4](README.md)）；公共议题发现不在此外壳页内，由空间里的公共议题插件承担（[README §八决策 2](README.md)）；上下文/公共身份切换控件也归插件、外壳不设全局切换（[README §八决策 4](README.md)）；
- 深链到达时**先切域**（current-space 已有 `validateCurrentSpace`），上下文条同步（G6）；
- 消息卡片现有 card-actions 改为走同一 deep-link 服务，消灭两套跳转逻辑。

### 4.5 上下文条（G6）

PC：TopNavbar 升级为上下文条 = 域头像/名称 + 域内身份昵称 + 同步状态（现有 SpaceSwitcher + NetworkStatusBar 重组，无新数据源）。移动：MobileTopBar 在"空间"相关页显示当前域。深链切域时联动。

### 4.6 样式与组件纪律

按 [visual-style](visual-style.md)：新元素全部映射到既有 `--spark-*` 令牌与 Element 组件（其 §8 映射表）；动工前先补其 §10 的令牌缺口（`--spark-text-on-color`、`--spark-service`、`--spark-cat-*`、动效令牌、z-index 令牌、桌面/窗口结构常量）——**这是所有 UI 任务的前置任务**。

### 4.7 与操作系统对接（G9/G10，全部走 Tauri 原生能力，不进 iframe）

- **文件互拖（G9）**：监听 Tauri webview 的 file-drop 事件拿本机绝对路径（不用 Web HTML5 拖放——拿不到真实路径、不能递归文件夹）；Rust 侧负责读取、递归、算哈希去重、流式入库（拖入）或解密写出（拖出）；前端 WindowManager 只负责拖放态高亮、"落点即域"判定与进度回执，插件 iframe 不接触本机路径。拖入组织域 / 从组织域拖出都要走可见范围与"明文副本"提示（[shell-desktop §3.6](shell-desktop.md)）。
- **快捷入口与深链唤起（G10）**：Tauri 写入系统快捷方式（.lnk/.desktop/Dock）、注册自定义协议 `spark://` 与 https 关联、开启单实例锁；外部唤起统一交给 §4.4 的 deep-link 服务路由（先切域再开目标），不另起第二套跳转。手机端：Android `requestPinShortcut`/动态快捷方式，iOS 走 Web Clip/Quick Actions/Universal Link。快捷方式只存寻址串、不存密钥与数据，退出组织后指向该域的入口自动失效、路由给"已无权访问"。

### 4.8 移动端诚实呈现（[shell-mobile v0.3](shell-mobile.md) 已定决策的界面落点）

- **第一版不做系统通知栏推送**（不接 APNs/FCM/厂商通道）：仅前台/打开时经 P2P 收取；"我的→通知与设置"与相关界面如实说明这一限制，不假装能实时触达；
- **弱网/离线操作**：当下即用本机私钥签名并入本地队列，联网自动补发、**不二次确认**；消息与事务条目标全程状态 `待发送 → 已上链`，补发被拒（已截止/资格不符/已达标）时给"未被接受及原因"的明确回执，不静默丢弃；
- **不做**手机系统小组件（widget）；
- 实现面：复用现有消息发送状态机扩展"待发送/未接受"态（`stores/messages.ts` 与事务卡片的元数据区），无新数据源，属呈现层工作。

## 五、与插件生态的接口冻结声明

以下契约在新 UI 落地中**只扩展、不破坏**：

| 契约 | 现状 | 新 UI 动作 |
|---|---|---|
| plugin-sdk 桥协议（握手/权限/心跳） | 稳定 | 不动 |
| manifest `views` / `chrome.hostTitleBar` / `supportedSpaces` | 稳定 | 不动 |
| manifest `permissions` | 稳定 | 不动 |
| manifest 新增 `affairTypes`（可选） | 无 | 纯增量，缺省即旧行为 |
| 消息卡片 schema 与 card-actions | 稳定 | 内部改走 deep-link，卡片格式不变 |
| 内置灰度 `spark:builtin-impl:*` | 稳定 | 事务页沿用同范式 |

## 六、迁移路径（四个阶段，每阶段可独立发布）

1. **阶段 0 · 令牌与骨架**：补令牌缺口；`activeTab` 换四入口（空间/事务先放"建设中"占位页）；**通讯录不再占一级 tab、改为空间内插件**，消息页只留会话。——纯外壳手术，风险最低。
2. **阶段 1 · 移动先行**：手机端域列表 + 手机桌面 + 全屏 App（全部复用 mobile-nav 栈）。移动端无窗口管理器，是验证"空间桌面"概念的最低成本路径。
3. **阶段 2 · PC 桌面**：WindowManager + IconGrid + Dock + 持久化；插件 tab 旧入口并行保留一个版本周期后移除。
4. **阶段 3 · 事务**：affair-feed + affair-types 注册表 + deep-link 统一；消息卡片跳转切流；角标语义切换为"等我操作"。

每阶段挂 `spark:builtin-impl` 式灰度开关，可整段回退。

## 七、风险与对策

| 风险 | 等级 | 对策 |
|---|---|---|
| 多 iframe 窗口内存 / 心跳开销 | 高 | 不设硬上限、交由用户自管；非激活窗心跳降频（watchdog 已有钩子）+ `v-show` 保活避免重建 + 内存吃紧时非阻断提示 |
| 窗口拖拽与 iframe 事件冲突 | 中 | ark 遮罩方案直接采用（§4.2），已在蓝本验证 |
| 深链切域导致"误操作到错误空间" | 中 | 上下文条强制联动 + 切域瞬间的域水印闪现（domain-space §5.1） |
| 事务计数（角标）数据源缺失 | 中 | 需内核 affairs 索引提供"待我处理"谓词；落地前以"我持有副本且状态=进行中"近似，文档标注 |
| 旧"应用"页用户习惯 | 低 | 阶段 2 内新旧并行 + 灰度开关 |
| 移动卡片式多任务复杂度 | 低 | 二期，一期用栈行为近似（§4.3） |

## 八、设计决策记录（原待决问题，均已落定）

1. **窗口恢复为单实例**：`spark:windows:<spaceId>` 重启恢复时只还原"上次开着哪些应用"，每个应用恢复为一个实例，多实例的 `seq` 不持久化还原（§4.2）。
2. **事务类型注册表由宿主自建**：宿主扫描已装插件 manifest 的 `affairTypes` 字段在本地构建 `Map<affairType, pluginId>`，内核零改动、零下发（§4.4）。
3. **最近任务卡片栈预览＝按内存自适应混用两种方案**：卡片栈即手机系统"最近应用"界面的同款形态——从域桌面底部上滑调出一叠卡片，每张是一个已打开全屏插件 App 的预览，左右滑切换、上滑关闭（[shell-mobile §3.2](shell-mobile.md)）。一期用 mobile-nav 栈行为近似（§4.3）；二期实现卡片本体时，**内存充裕的会话用活 iframe 缩略（所见即所得），内存吃紧时降级为静态截图（进入时刷新）**——按设备实际内存 / 当前 iframe 占用动态选择，同一栈内两种卡片可并存，不设全局单选。具体阈值（如 `navigator.deviceMemory`、Performance memory 水位）在二期实测标定。

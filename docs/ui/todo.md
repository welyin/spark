# 新 UI 落地 TODO

> 状态：草稿 v0.4 ｜ 配套文档：[ui-architecture.md](ui-architecture.md)（架构与阶段划分）、[README.md](README.md)（信息架构）、[visual-style.md](visual-style.md)（样式基线）
>
> v0.4 对齐上游设计更新：rail 默认展开 240px（shell-desktop v0.5）、弱网签名入队与不做系统推送的诚实呈现（shell-mobile v0.3）、全局组件 G11（shell-desktop §四）、跨域拖拽确认强度定稿（domain-space v0.4）。
>
> 用法：按阶段顺序执行；每项标注设计出处与主要改动面。完成一项勾一项，并在 wiki/ui 补落地记录。

## 阶段 0 · 令牌与骨架（纯外壳手术）

> 2026-09-08 PC 复核：以下历史勾选不代表所有子需求验收完成。最新事实见 [PC 文档对齐复核](../../wiki/ui/ui-new-ui-implementation.md)。本次完成壳层 72/240px、空间二级列表、市场窗口、图标固定/排序、Dock、多窗口分屏/几何记忆与切页保活；桌面文件夹/分页、完整一级页分栏及原生功能仍待补齐。

- [x] **0.1 补令牌缺口**（前置，阻塞后续所有 UI 任务）：`--spark-text-on-color`、`--spark-service`、分类色板 `--spark-cat-1..5`、动效令牌 `--spark-dur-*/--spark-ease-*`、z-index 层级令牌、桌面图标块尺寸与窗口标题栏高常量 —— 出处 [visual-style §10](visual-style.md)；改动 `code/app/src/styles/tokens.css` ✅ 2026-09-08（vite build 通过）
- [x] **0.2 一级入口换血**：`activeTab` 改为 `mine | messages | space | affairs`；PC rail 顺序 我的·消息·空间·事务，且**默认展开 240px**（现状默认 64px 窄栏，改默认值、仍记忆用户折叠选择）；移动底栏 消息·空间·事务·我的 —— 出处 [README §五](README.md)、[shell-desktop §六决策 1](shell-desktop.md)；改动 `App.vue`、`stores/ui-layout.ts`（`MOBILE_TABS`）、rail 持久化键 `spark:rail-expanded` 默认值 ✅ 2026-09-08（vue-tsc 通过）  - 二次调整（2026-09-08，用户评审）：rail 顶部项改为 **搜索·消息·空间·事务**（搜索开浮层，非页面）；「我的」入口并入底部头像+名字；身份块名字下方小字改为**网络状态行**；空间二级菜单宽栏常驻；**顶栏仅「空间」桌面出现**，其余一级页无顶栏（详见 [shell-desktop §一](shell-desktop.md) 与 [落地记录](../../wiki/ui/ui-new-ui-implementation.md)）。- [x] **0.3 通讯录改为空间插件、退出一级与消息页签**：移除一级 contacts tab，消息页只留会话（搜索 / `＋` 拉起选人）；通讯录转为各空间内插件（个人好友 / 组织成员两套人脉）—— 出处 [README §4.2/§八](README.md)；改动 `pages/MessagesPage.vue`、`components/contacts/`（转成空间插件视图）
  - **阶段 0 已完成部分**（2026-09-08）：一级 contacts tab 已移除（rail/底栏四入口为 我的·消息·空间·事务），消息页只留会话流、无内嵌通讯录页签；
  - **遗留已闭环**（2026-09-08，空间桌面就绪后）：意图穿透沙箱的通道打通——宿主经统一深链 `services/deep-link` 打开 spark-contacts 并把意图写进 `viewBootstrap.cardData`（`{intent:'__browse__'|'__add__'}` 或 `{rootId}`），插件侧 `pending-contact.ts` 改为消费 `window.__sparkPluginView.cardData`（A19「反向通道」由此通道解决）；消息页空态「发起新会话/添加朋友」（`open-intents`）、全局搜索打开联系人（GlobalSearch）、移动端顶栏「＋」均改走 deep-link 打开插件；废弃的 `spark:open-contact` 事件与宿主 `pending-contact` 引用已从 App.vue 移除。旧 `ContactsPage`（legacy）及宿主 `pending-contact`/`pending-add-contact` store 按灰度纪律保留一个版本周期（仍被 legacy 页与测试引用），后续随 2.9 一并退役。
- [x] **0.4 占位页**：space / affairs 先放"建设中"空状态（`el-empty`），保证阶段 0 可发布 ✅ 2026-09-08（`pages/PlaceholderPage.vue`，随 0.2 挂入 `App.vue` 页面分支）
- [~] **0.5 回归**：移动栈导航、Android 返回键、rail 宽窄持久化、深色模式四项手工走查
  - 最新复核（2026-09-08）：`vue-tsc`、`vite build` 通过；标准环境串行 `vitest run --threads false` 为 86 文件 / 666 测试通过。早期将两组 e2e 误记为空文件的判断撤回；它们是有效测试。
  - 手工走查（需真机/界面，建议阶段评审时执行）：① 移动底栏四 tab 切换与栈导航 ② Android 返回键逐层回退 ③ rail 默认展开 240px 且折叠选择被记忆 ④ 深色模式下 rail/底栏/占位页观感。

## 阶段 1 · 手机端空间桌面（移动先行）

- [x] **1.1 域列表页**：space tab root = 个人空间置顶 + 已加入域列表 + 待办角标 + 创建/加入入口 —— 出处 [shell-mobile §3.1](shell-mobile.md)；复用 `MobileSpaceDrawer` 的数据源 ✅ 2026-09-08（新建 `components/SpaceListPage.vue` + `SpaceMobile.vue` 两级宿主挂入 `App.vue` space 分支；点域 `pushPage('space','desktop')` 进桌面（占位，1.2 承接图标网格）；vue-tsc 通过）
- [x] **1.2 手机桌面**：pushPage 进域桌面，图标网格/分页/长按编辑；图标 = 该域已装插件（按安装作用域过滤）—— 出处 [domain-space §4.1](domain-space.md) ✅ 2026-09-08（`components/SpaceDesktop.vue` 图标网格，`pluginMarket.list` 过滤 installed+supportedSpaces 与 AppsPage 同口径；点图标 open-app 走 openPluginTab。分页/文件夹/长按编辑留 1.3+）
- [x] **1.3 全屏 App 容器**：点图标 pushPage 进 `PluginIframeHost` 全屏页（沉浸式 chrome 已有），统一 App 导航栏 `‹返回桌面 · 应用名 · 当前域` —— 出处 [shell-mobile §3.2](shell-mobile.md) ✅ 2026-09-08（打开走 openPluginTab → `.plugin-tab-card` 移动端整页 + 沉浸式；返回键 `goBackFromPlugin` 回 space tab 且栈停在 desktop 帧 = 返回域桌面；`chrome.hostTitleBar:false` 插件自接管顶栏）
- [x] **1.4 桌面布局本机持久化**：`localStorage["spark:desktop:<spaceId>"]`，不跨端 —— 出处 [shell-desktop §3.5(4)](shell-desktop.md) ✅ 2026-09-08（图标排序键 `spark:desktop-icons:<spaceId>` 按空间本机隔离，SpaceDesktop 读取；壁纸/壁纸项后续并入同键）
- [x] **1.5 卸载确认话术**：弹窗明示"只移除应用、不删除该域数据" —— 出处 [domain-space §4.1](domain-space.md) ✅ 2026-09-08（`AppsPage.uninstallApp` 确认框现状已明示"卸载仅移除插件程序，插件数据（文档/消息）保留在本机"，符合要求，无需改动）
- [x] **1.6 应用市场入口**：桌面"添加应用"图标 → 现 AppsPage 市场面板改造嵌入 —— 复用 `components/apps/AppMarketPanel.vue` ✅ 2026-09-08（SpaceDesktop 空态「去应用市场」按钮 + `open-market` 事件 → `handleMenuSelect('apps')` 进应用页市场；长按编辑模式的常驻「添加应用」图标留待 1.3+ 编辑模式）
- [ ] **1.7 添加到系统主屏幕**：Android requestPinShortcut/动态快捷方式，iOS Web Clip/Quick Actions；深链唤起走单实例路由 —— 出处 [shell-mobile §3.4](shell-mobile.md)。⏳ 依赖 Tauri 原生侧开发（Kotlin/AAB 环境 + iOS 工程）与真机验收，属原生工程任务；待原生环境就绪后实现，前端仅需提供待固定的目标清单（spark://space/... 深链串）。
- [~] **1.8 弱网与推送的诚实呈现**：消息/事务条目全程状态 `待发送 → 已上链`，补发被拒给"未被接受及原因"回执（不静默丢弃、不二次确认）；"我的→通知与设置"如实说明"第一版不做系统推送、仅前台收取" —— 出处 [shell-mobile §六/§八](shell-mobile.md)、[ui-architecture §4.8](ui-architecture.md)；改动 `stores/messages.ts` 状态机、消息气泡/事务卡片状态区、MinePage 通知设置
  - 部分已在现状：消息发送状态机已有 发送中/已送达/已读；待补="待发送（离线入队）"态与"未接受及原因"回执、事务卡片同款、通知设置页的限制说明文案。建议与阶段 3（事务）一并做，因事务卡片状态区是主场景。

## 阶段 2 · PC 窗口桌面（核心新建）

- [x] **2.1 应用注册表 store**：`stores/desktop/app-registry.ts`，当前空间已装插件 → `Map<appId, AppDef>` —— 出处 [ui-architecture §4.2](ui-architecture.md) ✅ 2026-09-08（pluginMarket.list 过滤 installed+supportedSpaces，随空间切换重建）
- [x] **2.2 窗口管理器 store**：`stores/desktop/window-manager.ts`：实例表按空间分桶、winKey 多实例、activeId/maxZ、开窗幂等 + 级联偏移、关窗自动激活 ✅ 2026-09-08（9 条单测全过；含 openNewWindow 多开、toggleWindow 三态、单实例恢复）
- [x] **2.3 WindowFrame 组件**：标题栏（高 44 复用 topbar）、拖动、八向缩放（Pointer Events）、最小化 `v-show` 保活、**iframe 遮罩**（必做，ark 已验证的坑）、双击标题栏最大化/还原 —— 出处 [shell-desktop §3.5](shell-desktop.md) ✅ 2026-09-08（`components/desktop/WindowFrame.vue`，rect 本地持有、iframe 遮罩、边界夹取）
- [~] **2.4 贴边磁吸与半屏/四分屏**、右/下边界夹取；**不设窗口硬上限**，做多窗内存监测与非阻断提示（不自动关窗，用户自管）
  - 已落（2026-09-08）：边界夹取、拖边分屏预览、左右半屏/四角四分屏菜单、最大化还原及位置尺寸记忆；几何单测和浏览器窗口检查通过，不设硬上限。
  - 推进项：多窗内存吃紧的非阻断提示（需接内存水位）。
- [x] **2.5 IconGrid + 桌面编辑**：图标网格、选中/双击打开、右键菜单（打开/固定/卸载/信任级）、溢出分页；布局持久化 `spark:desktop:<spaceId>` ✅ 2026-09-08（`DesktopIconGrid.vue`：单击选中/双击开窗/右键 打开·新窗口·市场查看；壁纸/排列与「固定到任务栏」右键项、卸载确认入口后续并入）
- [x] **2.6 TaskDock**：固定区｜已开窗口区、点击三态（未开→打开；前台→最小化；后台→置顶）、运行中小圆点 ✅ 2026-09-08（`TaskDock.vue`：固定区读 `spark:desktop:<spaceId>.dock`、运行区按 appId 去重、toggleWindow 三态、前景高亮）
- [x] **2.7 窗口状态持久化**：`spark:windows:<spaceId>`；恢复策略已定：只还原"上次开着哪些应用"、每应用恢复为单实例，多实例 `seq` 不还原（[ui-architecture §八决策 1](ui-architecture.md)） ✅ 2026-09-08（window-manager 持久化层 persistWindows/restorePersistedWindows + PcDesktop 挂载/切空间恢复 + watch 自动保存）
- [x] **2.8 上下文条升级**：TopNavbar 重组为 域头像/名称 + 域内身份 + 同步状态 —— 出处 [README §6.1](README.md) ✅ 2026-09-08（TopNavbar 补域内身份昵称：组织空间显域内昵称缺省回退根身份、个人空间显根身份；SpaceSwitcher+NetworkStatusBar 已有）
- [~] **2.9 插件 tab 旧入口退役**：新旧并行一个版本周期后移除 rail 动态 pluginTabs（保留灰度回退开关）⏳ 策略：PC 桌面（2.1–2.8）刚落地，**先并行一个版本周期**，期间 rail 动态 pluginTabs 保留作回退；待桌面稳定后移除。当前不移除（避免破坏 AppsPage「打开」的既有路径）。
- [ ] **2.10 与 OS 文件互拖**：Tauri file-drop 拖入摄取（落点即域/预检/哈希去重/先落本机再同步）+ 拖出落盘（明文副本提示）—— 出处 [shell-desktop §3.6](shell-desktop.md)、[ui-architecture §4.7](ui-architecture.md)。⏳ 需 Tauri Rust 侧命令（读盘/递归/哈希/流式入库）+ 前端拖放态；原生工程量较大，建议单独排期与内核同学对齐。
- [ ] **2.11 系统快捷入口/深链唤起**：Tauri 写桌面/Dock 快捷方式、注册 `spark://` 与单实例锁，外部唤起统一走 deep-link；退出组织后指向该域的入口自动失效 —— 出处 [shell-desktop §3.7](shell-desktop.md)。⏳ 依赖阶段 3 的 deep-link 服务（§4.4）先落地，再做 Tauri 协议注册与单实例；与 3.4 联动。
- [~] **2.12 全局组件**：全局新建（Cmd/Ctrl+N，随当前域上下文发起）、快捷键（⌘/Ctrl+1~4 切四入口、K 搜索、F 窗内查找）、右上角通知中心（点击等价走 deep-link）—— 出处 [shell-desktop §四](shell-desktop.md)、缺口 G11；GlobalSearch 的 Cmd/Ctrl+K 已有，其余增量
  - 已落：Cmd/Ctrl+1~4 切四入口、Cmd/Ctrl+K 搜索浮层，左栏与 Dock 搜索入口。
  - 推进项：Cmd/Ctrl+N 全局新建、窗口内查找、通知中心聚合。

## 阶段 3 · 事务一等入口与深链统一

- [x] **3.1 AffairsPage 三栏**（PC）/ 列表（移动）：**只做"与我相关"**（无公共页签，公共议题是独立空间插件；身份切换控件归插件、外壳不设全局切换）、"等我操作"置顶高亮分组、筛选维度按 [README §4.4](README.md)；条目支持"在所属空间桌面中打开"（开成桌面窗口，与账本/文件并排）—— 出处 [shell-desktop §2.4](shell-desktop.md) ✅ 2026-09-08（`pages/AffairsPage.vue`：等我操作（进行中）置顶高亮 + 已关闭分组 + 点卡片分发类型插件；PC 三栏右栏嵌插件为终态，第一版先列表+分发，筛选抽屉留推进项）
- [x] **3.2 affair-feed store**：跨域事务列表 + "待我处理"计数（角标语义切换）；计数谓词若内核未提供，先以"我持有副本且进行中"近似并标注 —— 见架构风险表 ✅ 2026-09-08（`stores/affairs/affair-feed.ts`：listFollowed+readLog/readResolution，actionableCount=进行中且关注近似，已标注待内核精确谓词；rail 事务角标接入）
- [x] **3.3 类型注册表**：manifest 新增可选 `affairTypes`；**宿主扫描已装插件 manifest 自建** `Map<affairType, pluginId>`（内核零改动，[ui-architecture §八决策 2](ui-architecture.md)）；SDK 类型声明同步（`code/packages/plugin-sdk`），向后兼容 ✅ 2026-09-08（`stores/affairs/affair-types.ts` 宿主扫描自建；`plugin-sdk` PluginManifest 增可选 `affairTypes`；spark-affairs manifest 声明 discussion/vote/budget/topic）
- [x] **3.4 deep-link 服务**：定稿语法 `spark://space/<域>/app/<插件>[/object/<对象>]`（与 [shell-desktop §3.7](shell-desktop.md) 一致，https Universal/App Link 同源）；点击事务卡片 → 查表 → PC 右栏/桌面窗口、移动全屏 App；未装插件给市场引导空状态 —— 出处 [README §4.4](README.md) ✅ 2026-09-08（`stores/affairs/affair-open.ts`：查类型注册表→切域→派 `spark:open-affair-plugin` 事件，App.vue 经 openPluginTab 渲染并传 `viewBootstrap.cardData.affairId` 注入 iframe；未装插件返回 no-plugin 供市场引导。`spark://` URL 解析/外部唤起属 2.11 原生侧）
- [x] **3.5 消息卡片跳转切流**：`plugin/card-actions.ts` 内部改走 deep-link，卡片 schema 不变；深链到达先切域、上下文条联动 ✅ 2026-09-08（新建 `services/deep-link.ts` 统一深链事件 `spark:open-plugin`；AppMessageCard 动作主实例未运行时回退经 deep-link 拉起插件（原设计丢弃，现不丢）；affair-open 复用同一服务；App.vue 统一监听打开插件。切域由 deep-link 调用方负责，上下文条随 currentSpace 联动已有）
- [x] **3.6 spark-affairs 插件适配**：补 `affairTypes` 声明，承接从事务列表打开的视图参数 ✅ 2026-09-08（manifest 加 affairTypes；AffairsView onMounted 消费 `__sparkPluginView.cardData.affairId` 自动打开详情）

## 二期（不在本期承诺）

- [ ] 手机最近任务卡片栈（底部上滑调出一叠已开 App 预览卡片，左右滑切换、上滑关闭）；预览方案已定：**按内存自适应**——内存充裕用活 iframe 缩略、吃紧降级静态截图，同一栈内可混用，阈值二期实测标定（[ui-architecture §八决策 3](ui-architecture.md)）
- [ ] PC **内部**跨窗口拖拽分享（对象在域间/窗间流动）：同域不弹；跨域每次弹目标域确认，可勾选"本次会话内同一 源域→目标域 不再提示"（仅本次运行有效，不做永久豁免）—— 确认强度已定稿；与 2.10"OS⇄Spark 互拖"区分（[domain-space §五.2](domain-space.md)、[shell-desktop §3.2/§3.6](shell-desktop.md)）
- [ ] 桌面文件夹、壁纸/主题色按空间配置（[domain-space §5.1](domain-space.md)）
- [ ] 副本健康度桌面角标（[domain-space §5.3](domain-space.md)）
- [ ] 全局搜索升级：来源 indexer 切换与标注（[README §6.4](README.md)）

## 每项通用的验收纪律

- 颜色/尺寸/圆角/动效只许用 `--spark-*` 令牌，禁止裸写 hex 与魔法数（[visual-style §1](visual-style.md)）
- 深色模式不做单独适配，靠 `--el-*` 别名自动接管；提交前双主题各过一眼
- 插件桥协议、manifest 既有字段、消息卡片 schema 不许破坏（[ui-architecture §5](ui-architecture.md) 接口冻结声明）
- 每阶段结束在 [wiki/ui](../../wiki/ui/README.md) 补一篇落地记录（现行实现的事实来源保持最新）

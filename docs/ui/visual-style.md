# 视觉基线与设计令牌（visual-style）

> 状态：草稿 v0.1 ｜ 分册：UI 设计
>
> **本篇不是新设计系统，而是对现有前端实现的抽取与约束。** 事实来源：`code/app/src/styles/` 下的 `tokens.css`、`base.css`、`element-theme.css`、`app-shell.css`。新 UI（空间桌面、独立窗口、事务详情、移动 App 化）必须复用本基线，做到**视觉延续、改动最小、深色模式零额外工作**。信息架构见 [README](README.md)。

---

## 一、总原则：贴着现状走（五不）

1. **不换组件库**：Element Plus 2.14 是唯一基础组件来源，图标用 `@element-plus/icons-vue`，不引第二套组件 / 图标库；
2. **不另定主色、不造第二套色板**：任何颜色走 `--spark-*` / `--el-*` 令牌，不在组件里写死 hex；
3. **不就地写魔法数**：尺寸、圆角、阴影、字号、间距优先用现有令牌，缺了先补进 `tokens.css` 再用；
4. **深色模式不单独做**：颜色令牌已别名到 `--el-*`，`html.dark` 下由 Element 官方 dark/css-vars 自动接管（见 `stores/theme.ts`）；
5. **不改外壳像素骨架**：顶栏 44px、rail 64/155、断点 768 等已上线尺寸保持不动，新界面在其内部生长。

## 二、技术栈与样式分层（现状）

- 栈：Vue 3 + TypeScript + Vite + Tauri 2 + **Element Plus 2.14**；无 Tailwind / CSS-in-JS，纯 CSS + CSS 变量；
- 分层与职责：

| 文件 | 职责 | 新代码怎么用 |
|---|---|---|
| `tokens.css` | 全部设计令牌（颜色别名 + 结构常量） | 唯一允许新增令牌的地方 |
| `base.css` | reset、字体栈、滚动条、通用文本类 | 直接复用 `.eyebrow/.lede/.hint/.desc/.mono` |
| `element-theme.css` | 组件库结构性微调（圆角、弹窗、移动端宽度） | 一般不动，全局生效 |
| `app-shell.css` | 外壳 rail/topbar/main、抽屉、移动转场、安全区 | 新外壳元素沿用其类与变量 |
| `pages/*.css`、`components/*.css` | 页面 / 组件 scoped 样式 | 只写布局，颜色尺寸用令牌 |

- 主题三态 `system / light / dark`：切到深色时给 `<html>` 挂 `.dark`，选择持久化在 `localStorage`（读写 try/catch 兜底）；
- **令牌关系**：`--spark-*`（业务语义层）颜色一律 `var(--el-*)`（组件库层），结构常量（尺寸类）由 Spark 自定义。业务代码只认 `--spark-*`，不直接依赖 `--el-*`，以便将来整体换肤只改一处。

## 三、颜色令牌

### 3.1 品牌与语义色（已存在，直接用）

| 用途 | Spark 令牌 | 别名到 | 浅色参考值* |
|---|---|---|---|
| 主色 / 激活 / 链接 | `--spark-primary` | `--el-color-primary` | `#409eff` |
| 主色悬停 | `--spark-primary-hover` | `--el-color-primary-light-3` | `#79bbff` |
| 主色按下 | `--spark-primary-active` | `--el-color-primary-dark-2` | `#337ecc` |
| 主色浅底（激活项底） | `--spark-primary-light` | `--el-color-primary-light-9` | `#ecf5ff` |
| 成功 | `--spark-success` / `-bg` | success / light-9 | `#67c23a` / `#f0f9eb` |
| 警告 | `--spark-warning` / `-bg` | warning / light-9 | `#e6a23c` / `#fdf6ec` |
| 危险 | `--spark-danger` / `-bg` | danger / light-9 | `#f56c6c` / `#fef0f0` |

\* 参考值仅为便于讨论，**以 Element Plus 当前版本输出为准，代码中不得硬编码**；深色由 `--el-*` 自动转深。

### 3.2 中性色（已存在，直接用）

| 用途 | 令牌 | 浅色参考值* |
|---|---|---|
| 页面底色 | `--spark-bg-page` | `#f2f3f5` |
| 卡片 / 面板底 | `--spark-bg-card` | `#ffffff` |
| 悬停浅底 | `--spark-bg-hover` | `#f5f7fa` |
| 主文字 | `--spark-text-1` | `#303133` |
| 次文字 | `--spark-text-2` | `#909393` |
| 占位 / 弱文字 | `--spark-text-3` | `#a8abb2` |
| 边框 / 浅边框 | `--spark-border` / `-light` | `#dcdfe6` / `#e4e7ed` |

### 3.3 业务色与"建议补"的少量色令牌

- 已存在：性别色 `--spark-gender-male #3296fa` / `--spark-gender-female #eb2f96`（深浅一致，参考微信）；
- **建议补进 `tokens.css`（现在散落在页面 css，属最小补缺，不是新色板）**：
  - `--spark-text-on-color: #fff`——彩色底上的白字（现状大量裸写 `#fff`，统一到一个令牌）；
  - `--spark-service: #64748b`——服务号 / 系统标识色（现散见 `messages.css`、`contacts.css`）；
  - 一组**分类色板** `--spark-cat-1..5`（收敛 `contacts.css` 的 `#ff7d00 / #3296fa / #f7b500 / #64748b / #34c19b`），只用于头像 / 分类底标，不承载语义对错。

## 四、字体与字号

- 字体栈（`base.css`）：`-apple-system, BlinkMacSystemFont, 'Segoe UI','PingFang SC','Hiragino Sans GB','Microsoft YaHei', …`；等宽 `.mono`：`Menlo, Monaco, Consolas, …`（RootID/PeerId/哈希用）；
- 基准 `font-size:14px`、`line-height:1.6`、`-webkit-font-smoothing:antialiased`；
- 字号四档（已存在）与新 UI 用法：

| 令牌 | 值 | 用途（含新 UI） |
|---|---|---|
| `--spark-font-size-title` | 20px | 页面 / 弹窗主标题、事务详情标题 |
| `--spark-font-size-base` | 14px | 正文、菜单、窗口内容、列表主体 |
| `--spark-font-size-placeholder` | 13px | 输入占位、次级正文 |
| `--spark-font-size-secondary` | 12px | 辅助说明、角标、桌面图标名、时间戳、hint |

- 通用文本类直接用：`.eyebrow`（眉标，主色 12px/600/字距）、`.lede`（标题下引导）、`.hint`（操作下补充，最弱）、`.desc`（次要描述）；
- 字重沿用常规 400 / 600（弹窗标题已全局 600），不引入更多字重。

## 五、圆角、阴影、间距

### 圆角（已存在，四档，各司其职）

`--spark-radius-s:4px`（标签/小元素）｜`-m:8px`（按钮/输入/菜单项，已对齐 `--el-border-radius-base`）｜`-l:12px`（卡片/面板/rail-item）｜`-xl:16px`（弹窗/抽屉，已对齐 dialog/messagebox）。**新窗口用 xl，事务卡片用 l，按钮输入用 m，标签用 s，不新增档位。**

### 阴影（已存在三档，深色已另给值）

| 令牌 | 浅色 | 用途 |
|---|---|---|
| `--spark-shadow-card` | `0 1px 2px rgba(31,35,41,.04)` | 卡片、页面头部、静态面板 |
| `--spark-shadow-hover` | `0 4px 16px rgba(31,35,41,.1)` | 悬停浮起、下拉、桌面窗口非激活 |
| `--spark-shadow-pop` | `0 6px 24px rgba(31,35,41,.12)` | 弹窗、激活窗口、弹出层 |

### 间距（已存在 + 现状惯例）

`--spark-gap-page:16px`（元素间距）、`--spark-padding-page:20px`（页面/卡片内边距）；外壳惯例：rail 项 gap 4px、页面操作区 gap 8px、卡片间 16px。**新增空间桌面内边距沿用 `--spark-padding-page`，窗口内边距用 16，不再造新值。**

## 六、外壳与布局尺寸（新 UI 直接沿用，不改）

- 骨架：`.shell` 固定 `100vh`（移动端 `100dvh`）不整页滚动，只有 `.main` 与页内分栏内部滚动；全局 `overscroll-behavior:none`；
- 顶栏 `--spark-topbar-height:44px`（浅色 `#f8fafc`，深色回 `--el-bg-color`）；
- 左导航 rail：`--spark-rail-width:64px`（窄，图标上文字下）/ `--spark-rail-width-expanded:155px`（宽，左图标右文字、行高 40）；rail-item 52×52、圆角 l；**激活态固定为"主色浅底 + 主色字"**（`--spark-rail-item-active` + `--spark-primary`），hover 用 `--spark-rail-item-hover`；
- 响应式断点：**≤768px 移动**（rail 不渲染、改 `MobileTabBar` 底栏，与 `stores/ui-layout.ts` 同断点）；900px 仅 `.page-header` 折行；
- 安全区：统一 `--spark-safe-top/bottom: env(safe-area-inset-*)`，新移动页面必须叠加；
- 滚动条：宽/高 8px、thumb 圆角 4px、track 透明。

> 新四入口（我的/消息/空间/事务）只是替换 rail 项与页面内容，**rail、topbar、main、移动底栏的尺寸与行为完全复用**；"空间"内的桌面是 `.main` 里的内容，不另起外壳。

## 七、动效（沿用现有时长与曲线，令牌化）

- 现状：rail 宽度 `0.2s ease`；rail-item 背景/色 `0.15s ease`；移动栈 push/pop `260ms cubic-bezier(0.25,0.46,0.45,0.94)`（参考微信，旧层静止、新层右滑入）；tab 切换 `150ms ease-out` 淡入淡出；
- **建议把时长 / 曲线补成令牌**（最小补缺）：`--spark-dur-fast:150ms`、`--spark-dur-base:200ms`、`--spark-dur-page:260ms`、`--spark-ease-standard:ease`、`--spark-ease-ios:cubic-bezier(0.25,0.46,0.45,0.94)`；
- 窗口开合、桌面切换、图标 hover 一律取这些既有值，不引入弹跳 / 复杂动画，保持与钉钉 / 微信一致的克制感。

## 八、新 UI 元素 → 现有令牌 / 组件映射（保证不大改）

| 新元素 | 底色 / 边框 | 圆角 / 阴影 | 用什么 Element 组件 |
|---|---|---|---|
| 空间桌面背景 | `--spark-bg-page` | — | 自定义容器 |
| 桌面图标块 | 透明，hover `--spark-bg-hover` | `--spark-radius-l` | 自绘按钮 |
| 独立窗口（PC） | `--spark-bg-card` + `--spark-border-light` | xl + `--spark-shadow-pop`（激活）/ hover（非激活） | `el-dialog` 风格或自绘，标题栏高对齐 topbar 44 |
| 移动全屏 App 页 | `--spark-bg-card` | 无（全屏） | 现有 `.mobile-stack-*` 栈转场 |
| 事务卡片 / 列表项 | `--spark-bg-card` + 浅边框 | l + card 阴影 | `el-card` / 自绘行 |
| 状态 pill / 进度 chip | 语义 `-bg` 底 + 语义字 | s | `.stat-chip` 同款 |
| 当前域上下文条 | `--spark-bg-card` + 底边框 | 无 | 自绘条 |
| 服务号消息卡片 | `--spark-bg-card`，标识用 `--spark-service` | l | 自绘卡片 |
| 空状态 | 文字 `--spark-text-3` | — | `el-empty` |
| 角标 | danger 色 | — | `el-badge`（rail 角标位置已在 app-shell 修正） |

## 九、组件使用约定

- 优先用 Element 组件表达标准交互：按钮 `el-button`、弹窗 `el-dialog`、抽屉 `el-drawer`（统一套 `.app-drawer` 去默认头、`.app-drawer-body` 20/24 内边距、`.app-drawer-close` 圆形关闭钮）、页签 `el-tabs`、徽标 `el-badge`、卡片 `el-card`；
- 全局已把 dialog/messagebox 圆角调到 xl、标题加粗；移动端 dialog 自动 `100vw-32px`，新弹窗不重复写这些；
- 插件视图容器沿用 `.plugin-tab-card` 与沉浸式 `--immersive`（iframe 全高、可自接管顶栏），空间桌面里的"应用窗口 / App 页"就是它的新外观，内部契约不变。

## 十、需要补的令牌缺口 & 现存技术债

**建议补（都加在 `tokens.css` 同一处，属补缺非新体系）**

1. `--spark-text-on-color`、`--spark-service`、分类色板 `--spark-cat-1..5`（§3.3）；
2. 动效时长 / 曲线令牌（§7）；
3. 一组层级 `z-index` 令牌（桌面 / 窗口 / 弹窗 / 抽屉 / toast 的堆叠次序），避免各窗口随手写 `z-index:999`；
4. 空间桌面 / 窗口的少量结构常量（桌面图标块尺寸、窗口标题栏高=44 复用 topbar）。

**现存技术债（非阻塞，新代码不得再犯）**

- `styles/pages/apps.css` 内硬编码了一批 Element 原色（`#409eff/#f56c6c/#e6a23c/#79bbff/#b3d8ff/#fef0f0/#c0c4cc` 等），应逐步替换为 `--spark-*` / `--el-*`；
- `contacts.css`、`messages.css` 的分类 / 服务号色（`#ff7d00/#f7b500/#34c19b/#64748b`）在提为分类色板令牌后统一引用；
- 彩色底上的裸 `#fff` 统一改 `--spark-text-on-color`；
- 这些只做等价替换、不改观感，可随页面迭代顺手清理，不为它单起重构。

## 十一、落地自检清单（新界面提 PR 前过一遍）

1. 有没有写死 hex / 像素魔法数？有 → 换令牌或先补令牌；
2. 切到深色是否自动正确（没有为浅色单独写死底色）？
3. 是否优先复用了 Element 组件与 `.app-drawer/.plugin-tab-card/.mobile-stack-*` 既有类？
4. 圆角 / 阴影 / 字号是否落在既有档位？
5. 移动端是否叠加安全区、用 100dvh、走 768 断点与既有栈转场？
6. 外壳（rail/topbar/底栏）像素是否保持不变？

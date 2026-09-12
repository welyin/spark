# 插件分发规格（仓库锚定安装 + 镜像中转）

> 来源：设计文档 `插件体系·分发与信任`（wiki architecture/plugins/plugin_system.md） 的协议化落地。
> 本文档为字节级权威：插件 id 语法、仓库声明文件（spark-plugin.json）字段、
> 验证算法、镜像展开顺序、安装包清单校验与错误码，实现必须逐条对齐。
> 广播索引（plugin-announce topic、PoW/TTL、relay 资历制、懒惰核查）见 §8。

## 1. 插件 id（仓库地址）

### 1.1 语法

```
id = host "/" owner "/" repo [ "/" sub-path ]
```

- `host` ∈ { `github.com`, `gitlab.com`, `gitee.com` }，其余一律拒绝；
- `owner` / `repo`：单段，字符集 `[a-z0-9._-]`，长度 1–100；
- `sub-path`：可选，1–8 段，每段字符集 `[a-z0-9._-]`，长度 1–64（monorepo 子目录）；
- 每段不得为 `.` / `..`，不得含 `\`、`%`、空白、查询串与 fragment；
- id 总长度 ≤ 256 字节（UTF-8）。

### 1.2 规范化（parse 必做，比较与缓存键一律用规范化结果）

按序执行：

1. 去首尾空白；剥掉一次 `https://` / `http://` 前缀（大小写不敏感；仅作输入容忍，不参与任何抓取）；
2. 去末尾 `/`（重复执行至无尾斜杠）；`repo` 段去 `.git` 后缀（一次）；
3. 全串转小写（`to_lowercase`）；
4. 按 §1.1 校验，失败即拒。

示例：`HTTPS://GitHub.com/Owner/Repo.git/` → `github.com/owner/repo`。
规范化后等长的两个输入视为同一插件。

## 2. 仓库声明文件 spark-plugin.json（信任锚点）

放置位置：id 指向的目录（仓库根或 sub-path 子目录）下，文件名固定 `spark-plugin.json`；
同内容同时作为 release 资产发布（获取顺序见 §3.2）。

### 2.1 字段定义

| 字段 | 类型 | 必填 | 定义 |
| --- | --- | --- | --- |
| `id` | string | 是 | 插件 id，**规范化后必须等于声明文件所在仓库地址**（§4.1） |
| `name` | string | 是 | 显示名，1–64 字符 |
| `icon` | string | 否 | `data:` base64 图片（≤ 20 KB）或 https URL；空串 = 无图标。与包内 manifest.json 的 `icon` 同图（标准见 §2.3） |
| `summary` | string | 是 | 简介，1–256 字符 |
| `category` | string | 是 | `"ai-assistant"` \| `"social"` \| `"tool"` \| `"game"` \| `"foundation"`（其余值按 `"tool"` 展示） |
| `requires` | object | 否 | 运行时平台约束。`requires.platforms` 为字符串数组，可选值 `"desktop"` / `"mobile"`；缺省全平台可用。安装时校验，当前平台不在列表中即拒 |
| `window` | object | 否 | PC 窗口默认尺寸（插件级，不做 per-view）。`window.defaultWidth` / `window.defaultHeight` 为整数像素，合法范围宽 320–3840 / 高 220–2160；缺省或任一维度缺失/越界一律按壳层默认 880×620（体验提示而非安全约束，非法值不 fail-loud）。移动端全屏打开，忽略本字段。包内 manifest.json 的同名字段是侧载/内置链路的声明源（两级口径同 `supportedSpaces`/`requires`：声明文件缓存优先，缺省回落安装时落库值） |
| `version` | string | 是 | 当前发布版本，semver 三段（`x.y.z`，可带预发布后缀与 `+build` 元数据） || `releaseAssetPattern` | string | 是 | 包资产命名模板，必须包含一次 `<version>` 占位且以 `.spkg` 结尾；派生规则见 §2.2 |
| `permissions` | string[] | 是 | 声明权限（伞权限字符串集；非法项忽略，可为空数组） |
| `mirrors` | string[] | 否 | 镜像仓库 id 列表（§1.1 语法，host 不限于原 host），≤ 8 条；空/缺省 = 无声明镜像 |
| `sdkVersion` | string | 是 | 构建所用 SDK 协议版本（如 `1.0.0`）；壳层不兼容即拒载 |
| `supportedSpaces` | string[] | 否 | 插件支持的空间类型（`personal` / `org` 子集，非空；其余值即拒）；缺省按 `["org"]` 处理（spaces-and-plugins §4），市场按当前空间过滤展示 |

未知字段忽略（前向兼容）。声明文件整体 ≤ 64 KiB（UTF-8 字节），超限即拒。

### 2.2 由 releaseAssetPattern 派生命名（字节级规则）

设 `pattern = releaseAssetPattern`，`version` 为 §2.1 的 version：

- 包资产名 = `pattern` 中 `<version>` 替换为 `version`；
- 更新清单资产名 = `pattern` 中 `<version>` 替换为 `manifest`，再去掉结尾 `.spkg` 换成 `.json`；
- 签名资产名 = 同上，换成 `.sig`（可选资产，见 §4.3）；
- release tag：无 sub-path 时 `v<version>`；有 sub-path 时 `<sub-path 最后一段>-v<version>`。

例：`spark-plugin-spark-example-<version>.spkg`，version `0.1.0` →
包 `spark-plugin-spark-example-0.1.0.spkg`，清单 `spark-plugin-spark-example-manifest.json`，
签名 `spark-plugin-spark-example-manifest.sig`，tag `v0.1.0`。

### 2.3 插件图标标准（2026-09-10 定稿）

每个插件都可以指定自己的图标图片。标准分两个载体，**同一张图**：

**① 包内图标（权威源，运行时）**：包内 `manifest.json` 新增可选字段 `icon`，
值为**包内相对路径**（推荐 `assets/icon.svg`；构建脚本把 `dist/` 全量打包，
放 `dist/assets/` 即随 .spkg 分发）。壳层经 `plugin://localhost/<id>/<path>`
协议读取——该协议只服务已安装且已启用的包，与"启用才出现在桌面"的口径天然一致。

- 格式：**SVG 优先**（矢量、任意尺寸清晰、体积小）；或 PNG 512×512（透明通道）。
- 视觉要求：内容画在方形安全区内（四边各留 ≥10% 内边距），壳层统一套圆角；
  图标自含底色（不要透明底配浅字），深浅色主题下均可辨识；
  不依赖外部字体与网络资源。
- 安全：壳层一律以 `<img>` 引用（SVG 内脚本不执行），不做 inline SVG。

**② 声明图标（市场层，未安装也可展示）**：声明文件既有 `icon` 字段
（§2.1：`data:` base64 ≤ 20 KB 或 https URL）。未安装条目（市场卡片 / 详情 /
探索页）用它；懒惰核查后以声明文件为准回写 corrected。**两个载体必须同图。**

**壳层统一回退链**：包内图标（已安装且已启用）→ 声明图标（市场条目 icon）→
首字符 + 哈希渐变。由壳层 AppIcon 组件统一实现，各展示位（桌面 / Dock / Launchpad /
窗口标题栏 / 市场 / 详情 / 属性窗口 / 手机桌面 / 全局搜索）不再各自拼装。

## 3. URL 模板与镜像展开

### 3.1 原始源（origin）URL 模板

声明文件（`{dir}` = sub-path 段加 `/` 或空串；`{declAsset}` = 无 sub-path 时
`spark-plugin.json`，有 sub-path 时 `<sub-path 最后一段>-spark-plugin.json`）：

| host | 模板 |
| --- | --- |
| github.com | release 资产（latest 指针）：`https://github.com/{owner}/{repo}/releases/latest/download/{declAsset}`；raw：`https://raw.githubusercontent.com/{owner}/{repo}/HEAD/{dir}spark-plugin.json` |
| gitlab.com | release 资产：`https://gitlab.com/{owner}/{repo}/-/releases/permalink/latest/downloads/{declAsset}`；raw：`https://gitlab.com/{owner}/{repo}/-/raw/{branch}/{dir}spark-plugin.json`（`{branch}` 依次试 `main`、`master`） |
| gitee.com | release 资产：`https://gitee.com/{owner}/{repo}/releases/latest/download/{declAsset}`；raw：`https://gitee.com/{owner}/{repo}/raw/{branch}/{dir}spark-plugin.json`（同上） |

release 资产（`{tag}` 见 §2.2，`{asset}` 为派生资产名）：

| host | 模板 |
| --- | --- |
| github.com | `https://github.com/{owner}/{repo}/releases/download/{tag}/{asset}` |
| gitlab.com | `https://gitlab.com/{owner}/{repo}/-/releases/{tag}/downloads/{asset}` |
| gitee.com | `https://gitee.com/{owner}/{repo}/releases/download/{tag}/{asset}` |

### 3.2 获取顺序（任一文件）

1. 声明文件**优先以 release 资产（latest 指针）取**（KB 级，与发布内容原子一致）；
   取不到再按同顺序试 raw 形式；
2. 源顺序：**origin 直连 → 声明文件 `mirrors[]` 按列表序 → 内置公共镜像回退**。
   `mirrors[]` 条目只取 host/owner/repo 三段，sub-path 沿用原 id。
   注意：`mirrors[]` 本身来自声明文件，故首轮取声明文件时只有 origin + 内置公共镜像
   两族源；拿到声明文件后，其 `mirrors[]` 才加入后续文件（清单/包）的源列表；
3. `http://` 一律拒绝；所有抓取仅 https（系统信任库），连接超时 5 s、整体超时 30 s。
   重定向拒绝 https→http 降级（最多 5 跳，响应最终 URL 再校验一次 scheme）；
   响应体有界读取（Content-Length 超限即断、流式截断兜底；声明文件 > 64 KiB 即拒）；
4. 远程获取的更新清单中，包资产 `url` 仅允许 https；`file://` 仅属内置目录
   本地 bundle 链路，远程清单指向本地文件一律拒绝。

### 3.3 内置公共镜像（仅 github.com origin 适用；不可信，仅作通路）

对任一 origin URL `U`（raw 或 release 资产），按序追加：

1. `https://mirror.ghproxy.com/{U}`
2. `https://gh-proxy.com/{U}`
3. jsDelivr（仅已知 tag 的仓库文件）：`https://cdn.jsdelivr.net/gh/{owner}/{repo}@{tag}/{dir}<文件>`。
   首波实现暂不消费（清单/包为 release 资产，jsDelivr 不承载），为后续按 tag 固定
   声明文件交叉比对预留。

镜像列表为协议常量，实现可按可达性追加实例，但不得置于 origin 之前。

### 3.4 双源交叉规则

- 声明文件与「无签名的更新清单」**必须双源交叉**：按 §3.2 顺序取到第一份后，
  继续从**与该源不同族**（origin / 声明镜像 / 内置公共镜像互为异族）的源取第二份；
  两份字节不一致 → 拒绝（`E_DECL_CROSS_MISMATCH`）；
- 仅一族源可达时允许单源降级继续（记 `source=single`），但声明文件单源降级
  仅当该源为 origin 时允许——声明文件是信任锚，镜像单源不得单独作数；
- 带合法签名的更新清单免交叉（签名层已覆盖，见 §4.3）；
- `.spkg` 包体不交叉：sha256 逐字节校验（§5）已覆盖，镜像最坏导致「下不到」。

## 4. 验证算法

### 4.1 id 一致性（展示/安装前必过）

1. 规范化输入 id（§1.2），失败 → `E_ID_INVALID`；
2. 按 §3.2/§3.4 取声明文件；
3. 解析并校验字段与大小限制（§2.1），失败 → `E_DECL_INVALID`；
4. `normalize(declaration.id) == normalize(输入 id)`，不等 → `E_DECL_ID_MISMATCH`。

包内 manifest（.spkg 内 manifest.json）自证**不作数**，不得替代上述任一步。

### 4.2 安装流程（installFromRepo）

1. id 一致性校验（§4.1）通过，得声明文件 `D`；
2. 由 `D.version` + `D.releaseAssetPattern` 派生 tag 与三资产名（§2.2）；
3. 按 §3.2 取更新清单资产：
   - 同时存在签名资产：取 `.sig`，验签（Ed25519 detached，信任公钥沿用现有市场
     信任链配置），失败 → `E_MANIFEST_SIG`；通过 → `trust = "signed"`；
   - 签名资产不存在（404）：按 §3.4 双源交叉取清单 → `trust = "repo-anchored"`；
4. 校验清单：`pluginId == 规范化 id`（不等 → `E_MANIFEST_ID_MISMATCH`）、
   `version == D.version`（不等 → `E_MANIFEST_VERSION_MISMATCH`）；
5. 定位 `kind == "package"` 资产，下载 `.spkg`，逐字节校验 sha256 与 size（§5）；
6. 落安装状态：`grantedPermissions = 基础权限 ∪ (声明 ∩ 高级权限)`，
   `trust` 按第 3 步标记持久化。

### 4.3 签名层（可选增强）

签名为**可选**：有 `.sig` 必验（验不过即拒），无 `.sig` 走纯哈希 + 交叉并标记
`trust = "repo-anchored"`。签名验证算法、公钥配置（内置默认 +
`SPARK_PLUGIN_UPDATE_PUBLIC_KEY_PEM` 环境覆盖）与现有市场信任链完全一致。

### 4.4 声明文件缓存

- 键：`plugin:repo:<规范化 id>`；值：JSON `{ "fetchedAt": <ms>, "text": <声明文件原文> }`；
- 内存 TTL **10 分钟**；sled 持久化副本无 TTL（离线可用），
  内存未命中时读 sled 回填内存；网络取到新副本即双写；
- 缓存内容使用前仍须过 §4.1 第 3–4 步（字段校验与 id 一致性不省）。

## 5. 安装包清单（update-manifest.json）

| 字段 | 类型 | 定义 |
| --- | --- | --- |
| `pluginId` | string | 仓库锚定安装时必须等于规范化 id（§4.2-4） |
| `domain` | string | 插件域身份，约定 `plugin:<规范化 id>` |
| `manifestVersion` | number | 清单格式版本（当前 1；不消费，忽略） |
| `version` | string | 发布版本，必须等于声明文件 version |
| `releaseTime` | string | ISO8601（不消费，忽略） |
| `permissions` | string[] | 可选；缺省用声明文件 permissions |
| `assets[]` | object[] | `{ kind, fileName, url, sha256, size }`；`kind == "package"` 为包资产 |

校验：包资产下载后整文件 sha256（hex 小写）必须等于 `assets[].sha256`，
文件字节数必须等于 `size`；不等即拒，不留残留状态（不落 installed、不留包文件）。
`fileName` 必须是单段文件名（拒绝 `/`、`\`、绝对路径、`.`/`..` 段与盘符），
落盘路径恒为 `<packages_root>/<id>/packages/<fileName>`，防任意路径写盘与
跨插件覆盖。

## 6. 错误码

与现有市场错误风格一致（英文短句 + 冒号详情），仓库锚定链路统一 `Repo plugin` 前缀：

| 码 | 错误字符串模板 |
| --- | --- |
| `E_ID_INVALID` | `Repo plugin id invalid: {input}` |
| `E_DECL_FETCH` | `Repo plugin declaration fetch failed: {id}` |
| `E_DECL_INVALID` | `Repo plugin declaration invalid: {id}: {reason}` |
| `E_DECL_ID_MISMATCH` | `Repo plugin declaration id mismatch: expected {id}, got {declared}` |
| `E_DECL_CROSS_MISMATCH` | `Repo plugin declaration cross-check mismatch: {id}` |
| `E_MANIFEST_FETCH` | `Repo plugin manifest fetch failed: {id}` |
| `E_MANIFEST_INVALID` | `Repo plugin manifest invalid: {id}: {reason}` |
| `E_MANIFEST_CROSS_MISMATCH` | `Repo plugin manifest cross-check mismatch: {id}` |
| `E_MANIFEST_SIG` | `Repo plugin manifest signature verification failed: {id}` |
| `E_MANIFEST_ID_MISMATCH` | `Repo plugin manifest id mismatch: expected {id}, got {actual}` |
| `E_MANIFEST_VERSION_MISMATCH` | `Repo plugin manifest version mismatch: expected {declared}, got {actual}` |
| `E_PACKAGE_HASH` | 沿用现有：`Plugin package sha256 mismatch for {id}` / `Plugin package size mismatch for {id}` |

## 7. 波次 2 接口预留（广播索引）

广播索引已落地（§8，`/spark/plugin-announce/1.0.0` topic）。索引条目只携带 id 与
展示摘要，**验证锚仍是本规格**：消费侧对每条索引执行 §4.1（可直接复用 §4.4 缓存），
通过后才进入市场视图。安装入口复用 §4.2，索引层不得绕过。

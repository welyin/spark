# 架构设计：通信（DM 底座与聊天应用插件化）

> 模板：产品目标 → 现状 → 差距 → 目标设计 → 迁移路径 → 验收。对应产品文档：[../../product/community/communication.md](../../product/community/communication.md)（定稿）。

## 一、产品目标（来自 product/community/communication.md）

1. **内核 = 统一 DM 底座 + 数据**：一条 E2E 加密、可离线暂存、多设备可达的 1:1 通道，透明承载聊天 / 回执 / 撤回 / 好友与组织邀请 / 资料同步 / 定向投递（feed）；
2. **聊天应用 = 可替换插件**（默认内置一个；群聊未来在它里面实现，属插件自身演进、不进 Spark 里程碑）；系统接受任何第三方聊天插件，换插件不动消息数据；
3. **通讯录数据内核级**（好友 / 标签 / 拉黑，个人域同步）；通讯录视图可插件化；
4. 自我审查个人层：过滤规则（关键词 / 来源屏蔽）由客户端与插件实现，内核零改动（product/todo #22①）；
5. 离线语义：个人私聊发件人排队 + TTL；组织内单聊组织网关（全员候选活跃集）密文代收。

## 二、现状（核实日期：2026-09-08；A19 落地状态见文末注记）

**wiki**：`docs/protocol/foundation/p2p/p2p-dm.md`（统一 DM 信封族；§19.6 `feed` 信封供社交插件定向投递）；`wiki/architecture/plugins/`（plugin_system / plugin-runtime / plugin-data-api）。

**代码**：

- DM 底座已实现：E2E（每条消息独立临时密钥 + 会话密钥经 pdsync 多设备共享 `dm:e2e:key:`）、离线（发件人侧排队 + TTL；组织网关 org-mail 代收）、多设备寻址、幂等去重、撤回归属校验；
- **IM / 通讯录 UI 默认内置插件版已落地（A19）**：`code/plugins/spark-chat`（聊天）与 `code/plugins/spark-contacts`（通讯录）两个插件工程，组件树自壳层功能对等迁移、数据源经 §4.1 SDK 面；壳层预装为默认内置（`market/builtin.rs` 首跑对账资源目录 `builtin-plugins/*.spkg`，trust="builtin"，声明高危位随安装授予）；旧内置 UI 并存灰度（`stores/builtin-apps.ts` 开关，设置→通用切换，加载失败自动回退）；
- 插件运行时已在：iframe 视图沙箱 + QuickJS 后台沙箱 + bridge（call / subscribe / event）+ 插件 SDK + `plugin_data` 8 命令 + market 12 命令；`spark-moments` 已有插件复用 DM 底座（feed 信封）的生产先例；`ai-chat` 桌面插件在运行。

## 三、差距

| # | 差距 | 出处 |
| --- | --- | --- |
| — | 插件数据 API 面已由 A18 落地（2026-09-08：sdk.messages/contacts/feed 三模块 + 6 权限位三重过滤对齐 + feed 权限归一[feed.deliver→feed:write、pull/订阅→feed:read] + ChatReceived/ContactsSynced 事件门控推送；等语义对照与权限拒绝测试全覆盖，spark-moments/ai-chat 回归绿）——G1/G2 的地基就绪 | — |
| — | G1/G2 已由 A19 落地（插件版默认内置 + 灰度开关可用；SDK 等语义补 `messages.send` 的 messageId 透传与 overview 的 groupTree/memberExtras 类型）。**v1 记录在案的缺口**（code/app/TODO.md「默认内置插件」节）：organization 域 SDK 面（邀请/成员管理/bot 求证）、壳层→插件意图通道（全局搜索跳转、顶栏 +、host.request 反向面）、identity 域扩展字段、应用会话挂载区仍在壳层、移动端壳层导航栈语义不接、头像/网络状态面桩、**系统未读徽标/标题前缀聚合面未上移壳层**（评审 2026-10-07 U3：iframe 内 document.title 不可见、插件无 system 桥面，插件版迁移期角标行为差异，待壳层基于内核会话快照自行聚合）——其余均不影响消息/通讯录数据面等价 | — |
| G3 | 群聊不存在（聊天插件内演进项，需插件层群协议设计） | catalog 聊天应用行 |
| — | G4 已由 A20 落地（2026-09-08：spark-chat 插件内本地过滤——关键词/来源屏蔽规则存插件自身数据域 `declareCollection`（`spark-chat:filter-rules`，scope:local 数据不离开本机），渲染前过滤纯本地行为内核零改动；规则 JSON 导入导出（导入校验结构，畸形拒绝）；UI 如实标注「本地过滤只影响自己的视图，不影响他人与网络」） | — |

## 四、目标设计

### 4.1 插件数据 API 面：消息与通讯录（#17 的地基）

插件化不是"把 Vue 搬进 iframe"——先把内核能力整理成**稳定的插件数据 API**（bridge 模块 + 权限位）：

| bridge 模块 | 接口面（摘要） | 权限 |
| --- | --- | --- |
| `sdk.messages` | 会话列表 / 读消息（分页）/ 发消息 / 撤回 / 标记已读 / 订阅新消息事件（event 推送） | `messages:read`、`messages:write`（均高危确认） |
| `sdk.contacts` | 好友列表 / 申请应答 / 标签分组 / 拉黑 / 订阅变更事件 | `contacts:read`、`contacts:write`（高危） |
| `sdk.feed` | 定向投递（feed 信封收发，§19.6 既有） | `feed:read`（订阅收件事件）、`feed:write` |

设计纪律：① 接口面是现有 Tauri 命令的**等语义移植**（同一 kernel 门面薄壳，行为不变）；② 事件经 bridge eventPump 推送（既有机制，与 sys_fetch_stream 同通路）；③ 插件只见密文处理后的明文结果——E2E 加解密永远在内核（私钥不出内核红线不变）。

### 4.2 聊天应用与通讯录应用（默认内置插件）

- 两个 Vue 界面从 `code/app/src` 迁移为独立插件包（源码进各自仓库，仓库锚定 L1；壳层预装为**默认内置**）；
- 数据归属不变：消息 / 通讯录在 sled，插件经 §4.1 API 访问——换插件数据原样还在；
- 壳层保留：空间切换器、设置骨架、应用会话列表挂载区（应用会话 / 服务号由插件呈现在聊天应用的消息列表，会话背后业务逻辑属各插件）；应用市场入口为**过渡期保留**，A34（市场界面插件化）后收敛，终态壳层只剩空间切换器与设置骨架（product/todo #17）；
- **灰度面更新（2026-09-08，docs/ui 阶段 3 用户导航重构）**：通讯录已转为**空间桌面插件窗口**（经 openPluginTab 全屏打开 spark-contacts），不再是壳层主 tab——灰度开关与 BuiltinAppHost contacts 分支已随导航重构收敛移除（无 legacy 壳层面）；聊天（messages）灰度开关保留。通讯录「换插件不动数据」的验收改由空间桌面窗口路径承载；
- 群聊（聊天插件内演进）：**插件层群协议**——群会话密钥由发起者经 1:1 DM 信封逐一投递给成员（内核只见普通信封），群消息经 feed 扇出；内核零新增，全部语义在聊天插件内（第一版单插件闭环）。

### 4.3 个人过滤规则（#22①）

聊天插件内实现本地过滤：关键词 / 来源屏蔽规则存插件自身数据域（`declareCollection`），过滤在渲染前应用——纯本地行为，内核零改动；规则可导入导出（换插件可携带）。

## 五、迁移路径

1. §4.1 API 面先行（SDK 三模块 + 权限位 + 事件），以 `spark-moments` 先行验证 feed 面；
2. 聊天 / 通讯录插件以**功能对等**迁移（同一组件树，数据源从 invoke Tauri 命令换成 SDK 调用）；
3. 灰度：旧内置 UI 与插件版并存一个版本（功能开关），用户无感切换后移除旧 UI；消息数据零迁移；
4. 群聊与过滤规则随聊天插件自身版本演进，不占内核排期。

## 六、验收

- **向量**：无协议线形变更；bridge 信封既有 v:1 不变；
- **单测**：SDK 三模块与 Tauri 命令的等语义对照（同输入同结果）；权限拒绝（未声明 `messages:write` 发消息被拒）；事件订阅（新消息 → 插件收到 event）；
- **集成**：第三方"最小聊天插件"（示例仓库）安装后读写同一消息数据——换前端数据原样在；灰度切换 e2e；
- **回归**：DM 底座全部既有测试（E2E / 离线 / 撤回 / 去重）、`spark-moments` / `ai-chat` 插件功能全绿。

---

> 关联：product/todo #17（§4.1–4.2）、#22①（§4.3）；architecture/plugins/runtime-and-trust.md 篇（沙箱与能力三重过滤的承载）；`p2p-dm.md`（DM 底座规格权威）。

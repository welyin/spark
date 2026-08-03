# 前端开发 TODO 清单

> 本文档统一记录新 UI（wiki/design/ui）开发过程中的 mock/假数据与未决事项。
> 代码中对应位置均有 `// TODO(mock): <简介>` 行内注释（少数为 `// TODO:`），
> 本表记录位置与详细内容，便于查阅和后续跟踪。行号以记录时为准，改动后请在文件内搜索 `TODO(mock)` 重新定位。

## 应用壳 / 顶部导航（ui-space-navbar）

- `src/stores/org-identity.ts:9` | 组织身份（昵称/头像/「使用个人身份」开关）存 localStorage | 待 `OrganizationMember.nickname/avatar` 字段与更新接口（设计 §9.4）
- `src/App.vue`（rail 测试入口） | 测试页正式发版隐藏 | 通过构建配置/环境变量隐藏测试入口且不打包其路由（设计 §6.4），当前始终可见
- 插件 SDK `sdk.space` | 当前 space 经桥握手 ctx.space（PluginContext）注入插件 | `sdk.space` 属性属 SDK 侧工作（设计 §11.4），未做
- `src/components/GlobalSearch.vue` | 顶栏全局搜索（纯前端模糊匹配，分组：联系人/会话/应用/组织） | 数据源复用 mock/contacts、mock/messages、mock/apps + 真实 `organization.listMine`/`pluginMarket.list`，随各 mock 替换自动切换真实数据

## 我的（个人中心）

- `src/components/mine/ProfileModule.vue:234` | 地区「定位」按钮仅演示交互 | Geolocation 只能拿到经纬度，缺少逆地理编码服务无法解析地级市；定位成功/失败后仍需手动选择或输入城市，待接入逆地理编码后自动填充
- `src/components/mine/ProfileModule.vue:103` | 「安全设置」分组为占位说明（修改登录密码、登录设备管理） | 待内核安全相关接口落地
- `src/components/mine/ProfileModule.vue:113` | 「隐私设置」分组为占位说明（发现设置、黑名单、朋友权限） | 待真实隐私模型落地；设置页「当前空间」有同名 mock 开关组

## 设置页（ui-space-navbar §6）

- `src/components/settings/SystemSettingsPanel.vue:83` | 关于页版本号硬编码 0.1.0 | 待接入构建注入的真实版本/构建信息
- `src/components/settings/SystemSettingsPanel.vue:150` | 系统设置「通用」开关组不生效 | 待主题/语言/字体持久化方案
- `src/components/settings/SystemSettingsPanel.vue:157` | 系统设置「通知」开关组不生效 | 同上
- `src/components/settings/SystemSettingsPanel.vue:164` | 系统设置「隐私」开关组不生效 | 同上
- `src/components/settings/MockSettingGroup.vue:8` | 开关组统一 mock 说明 | 随上述各组落地后移除该组件

（以下已落地，仅保留记录）
- `src/components/settings/ProxySettings.vue` | HTTP 代理设置已真实生效（`src-tauri/src/proxy.rs`） | 已知限制：修改代理需重启应用

## 消息（ui-messages）

> ✅ 已落地：内核 `message` 模块（sled 持久化）+ `/spark/dm/1.0.0` 直连协议 + `messages.*` 命令域。`src/mock/messages.ts` 已改为「内核真实数据 + 内存响应式缓存」接入层。

- `src/components/messages/ChatHeader.vue` | 语音/视频通话按钮占位 | 待真实音视频通话能力
- 未做条目 | 语音/图片/文件发送（按钮占位）、消息转发/多选、聊天记录搜索与导出、离线提示、桌面通知气泡 | 均依赖后续消息/通知能力

（以下已落地，仅保留记录）
- 系统会话 | 已落地：内置 system 应用会话（`app:system`），壳层经 `src/app-messages.ts` 写入；当前仅「插件安装/升级成功」一条通知源
- 应用会话 | 已落地：SDK `sdk.messages` + 桥 dispatcher + 卡片富渲染（`AppMessageCard.vue` iframe）+ 原生摘要降级
- 应用会话-卡片回调 | 已落地：卡片 action 经桥上行 → 壳层归属校验 → 主视图 `onCardAction`；主视图未运行时 action 丢弃（设计允许）
- 应用会话-卡片高度 | 已落地：插件经 `sdk.messages.requestCardHeight` 申请，壳层封顶 400px
- 应用会话-屏蔽 | 已落地：壳层 localStorage 持久化（`spark:app-conv-blocked`）
- 链接预览 | 发送方壳层本地抓取 OG/Twitter Card，接收方只展示；非 Tauri 退化为诚实占位
- 标题未读数 | 已实现 `document.title (n) 星火 Spark`
- `src/stores/pending-chat.ts` | 跨页「打开会话」请求 | 已通且数据真实
- 离线投递 | 对端不可达即 failed（可手动重发），无离线队列 | 待后续专项
- 消息加密 | 传输层依赖 libp2p Noise，未做应用层 E2E | 待协议规格与专项
- 应用会话-清空 | 内核只有 appDeleteConversation，无逐条清空 | 如产品需要补内核 clear 接口

## 通讯录（ui-contacts）

> ✅ 已落地：内核 `contact` 模块（朋友/申请/标签/分组树/成员附加资料 sled 持久化）+ `contacts.*` 命令域；好友申请双向确认走 `/spark/dm/1.0.0` 直连。`src/mock/contacts.ts` 已改为「内核真实数据 + 内存响应式缓存」接入层。

- `src/mock/contacts/types.ts:95` | 内核组织成员已携带真实身份字段（OrganizationMember 的） | 待补充
- `src/components/contacts/ContactPanel.vue:33` | 照片为色块占位 | 未接入真实上传/存储
- `src/components/contacts/ContactPanel.vue:134` | 插件子开关未做 | §6.2 按插件细分的权限开关待插件数据共享落地

（以下已落地，仅保留记录）
- 多设备配对 | 每 rootId 一条联系人记录，至多一台设备 | 多设备需设备清单模型（协议 §19.4）
- 名片二维码 | 已落地：编码真实节点名片（RootID + peerId/监听地址），节点未连接降级只含 RootID
- `src/stores/pending-contact.ts` | 跨页「打开联系人资料」 | 已通且数据真实
- `src/components/contacts/open-intents.ts` | 消息页空状态→通讯录跳转 | 复用 `spark:open-contact` 事件
- `src/components/contacts/TagManager.vue` | 标签成员编辑直写 reactive | 已由 deep watch 兜底持久化
- 拉黑 | 已真实落库；消息层过滤已生效
- 添加朋友/接受申请 | 双向确认已真实投递；离线时不可达（outbox 留 pending）
- `src/utils/pinyin.ts:4` | 拼音首字母映射表不全 | 生僻字落入 `#` 组；待换 pinyin 库
- `src/pages/ContactsPage.vue:221` | 搜索不支持拼音 | 暂只匹配名字/备注/标签/RootID
- `src/components/contacts/use-delete-contact.ts` | 删除朋友已真实 | §5.5「删除同时自动拉黑」选项待做
- `src/components/contacts/AddFriendDialog.vue` / `src/components/org/InviteMemberDialog.vue` | 仅本地模式拦截提交 | 真实实现已由内核对不可达投递置 failed/保留 pending
- 未做条目 | 置顶联系人分组（§2.3）、音视频通话按钮、扫码识别 | 待真实模型/能力

## 应用与市场（ui-apps-market）

> ✅ 已落地：仓库锚定安装（repo.rs）、广播索引（plugin_announce.rs + announce_verify.rs）、市场 UI 分区（收录/探索）、.spkg 侧载导入（market/sideload.rs）、插件按空间过滤展示（space-visibility.ts）、iframe 沙箱运行时（plugin_src.rs + PluginIframeHost.vue + 桥 dispatcher + 熔断）、示例插件 spark-example（原 weibo-core 更名迁移）。

- ✅ 已落地：Tauri 命令调用方域校验（`src-tauri/src/domain_guard.rs`，`requireSystemDomain` 等价物）——`market.rs` 全部命令入口校验调用方 webview URL 属系统域白名单（dev `http://localhost:1420` / 生产 `tauri://localhost`、`http(s)://tauri.localhost`），`plugin://` 与 `http(s)://plugin.localhost` 插件源一律拒绝（fail-closed）。边界：当前插件 iframe 是 opaque origin 沙箱本就无法直接 invoke；本守卫为独立插件窗口（`plugin-open-view` 排期）提前落地 URL 层拦截
- 仓库锚定安装-升级 | 已落地安装/解析/预览 | 升级探测只认内置目录，仓库锚定安装永不出现「可更新」；待排期
- `src/mock/apps.ts:11` | 伪造 6 个市场应用（论坛/投票/日历/任务看板/朋友圈/文件） | 真实市场仅 spark-example；仅 mock 模式与 `pluginMarket.list()` 合并展示；待市场数据充足后删除
- `src/mock/apps.ts:125` | mock 应用安装/启用状态存 localStorage | 待真实市场接口替换
- `src/components/apps/apps-store.ts:39` | 市场细分分类前端映射 | 仅有 foundation/business 粗分类；待市场数据带分类字段
- `src/components/apps/apps-store.ts:95` | 应用分组归属 localStorage | 待内核分组模型替换
- `src/components/apps/apps-store.ts:156` | 组织空间启用状态 localStorage | 待内核接口
- `src/components/apps/AppDetailPanel.vue:51` | 源码仓库与签名指纹缺失 | 市场数据无对应字段
- `src/pages/AppsPage.vue:274` | 联系管理员 toast 占位 | 待打开与管理员 1:1 聊天
- `src/stores/pending-app.ts` | 跨页「打开应用详情」请求 | 已通，条目来自真实 pluginMarket.list + mock 合并

（以下已落地，仅保留记录）
- 插件入口契约 | 已落地：独立 SDK 包 `@spark/plugin-sdk`，插件侧只依赖该包 + `manifest.json`
- iframe 沙箱运行时 | 已落地：`plugin://` 源服务 + CSP + 桥握手 + 权限中间件 + 熔断
- 示例插件 spark-example | 已落地：weibo-core 更名迁移，发帖→应用会话卡片通知→评论线程
- 广播索引 | 已落地：PoW + gossipsub + 懒惰核查 + relay 资历制
- 市场 UI 分区 | 已落地：收录/探索分区 + 「换一批」洗牌 + 搜索直达
- .spkg 侧载导入 | 已落地：文件选择→inspect→import（sha256 校验 + trust="sideloaded"）
- 插件按空间过滤 | 已落地：`space-visibility.ts` + 三处同口径过滤

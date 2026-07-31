# 前端开发 TODO 清单

> 本文档统一记录新 UI（wiki/design/ui）开发过程中的 mock/假数据与未决事项。
> 代码中对应位置均有 `// TODO(mock): <简介>` 行内注释（少数为 `// TODO:`），
> 本表记录位置与详细内容，便于查阅和后续跟踪。行号以记录时为准，改动后请在文件内搜索 `TODO(mock)` 重新定位。

## 应用壳 / 顶部导航（ui-space-navbar）

- `src/stores/org-avatars.ts:9` | 组织 logo 存 localStorage | 待 `OrganizationRecord.avatar` 后端字段落地后改为读写组织记录（设计 §11.2）
- `src/stores/org-identity.ts:9` | 组织身份（昵称/头像/「使用个人身份」开关）存 localStorage | 待 `OrganizationMember.nickname/avatar` 字段与更新接口（设计 §9.4）
- `src/components/org/OrgIdentityCard.vue:16` | 「我的组织资料」对话框写 localStorage（MinePage「组织身份」卡片，原 UserAvatarMenu 下拉项） | 同上，待内核组织身份接口
- `src/components/org/OrgIdentityCard.vue:69` | 组织身份缺省占位名「成员」 | 待后端组织身份接口（设计 §9.2）
- `src/components/org/OrgIdentityCard.vue:22` | 「使用个人身份」开关仅本地生效（MinePage「组织身份」卡片，原 UserAvatarMenu 下拉项） | 待后端组织身份接口（设计 §9.3）
- `src/App.vue`（rail 测试入口） | 测试页正式发版隐藏 | 通过构建配置/环境变量隐藏测试入口且不打包其路由（设计 §6.4），当前始终可见
- `src/components/SpaceSwitcher.vue` 等 | 组织 logo 展示链路 | 统一走 org-avatars store；后端字段落地后整体替换
- 插件 SDK `sdk.space` | 当前 space 经桥握手 ctx.space（PluginContext）注入插件 | `sdk.space` 属性属 SDK 侧工作（设计 §11.4），未做
- `src/components/GlobalSearch.vue` | 顶栏全局搜索（纯前端模糊匹配，分组：联系人/会话/应用/组织） | 数据源复用 mock/contacts、mock/messages、mock/apps + 真实 `organization.listMine`/`pluginMarket.list`，随各 mock 替换自动切换真实数据
- `src/components/common/TermLabel.vue` | 术语通俗化映射表（RootID→身份 ID、PeerID→节点 ID、P2P Addresses→节点地址） | 前端展示层约定；tooltip 白话解释如需改文案统一改这里

## 我的（个人中心）

- `src/stores/profile-extra.ts:9` | 我的资料扩展字段（性别/地区/签名）按 rootId 存 localStorage（`spark:profile-extra`） | 内核 `rootIdentity.updateProfile` 目前仅支持 nickname/avatar，待内核资料字段扩展后迁移真实接口
- `src/components/mine/ProfileModule.vue:234` | 地区「定位」按钮仅演示交互 | Geolocation 只能拿到经纬度，缺少逆地理编码服务无法解析地级市；定位成功/失败后仍需手动选择或输入城市，待接入逆地理编码后自动填充（四栏改造后自 ProfileFieldDrawer 迁入，编辑已内联第四栏）
- `src/components/mine/ProfileModule.vue:103` | 「安全设置」分组为占位说明（修改登录密码、登录设备管理） | 待内核安全相关接口落地
- `src/components/mine/ProfileModule.vue:113` | 「隐私设置」分组为占位说明（发现设置、黑名单、朋友权限） | 待真实隐私模型落地；设置页「当前空间」有同名 mock 开关组
- `src/components/mine/MyCardModule.vue:99` | 名片「分享链接」为演示数据（按 RootID 拼的占位 URL） | 待名片链接服务与扫码/打开链路落地
- `src/components/mine/BackupModule.vue:45` | 「加密导出」备份方式为占位 | 内核无对应接口，未来能力
- `src/components/mine/BackupModule.vue`（验密） | 二维码/助记词备份统一先验登录密码 | 内核无独立验密接口，复用 `rootIdentity.revealMnemonic` 验密（二维码备份验密后丢弃助记词结果）；若内核补验密接口可替换
- `src/components/mine/NetworkModule.vue` | 网络状态「高级模式」开关存 localStorage（`spark:network-advanced`） | 前端展示层偏好，普通用户视图只显示简单状态；如需跨设备同步待设置持久化方案

## 设置页（ui-space-navbar §6）

- `src/pages/SettingsPage.vue:41` | 个人空间「通知」开关组不生效 | 待通知设置的后端/持久化方案
- `src/pages/SettingsPage.vue:49` | 个人空间「隐私」开关组不生效 | 同上（含黑名单管理入口占位）
- `src/components/settings/SystemSettingsPanel.vue:83` | 关于页版本号硬编码 0.1.0 | 待接入构建注入的真实版本/构建信息
- `src/components/settings/SystemSettingsPanel.vue:150` | 系统设置「通用」开关组不生效 | 待主题/语言/字体持久化方案
- `src/components/settings/SystemSettingsPanel.vue:157` | 系统设置「通知」开关组不生效 | 同上
- `src/components/settings/SystemSettingsPanel.vue:164` | 系统设置「隐私」开关组不生效 | 同上
- `src/components/settings/MockSettingGroup.vue:8` | 开关组统一 mock 说明 | 随上述各组落地后移除该组件

## 消息（ui-messages）

> ✅ 已落地：内核 `message` 模块（sled 持久化）+ `/spark/dm/1.0.0` 直连协议（Ed25519 根身份签名信封，协议规格 wiki/protocol/p2p-messages.md §19）+ `messages.*` 命令域。`src/mock/messages.ts` 已改为「内核真实数据 + 内存响应式缓存」接入层（导出签名不变，组件零改动）：会话/消息水合、发送/重发/撤回/置顶/免打扰/草稿/清空/删除全部落库；状态流转真实（sending→delivered→read，不可达→failed）；对方消息经 `ChatReceived`/`ChatStatus` 事件推送；会话 id 约定 `dm:{peerRootId}`；非 Tauri 环境（单测/纯前端预览）退化为纯内存。

- `src/mock/messages.ts`（链接预览） | 仍按域名白名单映射生成 | 真实实现由发送方本地抓取 OG/Twitter Card 元数据（§6.4），待专项
- 离线投递 | 对端不可达即 failed（可手动重发），无离线队列 | 个人空间自动重发 + 组织网关暂存转发（§8.4）待后续专项
- 消息加密 | 传输层依赖 libp2p Noise，信封 Ed25519 签名鉴权；未做应用层 E2E | §8.5 的 X25519 1:1 会话密钥协商待协议规格与专项
- 系统会话（系统通知/组织公告） | 无真实数据源，不再播种 | 待系统通知/管理员公告能力（§8.3）
- `src/mock/messages.ts:420` 附近 | 标题未读数（§7.1） | 已实现 document.title `(n) 星火 Spark`；托盘角标/任务栏徽标待真实通知能力
- `src/components/messages/ChatHeader.vue` | 语音/视频通话按钮占位 | 顶部栏电话+视频两个 icon，点击 toast「通话功能将在下一期实现」（UI 评审 P1-1）；待真实音视频通话能力
- 未做条目 | 语音/图片/文件发送（按钮占位）、消息转发/多选、聊天记录搜索与导出、离线提示「消息将在对方上线后送达」（§8.4）、桌面通知气泡 | 均依赖后续消息/通知能力
- `src/stores/pending-chat.ts` | 跨页「打开会话」请求 | 联动链路已通且数据已真实（无需改此模块）

## 通讯录（ui-contacts）

> ✅ 已落地：内核 `contact` 模块（朋友/申请/标签/分组树/成员附加资料 sled 持久化）+ `contacts.*` 命令域；好友申请双向确认走 `/spark/dm/1.0.0` 直连（扫码名片解析节点地址，申请/接受互推，事件 `FriendRequestReceived`/`FriendRequestAccepted` 实时入账）。**个人空间联系人默认包含自己**（内核 overview 恒注入自条目；给自己发消息=向所有已配对个人设备直发同步；设备配对=扫自己另一台设备名片，对端自动接受）。`src/mock/contacts.ts` 已改为「内核真实数据 + 内存响应式缓存」接入层（导出签名与本地逻辑不变，组件与 contact-groups 测试零改动）；组件直写 reactive 对象的路径（标签成员/照片增删）由 deep watch 兜底持久化。非 Tauri 环境保留种子数据（渲染单测依赖）；Tauri 下默认真实内核数据，种子演示数据仅在 mock 模式（`npm run tauri:mock`，或 localStorage `spark:demo-contacts` 置 '1'，见 `demoContacts()`）启用。

- 多设备配对 | 存储模型每 rootId 一条联系人记录，至多配对一台设备 | 多设备需设备清单模型（协议 §19.4），待专项

- `src/mock/contacts.ts` | 照片只存占位标记（'photo-N'） | 色块占位，未接入真实上传/存储，待专项
- 添加朋友-搜索身份 ID | 仅 RootID 无法解析节点地址时后端报「无法确定对方节点地址，请使用扫码名片添加」 | RootID→节点地址的发现机制（DHT 按 rootId 索引）待专项；扫码名片链路已通
- 组织空间「新的成员」申请 | 返回空列表（真实加入走邀请码 §4.2，无申请入库） | 若未来要申请制需另立模型
- 拉黑 | 已真实落库；消息层过滤已生效（入站 chat/friend-request 被拒，对方收 failed） | 网络层拉黑为长期规划（§7.4）
- 添加朋友/接受申请 | 双向确认已真实投递 | 对方离线时申请不可达（outbox 留 pending），离线投递待网关专项
- `src/utils/pinyin.ts:4` | 拼音首字母映射表不全 | 生僻字落入 `#` 组；待换 pinyin 库或 Collator 边界判定（§10），调用方不变
- `src/pages/ContactsPage.vue:221` | 搜索不支持拼音 | §2.4 要求按拼音搜索，映射表无全拼，暂只匹配名字/备注/标签/RootID
- `src/components/contacts/use-delete-contact.ts` | 删除朋友已真实（removeFriend 落库） | §5.5「删除同时自动拉黑（可选）」选项待做
- `src/components/contacts/ContactPanel.vue:33` | 照片为色块占位 | 未接入真实上传/存储
- `src/components/contacts/ContactPanel.vue:134` | 插件子开关未做 | §6.2 按插件细分的权限开关待插件数据共享落地
- `src/components/mine/MyCardModule.vue` | 名片二维码编码真实节点名片（RootID + p2p.info 的 peerId/监听地址，JSON） | 节点未连接时降级只编码 RootID；扫码添加好友链路已通（sendRequest 解析名片），摄像头扫码识别待做（§9.1）
- `src/components/contacts/AddFriendDialog.vue` / `src/components/org/InviteMemberDialog.vue` | 仅本地模式拦截提交 | 仅本地（isLocalOnly）时 toast 拦截；真实实现已由内核对不可达投递置 failed/保留 pending
- `src/stores/pending-contact.ts` | 跨页「打开联系人资料」请求 | 联动链路已通且数据已真实（无需改此模块）
- `src/components/contacts/open-intents.ts` | 消息页空状态→通讯录跳转意图哨兵 | 复用 `spark:open-contact` 事件；若 App.vue 支持空 rootId 切 tab 可移除哨兵
- `src/components/contacts/TagManager.vue` | 标签成员编辑直写 reactive 对象 | 已由 deep watch 兜底持久化；如未来改组件可显式走 updateProfile
- 未做条目 | 置顶联系人分组（§2.3）、音视频通话按钮、扫码识别（摄像头未接入） | 待真实模型/能力

## 应用与市场（ui-apps-market）

- `src/mock/apps.ts:11` | 伪造 6 个市场应用（论坛/投票/日历/任务看板/朋友圈/文件） | 真实市场仅 weibo-core，UI 太空；仅 mock 模式（`npm run tauri:mock`，`VITE_MOCK=1`）与 `pluginMarket.list()` 结果合并展示（`AppsPage.vue` mergeItems / `GlobalSearch.vue`，开关 `src/mock/mode.ts`），待市场数据充足后整体删除
- `src/mock/apps.ts:125` | mock 应用安装/启用状态存 localStorage（`spark:mock-apps-state`，默认预装日历+任务看板） | 「打开」为 toast 占位不走插件链路；安装/启停在 `AppsPage.vue` 按 `isMockApp` 分支处理；待真实市场接口替换
- `src/components/apps/apps-store.ts:39` | 市场细分分类前端映射 | 市场条目只有 `foundation`/`business` 粗分类，§3.3 的办公/社交/工具/其他按名称+简介关键字启发式映射；待市场数据带分类字段后删除
- `src/components/apps/apps-store.ts:95` | 应用分组归属 localStorage | 分组归属、自定义分组内核无接口，按空间存 localStorage（`spark:apps-groups:*`）；待内核分组模型替换
- `src/components/apps/apps-store.ts:156` | 组织空间启用状态 localStorage | 设计 §4.2 组织启用由管理员统一管理但内核无接口，按 orgId 存 localStorage（`spark:apps-org-enabled:*`）
- `src/components/apps/AppDetailPanel.vue:51` | 源码仓库与签名指纹缺失 | 市场数据无源码仓库地址、Ed25519 域名签名/公钥指纹字段（设计 §3.4/§6.2），详情页显示占位文案；GitHub Star 等实时信息（§3.4）因此整体未做
- `src/components/apps/AppDetailPanel.vue:108` | 无卸载按钮 | `pluginMarket` 无 `uninstall` 接口（设计 §4.1 有卸载语义），待内核补接口
- `src/pages/AppsPage.vue:274` | 联系管理员 toast 占位 | 非管理员点「启用」按 §5.2 弹确认框；点联系管理员仅 toast，待打开与管理员 1:1 聊天并发送应用链接卡片（§5.2 第 4-5 步，可复用 `spark:open-chat` 链路）
- `src/stores/pending-app.ts` | 跨页「打开应用详情」请求（全局搜索→应用页详情视图） | 联动链路已通；条目来自真实 pluginMarket.list + mock 合并列表
- 插件入口契约已落地 | 插件与壳层双向耦合已消除：独立 SDK 包 `@spark/plugin-sdk`（`../packages/plugin-sdk`，纯类型 + `definePlugin` 入口契约（第三方约定保留）+ `getPluginSDK`/`ensurePluginSDK` 全局注入点读取），插件侧只依赖该包 + 声明式 `manifest.json`（`../plugins/weibo-core`）；壳层编译期注册（plugin-loader/plugin-view-registry）已退役，插件装载统一走 iframe 沙箱桥握手；插件测试已迁至 `../plugins/weibo-core/tests/`
- iframe 沙箱运行时（阶段 A 第三波收尾） | 已落地：`plugin://` 源服务（`src-tauri/src/plugin_src.rs`，安装包 .spkg 优先、内置 dist 兜底；安装包定位只信市场状态 packagePath，fail-closed；CSP + 路径穿越防护；Windows 经 `http://plugin.localhost` workaround）+ vite dev 中间件（`vite.config.ts`）+ 宿主组件 `src/components/plugin/PluginIframeHost.vue`（srcdoc 沙箱 iframe + 桥握手）+ 权限中间件 `src/plugin-bridge-dispatcher.ts`（grantedPermissions ∩ view 裁剪 ∩ space 三重过滤，identity:sign 使用时询问）+ 熔断 `src/plugin-watchdog.ts`/`src/plugin-disabled.ts`（心跳无响应覆盖层、崩溃环自动停用、错误经桥 `runtime-error` 上报）；旧 registry tab 路径已退役（`plugin-view-registry.ts`/`plugin-loader.ts` 删除，`plugin-sdk-browser.ts` 收敛为桥后端工厂，App.vue 插件 tab 统一 PluginIframeHost——iframe 成为唯一加载路径，weibo-core 全面迁移），AppsPage 卡片级「已停用」置灰+徽标（读 plugin-disabled） | 遗留：停用状态存 localStorage `spark:plugin-disabled` 待内核实例状态接口；.spkg 解析缓存、sdk.space（见上）、message-card 端到端、桥调用限流；运行中权限回收对已开 tab 不生效（grantedPermissions 为桥建立时快照，待权限变更事件使桥失效重建）；`ctx.theme` 为握手时快照（theme-changed 事件推送接线待做，壳层主题切换不实时同步已开插件）
- 未做条目 | 拖拽移动分组（现为右键菜单）、分组重命名/删除、开发者模式安装未签名应用（§6.2） | 拖拽为体验增强；开发者模式待内核验签策略

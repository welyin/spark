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
- `src/components/settings/ProxySettings.vue`（网络状态-网络代理） | HTTP 代理设置已真实生效（`src-tauri/src/proxy.rs`：`spark-proxy.json` 持久化 + `SPARK_PROXY`/`HTTPS_PROXY`/`ALL_PROXY` 环境变量注入，覆盖 updater 与市场 GitHub 链路） | 已知限制：市场 OnceLock 静态客户端与 updater 客户端创建后定型，修改代理需重启应用才对已建立链路生效（保存时 toast 已提示）

## 消息（ui-messages）

> ✅ 已落地：内核 `message` 模块（sled 持久化）+ `/spark/dm/1.0.0` 直连协议（Ed25519 根身份签名信封，协议规格 wiki/protocol/p2p-messages.md §19）+ `messages.*` 命令域。`src/mock/messages.ts` 已改为「内核真实数据 + 内存响应式缓存」接入层（导出签名不变，组件零改动）：会话/消息水合、发送/重发/撤回/置顶/免打扰/草稿/清空/删除全部落库；状态流转真实（sending→delivered→read，不可达→failed）；对方消息经 `ChatReceived`/`ChatStatus` 事件推送；会话 id 约定 `dm:{peerRootId}`；非 Tauri 环境（单测/纯前端预览）退化为纯内存。

- `src/mock/messages.ts`（链接预览） | 仍按域名白名单映射生成 | 真实实现由发送方本地抓取 OG/Twitter Card 元数据（§6.4），待专项
- 离线投递 | 对端不可达即 failed（可手动重发），无离线队列 | 个人空间自动重发 + 组织网关暂存转发（§8.4）待后续专项
- 消息加密 | 传输层依赖 libp2p Noise，信封 Ed25519 签名鉴权；未做应用层 E2E | §8.5 的 X25519 1:1 会话密钥协商待协议规格与专项
- 系统会话（系统通知/组织公告） | 系统通知已有真实数据源：内置 system 应用会话（`app:system`，按空间隔离，p2p-messages.md §20），壳层经 `src/app-messages.ts` 以 pluginId='system' 写应用消息；本波只接「插件安装/升级成功」一条通知源作样板 | 其余通知源（安全告警、更新提示等）与组织公告（§8.3）待接
- 应用会话（服务号模型 §20） | 已落地：SDK `sdk.messages`（sendAppMessage/listAppMessages/markRead/onCardAction + 卡片侧 triggerCardAction/requestCardHeight），桥 dispatcher messages 域按绑定身份注入 pluginId/space（`message:app` 高级权限，manifest 声明校验已加入内核权限表）；会话列表应用分组（标题取插件清单名、头像按 pluginId 哈希渐变）、聊天区卡片富渲染（`AppMessageCard.vue` 轻量 iframe 宿主，viewType='message-card'）/原生摘要降级（未装插件给「安装插件查看完整内容」跳市场）；免打扰/置顶/删除复用人际会话操作，屏蔽为本地持久化（`spark:app-conv-blocked`，抑制未读角标与聚合） | 用户→应用文本指令未实现（设计双向，阶段裁剪：输入区禁用「应用会话不支持回复」，ChatView 仅渲染应用消息流）
- 应用会话-卡片回调 | 卡片按钮 action 经桥上行 → 壳层归属校验（cardId 必须为该插件在架卡片）→ 主视图实例 onCardAction | **主视图实例未运行时 action 直接丢弃**（设计允许；如需可靠投递需排队补投机制，待专项）
- 应用会话-卡片高度 | 插件经 `sdk.messages.requestCardHeight` 申请，壳层封顶 400px | 默认高度 180px 为暂定值；卡片视图构建约定（插件如何区分打包 app/card 视图入口）随首个带卡插件落地
- 应用会话-屏蔽 | 仅壳层本地持久化（localStorage），被屏蔽会话消息仍写入内核、列表可见 | 如需跨设备/内核级屏蔽（写入即丢弃）待内核接口
- 应用会话-清空 | 应用会话无「清空聊天记录」入口（内核只有 appDeleteConversation，无逐条清空） | 如产品需要补内核 clear 接口
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

- Tauri 命令调用方域校验 | `src-tauri/src/commands/market.rs` 等命令域无 `requireSystemDomain` 等价物（旧 TS IPC 有） | 域隔离（独立插件窗口）未落地前，插件 iframe 与系统域同源可 invoke 市场命令：`plugin-market-uninstall` 为纯破坏性操作，install/upgrade 同样暴露；前端 UI 守卫（AppsPage/AppListPanel/AppDetailPanel 按空间/角色隐藏入口）不构成安全边界，待命令层域校验落地
- 仓库锚定安装（阶段 C 波次 1） | 已落地：协议规格 wiki/protocol/plugin-dist.md；`src-tauri/src/market/repo.rs`（id 解析/规范化、三托管平台 URL 模板、gh-proxy 镜像展开、声明文件双源交叉、id 一致性校验、签名可选 trust=repo-anchored、声明缓存内存 10min + sled `plugin:repo:<id>`）；命令 `plugin-market-resolve-repo` / `plugin-market-install-from-repo`；市场页「按仓库地址安装」入口（AppMarketPanel 对话框：解析→预览名称/图标/简介/权限→授权安装）；目录外已装插件按声明缓存合成市场条目；插件源链路支持 repo id（前端 encodeURIComponent 单段传输，plugin_src.rs/vite 中间件解码） | 待做：repo 插件升级/更新探测（现 upgrade 只认内置目录）；sled 声明缓存无刷新策略（装过的版本会长期命中缓存）；jsDelivr 源未消费。**已知限制（显式标注）：repo-anchored / signed 仓库安装无更新通路**——`checkForUpdates`/`upgrade` 只认内置目录，仓库锚定安装永不出现「可更新」，需重跑 installFromRepo 才能升级（更新探测通路待排期）
- 广播索引（阶段 C 波次 2a） | 已落地：协议规格 wiki/protocol/plugin-dist.md §8（消息结构/PoW hashcash 前 20 bit/TTL 30 天/relay 资历制 72h/逐 peer 限流 10 条每小时/本地索引 sled `mkt:ann:` 限量 1 万 + LRU/懒惰核查只有 verified 进市场视图）；内核 `core/src/p2p/plugin_announce.rs`（codec/PoW/校验链/索引 store）+ gossipsub `/spark/plugin-announce/1.0.0` 订阅发布（gossipsub 开启 validate_messages，overlay/sync 无条件 Accept 保持历史语义）+ relay 资历门控（按 peer 接入时长回报 Accept/Ignore/Reject）；壳层 `announce_verify.rs` 懒惰核查队列（后台 resolve_repo_plugin → verified 终态回写内核索引持久化）；命令 `plugin-market-announce-publish/list/get` + 市场页「发布声明（开发者）」入口；事件 `p2p-event` 的 PluginAnnounceReceived/Verified 及独立 `plugin-announce-received/verified` 推渲染端 | 待做：repo 插件升级/更新探测；声明续期提醒（30 天 TTL 到期重广播）
- 市场 UI 分区（阶段 C 波次 2b，plugin_system.md「市场展示与排序」） | 已落地：市场页「收录/探索」分区（默认收录层）；探索页 = verified 广播条目随机排序 + 「换一批」洗牌 + 搜索直达（名称/简介/id，搜索走 updatedAt 稳定序不用随机序），空态引导文案，详情 resolveRepo 校正 + 权限列表 + 复用波次 1 安装链路，`plugin-announce-verified` 事件增量并入但不打断当前洗牌序（提示「换一批查看」）；纯逻辑 `src/components/apps/apps-explore.ts` + 单测 | 待做：组织白名单来源（数据结构已预留 `AppMarketPanel.vue` orgWhitelistItems 占位分组，待组织管理接口下发管理员推荐清单）；联系人使用信号（web-of-trust 排序信号）；举报降权（只作用排序层）；pending 条目开发者模式状态展示（非必须，未做）
- .spkg 侧载导入（阶段 C 波次 2b，网络差降级） | 已落地：市场页「导入 .spkg 文件」入口（文件选择 → inspect 预览名称/版本/权限/**整包 sha256 供核对** → import 复核哈希 + 逐文件 sha256/size 校验 → 落状态 trust="sideloaded"）；命令 `plugin-market-inspect-local/import-local`（`src-tauri/src/market/sideload.rs` + 单测）；capabilities 增 `dialog:allow-open`；仓库不可达安装失败提示侧载路径；探索详情 resolveRepo 失败降级展示仓库地址 | 待做：侧载插件的升级链路（现 upgrade 只认内置目录，侧载/repository 插件同此限制）
- 插件按空间过滤展示（spaces-and-plugins §4） | 已落地：纯逻辑 `src/components/apps/space-visibility.ts`（`isPluginVisibleInSpace`，supportedSpaces 缺省/空数组按 ['org']）+ 单测；已安装列表（AppsPage `installedItems`）、市场收录层（`visibleItems`）、探索层（AppExplorePanel 取 `corrected.supportedSpaces`，缺省同口径）三处同口径过滤；`openApp` 直达守卫 toast 拒绝。数据打通：目录插件 `catalog.rs`  vendored `supportedSpaces`（与 `code/plugins/<id>/manifest.json` 保持一致）；repo 安装取声明文件 `supportedSpaces`（plugin-dist §2.1 新增可选字段，校验非空且只含 personal/org）；侧载安装解析包内 manifest.json 落 `InstalledPluginState.supportedSpaces`；探索层经懒惰核查回写 `CorrectedAnnounceFields.supportedSpaces`；mock 应用（朋友圈=['personal']、日历=['personal','org']，其余=['org']）演示过滤；全局搜索（GlobalSearch `appResultItems`）同口径过滤，`openPendingAppDetail`/`installApp`/`installRepoPlugin` 入口空间守卫 toast 拒绝；探索层过滤在 computed 链路随空间切换响应。历史数据代价（已处理）：旧 verified 索引条目（corrected 缺 supportedSpaces）启动一次性重核查补齐（`CorrectedAnnounceFields.supportedSpacesChecked` 标记防反复重核），旧侧载安装记录启动对账重解析包内 manifest 回填 supportedSpaces | 仅 UI 展示过滤，已装但当前空间不可见插件的应用会话/通知链路不受影响
- `src/mock/apps.ts:11` | 伪造 6 个市场应用（论坛/投票/日历/任务看板/朋友圈/文件） | 真实市场仅 spark-example，UI 太空；仅 mock 模式（`npm run tauri:mock`，`VITE_MOCK=1`）与 `pluginMarket.list()` 结果合并展示（`AppsPage.vue` mergeItems / `GlobalSearch.vue`，开关 `src/mock/mode.ts`）；收录层卡片已明确标注「演示数据」tag（波次 2b），待市场数据充足后整体删除
- `src/mock/apps.ts:125` | mock 应用安装/启用状态存 localStorage（`spark:mock-apps-state`，默认预装日历+任务看板） | 「打开」为 toast 占位不走插件链路；安装/启停在 `AppsPage.vue` 按 `isMockApp` 分支处理；待真实市场接口替换
- `src/components/apps/apps-store.ts:39` | 市场细分分类前端映射 | 市场条目只有 `foundation`/`business` 粗分类，§3.3 的办公/社交/工具/其他按名称+简介关键字启发式映射；待市场数据带分类字段后删除
- `src/components/apps/apps-store.ts:95` | 应用分组归属 localStorage | 分组归属、自定义分组内核无接口，按空间存 localStorage（`spark:apps-groups:*`）；待内核分组模型替换
- `src/components/apps/apps-store.ts:156` | 组织空间启用状态 localStorage | 设计 §4.2 组织启用由管理员统一管理但内核无接口，按 orgId 存 localStorage（`spark:apps-org-enabled:*`）
- `src/components/apps/AppDetailPanel.vue:51` | 源码仓库与签名指纹缺失 | 市场数据无源码仓库地址、Ed25519 域名签名/公钥指纹字段（设计 §3.4/§6.2），详情页显示占位文案；GitHub Star 等实时信息（§3.4）因此整体未做
- `src/pages/AppsPage.vue:274` | 联系管理员 toast 占位 | 非管理员点「启用」按 §5.2 弹确认框；点联系管理员仅 toast，待打开与管理员 1:1 聊天并发送应用链接卡片（§5.2 第 4-5 步，可复用 `spark:open-chat` 链路）
- `src/stores/pending-app.ts` | 跨页「打开应用详情」请求（全局搜索→应用页详情视图） | 联动链路已通；条目来自真实 pluginMarket.list + mock 合并列表
- 插件入口契约已落地 | 插件与壳层双向耦合已消除：独立 SDK 包 `@spark/plugin-sdk`（`../packages/plugin-sdk`，纯类型 + `definePlugin` 入口契约（第三方约定保留）+ `getPluginSDK`/`ensurePluginSDK` 全局注入点读取），插件侧只依赖该包 + 声明式 `manifest.json`（`../plugins/spark-example`）；壳层编译期注册（plugin-loader/plugin-view-registry）已退役，插件装载统一走 iframe 沙箱桥握手；插件测试已迁至 `../plugins/spark-example/tests/`
- iframe 沙箱运行时（阶段 A 第三波收尾） | 已落地：`plugin://` 源服务（`src-tauri/src/plugin_src.rs`，安装包 .spkg 优先、内置 dist 兜底；安装包定位只信市场状态 packagePath，fail-closed；CSP + 路径穿越防护；Windows 经 `http://plugin.localhost` workaround）+ vite dev 中间件（`vite.config.ts`）+ 宿主组件 `src/components/plugin/PluginIframeHost.vue`（srcdoc 沙箱 iframe + 桥握手）+ 权限中间件 `src/plugin-bridge-dispatcher.ts`（grantedPermissions ∩ view 裁剪 ∩ space 三重过滤，identity:sign 使用时询问）+ 熔断 `src/plugin-watchdog.ts`/`src/plugin-disabled.ts`（心跳无响应覆盖层、崩溃环自动停用、错误经桥 `runtime-error` 上报）；旧 registry tab 路径已退役（`plugin-view-registry.ts`/`plugin-loader.ts` 删除，`plugin-sdk-browser.ts` 收敛为桥后端工厂，App.vue 插件 tab 统一 PluginIframeHost——iframe 成为唯一加载路径，spark-example（原 weibo-core）全面迁移），AppsPage 卡片级「已停用」置灰+徽标（读 plugin-disabled） | 遗留：停用状态存 localStorage `spark:plugin-disabled` 待内核实例状态接口；.spkg 解析缓存、sdk.space（见上）、message-card 端到端、桥调用限流；运行中权限回收对已开 tab 不生效（grantedPermissions 为桥建立时快照，待权限变更事件使桥失效重建）；`ctx.theme` 为握手时快照（theme-changed 事件推送接线待做，壳层主题切换不实时同步已开插件）
- 示例插件 spark-example（阶段 D） | 已落地：`../plugins/weibo-core` 更名改造为 `../plugins/spark-example`（id/domain/name 身份层全量切换；集合名 weibo_* 沿用仅为减少无谓 churn——存储键含插件域段 `doc:<domain>:<collection>:`，域已更名即**不兼容升级**，旧 weibo-core 域存量数据不迁移，0.1.0 预发布阶段显式接受），作为插件体系参考实现：发帖后 `messages.sendAppMessage` 向组织应用会话发通知（summary 声明式降级文本 + post-card 卡片引用）、message-card 视图 `PostCard.vue`（docs 只读 + 免权限验签徽标 + `triggerCardAction`「去评论」+ `requestCardHeight`）、主视图 `onCardAction` 定位展开评论区并高亮、`identity:sign` 发帖防抵赖签名随帖存储（拒绝授权降级不阻断）；vite 多入口产出 dist/views/main.js + post-card.js（主入口按 `__sparkPluginView` 分发），打包/发布链路同步更名（build:example / package:example / release-plugin-spark-example.yml） | 遗留：message-card 端到端联调（卡片渲染→回调→主视图定位）需真机走查；wiki 文档同步在阶段 E
- 未做条目 | 拖拽移动分组（现为右键菜单）、分组重命名/删除、开发者模式安装未签名应用（§6.2） | 拖拽为体验增强；开发者模式待内核验签策略

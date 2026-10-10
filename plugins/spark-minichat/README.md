# spark-minichat（最小聊天 · 验收样例）

**身份（钉死，评审 2026-10-07）**：communication §六的验收样例——「第三方最小聊天插件安装后读写同一消息数据（换前端数据原样在）」。**不上市场、不进里程碑、不作产品演进**；功能聊天需求一律演进默认内置的 spark-chat，本插件只承担「可替换」对照件职责（登记见 `docs/product/plugins/catalog.md`「已有可复用插件」表下注）。

## 最小数据面（全部就这四个调用）

| SDK 调用 | 用途 |
|---|---|
| `sdk.messages.conversations()` | 会话列表（`refreshConversations`） |
| `sdk.messages.list(convId)` | 读会话消息（`openConversation` / 发送后收敛） |
| `sdk.messages.send(convId, text)` | 发文本消息（`sendText`） |
| `sdk.messages.onNewMessage(handler)` | 订阅新消息（`subscribeNewMessages`） |

无任何插件侧持久化：`conversations` / `activeMessages` / `activeConvId` 均为内存 ref，全部「写后重读内核收敛」——数据所有权完全在内核，这正是本插件要证明的命题。

## 验收测试

`code/app/src/tests/renderer/builtin-chat-data-sharing.test.ts`：双桥双向断言（minichat 发的消息 spark-chat 水合可见，反之亦然，同一 sled 数据）。

## 刻意省略项（不要照抄；正式聊天插件必须做）

| 省略项 | 后果 | 正式插件的正确做法 |
|---|---|---|
| 打开会话不调 `markConversationRead` | 会话未读角标在 minichat 里永不收敛 | 打开会话即 `sdk.messages.markConversationRead(convId)`（spark-chat `store.ts openConversation`） |
| 不订阅 `sdk.messages.onStatus` | 撤回 / 已读 / 投递失败的状态流转不实时反映，需手动重进会话 | 订阅 onStatus 就地合并状态（spark-chat `store.ts onChatStatus`） |
| 无失败重发 / 分页 | 发送失败仅一次性提示；长会话全量拉取 | 失败置 `failed` 终态可重发；list 分页 |
| 无样式框架（原生 CSS 单文件） | — | 刻意为之，保持 bundle 最小（~136 KB） |

发送失败处理是本样例唯一的「规范示范」：`sendText` 捕获异常置 `sendError` 提示（SDK 调用拒绝不得静默冒泡为未处理 Promise 异常）。

## 形态

iframe 桥插件（`index.ts` 经 `connectPluginBridge` 握手自挂载）；manifest 仅声明 `messages:read` / `messages:write`，`supportedSpaces: [personal, org]`；非内置、不预装，走第三方安装路径。

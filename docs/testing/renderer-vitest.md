# renderer 单测（vitest）

> 本文档从 [testing.md](testing.md) 拆分出来，独立覆盖前端 vitest 测试。

```bash
cd code/app && npm run test:unit   # 测试全绿，文件与用例数以仓库实测为准
```

覆盖（vitest 同时收集 `src/**`、`../plugins/**`、`../packages/**`）：

- invoke 适配层（`src/tests/renderer/plugin-sdk-browser.test.ts`：域解析、权限透传、错误形状）；
- 插件桥与沙箱：`packages/plugin-sdk/tests/bridge.test.ts`（桥协议握手/调用/事件/心跳编解码与版本协商）、`src/tests/renderer/plugin-bridge-dispatcher.test.ts`（三重过滤、view 裁剪、messages 域身份注入、identity:sign 使用时询问）、`plugin-watchdog.test.ts`（心跳超时、崩溃环自动停用）、`plugin-iframe-host.test.ts`（srcdoc 宿主与握手）、`plugin-card-actions.test.ts`（卡片 action 归属校验与路由）；
- 市场探索（`src/components/apps/apps-explore.test.ts`：收录/探索分区、随机洗牌与换一批、搜索稳定序、verified 增量并入）；
- spark-example 插件业务（`plugins/spark-example/tests/example.model.test.ts` / `example.service.test.ts`：仅主管理员发帖、260 字上限、评论回复结构、orgId 维度同步回归、发帖通知 sendAppMessage 摘要）；
- 消息/通讯录 mock 接入层（会话与消息水合、发送/重发/撤回状态机、标签分组树）与拼音首字母等纯逻辑。

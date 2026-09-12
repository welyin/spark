# p2p-peer-exchange

## 7. 直连协议 `/spark/peer-exchange/1.0.0`（peer-exchange.ts）

- 常量 `DIRECT_PEER_EXCHANGE_PROTOCOL`（constants.ts:55）
- 请求（peer-exchange.ts:88）：`{"type":"peer-exchange-request","want":<int>}`
  - want 缺省/非法 → 16；上限 16（`PEER_EXCHANGE_MAX`，constants.ts:60）
- 响应：`{"ok":bool,"type":"peer-exchange-response","peers":[{"peerId":string,"addresses":string[],"lastSeenAt":ms}], "reason"?:string}`
  - 非 `peer-exchange-request` 或限流时 `ok:false`（限流附 `reason:"rate-limited"`）
- 响应侧规则（:30-64、139-160）：
  - 读请求超时 3000ms
  - 同一请求方服务间隔 ≥ 60s（`PEER_EXCHANGE_MIN_INTERVAL_MS`，constants.ts:70）
  - 抽样：排除请求方、排除 `lastSeenAt` 早于 14 天（`PEER_EXCHANGE_MAX_AGE_MS`，constants.ts:65）的条目；
    verified 优先、其余按 lastSeenAt 降序；取前 want 条
- 请求侧规则（:70-118）：仅向**已连接**邻居发起；读响应超时 4000ms；
  每条样本取 ≤16 条处理，跳过自 peerId 与应答方 peerId，地址过滤空串后截 20 条；
  一律 `remember(..., 'exchange', verified=false)` 入池（未验证线索）
- 发起节奏：keepalive 每 tick 轮选一个已连接邻居交换一次（游标轮转，p2p-node.ts:356-370）

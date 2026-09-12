# plugin-announce (广播索引)

> 已从 [plugin-dist](plugin-dist.md) 拆分。
> 设计依据：`插件体系·分发与信任`（wiki architecture/plugins/plugin_system.md）「市场 = 广播索引」。
> 字节级权威：声明消息结构、规范序列化、PoW、TTL、relay 资历制、逐 peer 限流、本地索引与懒惰核查。

## 8.1 gossipsub topic

topic 固定为 `/spark/plugin-announce/1.0.0`，发布与订阅均走此 topic。

节点启动时订阅该 topic，入站消息按 topic hash 匹配路由到校验链（§8.3–§8.7）。gossipsub 配置：`flood_publish(true)`、`validate_messages()` + `Strict` 验证模式（必须显式上报 Accept/Ignore/Reject）。

## 8.2 消息结构（紧凑 JSON）

- 序列化格式：固定键序紧凑 JSON，**非 protobuf**。
- 总大小 ≤ **48 KiB**（`PLUGIN_ANNOUNCE_MAX_BYTES`），超限即拒。

```typescript
{
  "type": "spark-plugin-announce",   // 固定值
  "id": "github.com/owner/repo",     // 插件 id（§1.1 语法，host 白名单）
  "name": "插件名",                    // 1–64 字符
  "icon": "",                         // 空 | "https://" ≤512 | "data:" base64 ≤28672（约 20KB 二进制）
  "summary": "简介",                   // 1–256 字符
  "category": "business",             // 须与 spark-plugin.json 一致：ai-assistant | social | tool | game | foundation
  "version": "0.2.0",                // semver 三段 x.y.z，可带-预发布/+build，≤32 字符
  "releaseUrl": "https://...",       // 空 | "https://" ≤512
  "timestamp": 1720000000000,        // 发布时刻 (ms)
  "ttl": 2592000000,                 // TTL (ms) = 30 天，必须等于 PLUGIN_ANNOUNCE_TTL_MS
  "publisher": "<64位小写hex>",       // sha256(pubKey) 的 hex
  "pubKey": "<base64>",              // base64(Ed25519 验签公钥，32 字节)
  "pow": { "bits": 20, "nonce": 0 }, // PoW 难度与计数
  "signature": "<base64>"            // base64(Ed25519 签名，64 字节)
}
```

字段校验规则：
- `name`: 1–64 字符；
- `summary`: 1–256 字符；
- `category`: 须与 `spark-plugin.json` 的 `category` 一致（`"ai-assistant" | "social" | "tool" | "game" | "foundation"`，其余值按 `"tool"` 展示）；

> 广播索引自身不定义新的 category 枚举——直接透传声明文件的 `category` 值。
- `version`: semver 三段，≤32 字符；
- `icon`: 空、`https://` URL ≤512 字符、`data:` ≤28672 字符；
- `releaseUrl`: 空或 `https://` ≤512 字符；
- `id`: 必须通过 `announce_id_valid`（host 白名单 github.com/gitlab.com/gitee.com，3–11 段，段字符 `[a-z0-9._-]`，总长 ≤256，不含大写/scheme/.git 尾缀）；
- `publisher`: 64 位小写 hex；
- `timestamp > 0`；
- `ttl` 必须严格等于 `PLUGIN_ANNOUNCE_TTL_MS`（30 天）；
- `pow.nonce < 2^63`。

## 8.3 规范载荷与签名（字节级规范）

规范载荷由以下字段按固定键序紧凑序列化为 JSON（**不含 `pow` 和 `signature`**），用于 PoW 挖矿和签名验签：

```json
{"type":"spark-plugin-announce","id":"github.com/acme/todo","name":"待办","icon":"","summary":"测试插件","category":"business","version":"0.2.0","releaseUrl":"https://example.com","timestamp":1720000000000,"ttl":2592000000,"publisher":"<64hex>","pubKey":"<base64>"}
```

建钥定序：
1. `type` 始终排首；
2. `id`、`name`、`icon`、`summary`、`category`、`version`、`releaseUrl`；
3. `timestamp`、`ttl`、`publisher`、`pubKey`。

**签名算法**：Ed25519（RFC 8032）。签名输入 = `规范载荷` 的 UTF-8 字节。

**签名验证**：
1. `pubKey` base64 解码 → 32 字节验签公钥；
2. `sha256(pubKey)` 的低 64 位 hex → 必须等于 `publisher` 字段（公钥绑定）；
3. Ed25519 验签：规范载荷 UTF-8 字节，`signature` base64 解码 → 64 字节。

## 8.4 PoW（阻止垃圾广播）

基于 **hashcash**（SHA-256）：

- **输入**：`规范载荷字符串` + `nonce 的十进制 ASCII`（无前导零）；
- **要求**：`sha256(输入)` 的前 `bits` 位必须为 0；
- **默认难度**：`bits = 20`（`PLUGIN_ANNOUNCE_MIN_POW_BITS`），可通过 `P2pConfig.plugin_announce_pow_bits` 覆盖；
- **挖矿**：`nonce` 从 0 起递增至满足条件；
- **校验**：`pow.bits >= min_bits` 且实际前导零 ≥ `pow.bits`。

校验链中的位置：结构校验 → 逐 peer 限流 → TTL/新鲜度 → **PoW 校验** → 签名（PoW 失败报 Reject，扣传播源分）。

## 8.5 TTL 与新鲜度

- **TTL 固定值**：**30 天**（`PLUGIN_ANNOUNCE_TTL_MS = 2,592,000,000 ms`），本版不可变。
- **过期判定**：`now_ms - announce.timestamp > ttl` → Stale（拒收并报 Reject）。
- **远未来防篡改**：`announce.timestamp - now_ms > 10 min`（`PLUGIN_ANNOUNCE_MAX_FUTURE_MS`）→ Stale。
- **`ttl` 字段校验**：在结构校验阶段即检查 `ttl` 是否严格等于 `PLUGIN_ANNOUNCE_TTL_MS`，不等直接 Reject。
- 声明到期后需重新广播续期（新的 timestamp + 新的 PoW + 新的签名）。

## 8.6 relay 资历制（反女巫抑制）

防止女巫节点大量注入伪造消息并扩散：传播源节点必须与本节点保持**连续连接达到一定时长**，才有资格**转发**（relay）该消息；资历不足的节点**只收不转**。

| 参数 | 值 |
| --- | --- |
| 资历阈值 | **72 小时**（`PLUGIN_ANNOUNCE_RELAY_TENURE_MS = 259,200,000 ms`） |
| 判定逻辑 | `now - connected_since >= threshold` → Accept（转发）；否则 → Ignore（只收不转） |
| 连接跟踪 | `HashMap<PeerId, i64>` 记录每 peer 连接建立时刻，断连清零重计 |
| 首次见 peer | 资历 = 0（取当前时间作 `connected_since`，不含历史） |
| 本机自发消息 | 不经此路径（发布侧直接 `publish_raw` 广播） |

gossipsub 上报策略：
- 校验失败的限流 → Ignore（不扣分）；
- 其余校验失败 → Reject（扣传播源分）；
- 资历不足 → Ignore（不扣分）。

## 8.7 逐 peer 限流（滑动窗口）

单 peer 高频广播抑制：

| 参数 | 值 |
| --- | --- |
| 滑动窗口 | 1 小时（3,600,000 ms） |
| 每窗口上限 | **10 条/小时**（`PLUGIN_ANNOUNCE_RATE_LIMIT_PER_HOUR`） |
| 跟踪 peer 上限 | **1024**（`PLUGIN_ANNOUNCE_RATE_LIMIT_TRACKED_PEERS`） |
| 回收策略 | 跟踪 peer 满时先清过期条目，仍满则整体清空（兜底不误伤新 peer） |

限流失败报 `RateLimited`，gossipsub 上报为 Ignore（不扣分）。

## 8.8 本地索引存储

- **存储后端**：sled（内核层持久化）。
- **键格式**：`mkt:ann:<id>`，如 `mkt:ann:github.com/acme/todo`。
- **值线形**：JSON 序列化的 `PluginAnnounceIndexEntry`：

```typescript
{
  announce: PluginAnnounce,           // 原始消息
  firstSeenAt: 1720000000000,        // 首次见到时间 (ms)
  updatedAt: 1720000000000,          // 最后更新时间 (ms)
  verified: "Verified",              // "Pending" | "Verified" | "Failed"
  verifyError: "",                    // 核查失败原因（Failed 时）
  verifiedAt: 0,                      // 核查完成时间 (ms)
  corrected: {                        // 仓库声明文件校正后的展示字段（Verified 时）
    name: "...",
    icon: "...",
    summary: "...",
    version: "...",
    supportedSpaces: ["personal", "org"]
  }
}
```

**容量控制**：
- **上限**：**10,000 条**（`PLUGIN_MARKET_INDEX_MAX`）。
- **逐出策略**：超限时全量扫描，按 `updatedAt` 最旧逐出；同时惰性清除已过期条目。
- **单 id 策略**：同一 id 只保留 `timestamp` 最新的一条；旧 timestamp 返回 `Stale` 不入库；新 timestamp 替换旧条目并将 `verified` 重置为 `Pending`（重新触发核查）。

## 8.9 懒惰核查

**不在广播接收端实时校验仓库声明，而是后台异步逐条核查，避免中心化依赖与请求尖峰。**

### 触发时机

| 时机 | 行为 |
| --- | --- |
| 新声明入索引 | 内核发出 `P2pEvent::PluginAnnounceReceived` → 壳层 worker 入队核查 |
| 事件通道 Lagged（广播溢出丢事件） | 全量补扫所有未核查条目 |
| 启动时 | 存量未核查 + 旧 verified 条目迁移集（`supportedSpaces` 补齐）入队 |
| 定时重扫 | 每小时对 failed/unreachable 条目低频重扫 |

### 核查方式

- 调用市场服务的 `market.resolve_repo_plugin(&id)` → 执行 **仓库锚定验证**（[plugin-dist](plugin-dist.md) §4.1）：
  - 取声明文件（多源交叉 + id 一致性校验）；
  - 校验 `normalize(id) == normalize(declaration.id)`。
- 通过时从仓库声明文件取回 `name`/`icon`/`summary`/`version`/`supportedSpaces`，回写为 `corrected` 展示字段（以仓库为准，覆盖广播中的自声明字段）。
- 失败原因归类：
  - `"fetch failed"` → `"unreachable"`；
  - `"id mismatch"` → `"id-mismatch"`；
  - 其他 → 截断至 200 字符。

### 核查参数

| 参数 | 值 |
| --- | --- |
| 核查间隔 | 每条 2 秒（`VERIFY_INTERVAL`），避免请求尖峰 |
| 终态回写 | 绑定核查时 `expected_timestamp`；核查期间同 id 新声明到达（timestamp 变）→ 旧结论作废 |

### 终态回写

- `mark_plugin_announce_verified` 绑定核查时读到的 `expected_timestamp`；
- 核查期间同 id 新声明到达（timestamp 不匹配）→ 旧结论作废丢弃；
- 通过时标 `Verified`（进入市场视图）并回写 `corrected`；
- 失败标 `Failed` 并记原因；
- 发出 `P2pEvent::PluginAnnounceVerified` 事件通知壳层更新 UI。

## 8.10 接收侧校验链（完整流程）

```
入站消息 (gossipsub topic "/spark/plugin-announce/1.0.0")
    │
    ▼
① 结构校验 (parse_structure)
   ├─ 总大小 ≤ 48 KiB
   ├─ JSON 解析
   ├─ type == "spark-plugin-announce"
   ├─ ttl == PLUGIN_ANNOUNCE_TTL_MS（30 天）
   ├─ timestamp > 0
   ├─ pow.bits >= min_pow_bits（默认 20）
   ├─ pow.nonce < 2^63
   ├─ publisher: 64 位小写 hex
   ├─ fields_valid（name/summary/category/version/icon/releaseUrl/id）
   └─ 失败 → Reject（扣分）
    │
    ▼
② 逐 peer 限流 (SlidingWindowLimiter)
   ├─ 窗口 1h / 上限 10 条 / 跟踪 1024 peer
   └─ 失败 → Ignore（不扣分）
    │
    ▼
③ TTL / 新鲜度
   ├─ 已过期（now - timestamp > ttl）→ Stale → Reject
   └─ 远未来（timestamp - now > 10min）→ Stale → Reject
    │
    ▼
④ PoW 校验
   ├─ sha256(规范载荷 || decimal(nonce)) 前导零 ≥ pow.bits
   └─ 失败 → Reject
    │
    ▼
⑤ 签名校验
   ├─ pubKey base64 解码 → 32 字节
   ├─ sha256(pubKey) == publisher（绑定校验）
   ├─ Ed25519 验签（规范载荷, signature）
   └─ 失败 → Reject
    │
    ▼
⑥ 入本地索引 (upsert: 单 id 最新 + LRU 10K)
    │
    ▼
⑦ relay 资历制门控
   ├─ 传播源连续连接 ≥ 72h → Accept（转发给其他 peer）
   └─ 不足 → Ignore（只收不转）
    │
    ▼
⑧ 懒惰核查（异步，壳层 worker）
   └─ 后台解析仓库声明文件 → Verified/Failed → 回写索引
      只有 Verified 条目进入市场「探索」视图
```

## 8.11 发布流程（开发者侧）

1. 开发者在市场页「发布声明（开发者）」入口解析 `spark-plugin.json` 预填；
2. 确认后生成规范载荷；
3. **PoW 挖矿**：`nonce` 从 0 递增，直到 `sha256(规范载荷 || nonce十进制)` 前 20 bit 为 0；
4. 用开发者 Ed25519 私钥对规范载荷签名；
5. 组装完整消息（含 pow 和 signature），走 gossipsub `publish_raw` 广播至 `/spark/plugin-announce/1.0.0`；
6. 声明到期（30 天）需重新广播续期（新 timestamp + 新 PoW + 新签名）。

## 8.12 消费侧（市场视图）

- 市场「探索」页展示 `verified == Verified` 的所有条目；
- 展示字段优先取 `corrected`（仓库声明文件校正值），仓库不可达时退用 `announce` 原值；
- 消费者对每条索引可执行 `installFromRepo`（[plugin-dist](plugin-dist.md) §4.2），索引层不得绕过仓库锚定验证。

---

> 实现位置：`code/core/src/p2p/plugin_announce.rs`（消息结构/校验链/索引）、`code/core/src/p2p/constants.rs`（常量）、`code/core/src/p2p/node/gossip.rs`（relay 资历制）、`code/core/src/kernel/plugin_announce_ops.rs`（内核门面）、`code/app/src-tauri/src/announce_verify.rs`（懒惰核查 worker）。

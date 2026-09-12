# affair-metadata（议题元数据面）

> 状态：**C10 已落地（gossip 元数据面已上线）**。依据：`总体方案`（wiki architecture/community-affairs.md） §6 方案 A（元数据面 gossip + indexer 目录）、§7.3 indexer 定位（决策点 4）；产品语义：`公共议题平台`（wiki product/public-topics.md） 第三/四节。
> 通用编码约定见 `community 总约`（wiki protocol/community/README.md）。事务容器见 [affair](affair.md)。

## 1. 定位与边界

元数据面 = 议题的**发现层**：标题、简介、标签——**全网搜索只索引这三样**（产品级承诺），讨论内容、文件内容不进公共索引。indexer 订阅元数据面即全量副本，同时是公共议题的天然长期副本持有者。

协议只约定两件事（总体方案 §7.3）：**元数据面复制**与**收录查询所需字段**。匹配/聚合函数（模糊匹配、健康信号、履历聚合）为内核确定性计算（任何节点对同一查询返回同一结果、可全网复算）；推荐与呈现留客户端/插件层竞争，**协议零约定**（原则 3）。indexer 查询协议报文见 §7（C10 已落地：`/spark/affairmeta/1.0.0` + `core/src/index/`）。

## 2. gossip 主题

| 主题 | 用途 |
| --- | --- |
| `spark-affair-meta` | 议题元数据公告（本文）；全网公共主题，任何节点可订阅 |

- 新主题与既有 `spark-sync`/`spark-overlay`（[p2p-netstack](../foundation/p2p/p2p-netstack.md) §2）并列；节点启动时订阅与否由角色决定（轻客户端可不订阅，只向 indexer 查询）；
- 信封沿用 pubsub 信封规则（[p2p-envelope](../foundation/p2p/p2p-envelope.md) §3）：`type: 'affair-meta'`、`domain: 'affair'`、`id: <affairId>`、`payload` = §3 公告；**不强制签名**（与 org-share 同档——§4 的可验证性由 payload 内嵌的修订链指针承担，信封临时钥签名只提供完整性/反垃圾门槛，p2p-envelope §3.3 自证式口径）；
- 入站分流：topic == `spark-affair-meta` 且 type == 'affair-meta' → 元数据公告处理器；**不落业务库**（与 update/delete 路径隔离），只进元数据暂存区（§5）。

## 3. 元数据公告线形（payload）

```json
{
  "metaV": 1,
  "affairId": "<64hex>",
  "title": "第二届业委会选举",
  "summary": "……",
  "tags": ["region:110105", "hoa"],
  "region": "110105",
  "metaSeq": 2,
  "basisOpHash": "<64hex>",
  "contents": [ { "kind": "git", "ref": "<仓库地址>" } ],
  "updatedAt": 1720000000000
}
```

| 字段 | 约束 |
| --- | --- |
| `metaV` | 恒 1 |
| `affairId` | 必须等于信封 `id` |
| `title`/`summary`/`tags` | 同 [affair](affair.md) §2.1 元数据约束；公告内容是「当前生效元数据」的全量快照（非 diff） |
| `region` | 可省/可空；规范化区域代码（自 tags 中 `region:` 前缀项提取的冗余槽位，索引便利；真相以 tags 为准） |
| `metaSeq` | 元数据代际：创世 = 0，每次 meta-revise 生效 +1（[affair](affair.md) §11.2） |
| `basisOpHash` | **可验证性锚**：本代际元数据的生效依据——metaSeq=0 时为 affairId（创世哈希），metaSeq>0 时为生效的 meta-revise 操作 opHash；收录方可对事务日志复算修订链合法性（延迟否决生效，留痕可验） |
| `contents` | 可省；内容面描述性指针数组 `{ "kind": "git"\|"blob", "ref": string, "hint"?: string }`——**指针只做导航**，内容寻址与做种归内容面（Kad provider 记录，持有即做种，移动端叶子模式除外；产品第七节） |
| `updatedAt` | 发布方毫秒（LWW 裁决键，§5） |

## 4. 发布与复制约定

- **发布者**：事务关注者节点均可转发公告（ gossip 洪泛天然多源）；修订生效（公示期过、无阈值异议）后由主持人/任一副本发起新代际公告；
- **indexer 收录**：订阅 `spark-affair-meta` 即元数据全量副本；收录条目按 `(affairId)` 键控；
- **验证分层**（同步面不验签先例同族）：
  - 持 `basisOpHash` 且本地有事务日志副本 → 复算修订链，验证通过标 `verified`；
  - 无日志副本（纯 indexer 元数据面）→ 暂存标注 `unverified`，按 §5 裁决留存，**不得**以未验证公告覆盖已验证条目；
  - 公告与日志复算矛盾（title/tags 与生效代际不符）→ 丢弃并告警（可举证：公告 + 日志片段并排即证据）；
- 元数据修订的协议真相永远在**事务日志内**（meta-revise 操作链）；gossip 公告只是发现层的投影——索引收录以日志复算为准，公告加速发现。

## 5. 暂存区裁决与卫生

- 存储键：`affmeta:seen:{affairId}`（暂存公告 + 验证状态），本地键不进同步；
- 裁决：同 affairId 多公告 → `(metaSeq, updatedAt)` 字典序大者胜（**metaSeq 优先于 updatedAt**——修订代数是协议语义，updatedAt 只是打破平手）；verified 条目不被 unverified 覆盖；
- 体积卫生：单条公告序列化 ≤ 4 KB（title/summary/tags 约束已保证）；暂存区条数上限 100k，最旧未验证条目先淘汰（verified 不淘汰——验证劳动不白费），淘汰连带删除该公告的 `affmeta:idx:` 索引条目（索引是查询的事实来源，不留无暂存对应的残留索引）；
- 反垃圾：无发起门槛（产品「垃圾防线」：关注才复制，垃圾事务天然不扩散；indexer/公共目录自行过滤）；gossip 层沿用 floodPublish 与按连接既有节制，不新增协议机制。
- 实现（C10）：暂存区/裁决/体积卫生在 `core/src/index/staging.rs`，本地索引 `affmeta:idx:` 在 `core/src/index/local_index.rs`，收录编排（§4 复算 + §5 裁决 + 索引更新）在 `core/src/index/query.rs::ingest_announcement`——gossip 入站与内核自发布共用同一入口。

## 6. 组织公开名片（公共目录的另一收录类）

组织可发布签名的公开名片进入公共目录（产品第四节，字段逐个可选、粒度自决）——名片线形复用 [org-address](../community/org/org-address.md) §16 组织地址记录（`displayName`/gateways 已含），扩展可选段归组织名片插件（C11）的产品文档定义；本面只做一件事：**`spark-affair-meta` 主题同时承载 `type: 'org-card'` 信封**（domain `'affair'`、id = orgAddress），payload = 地址记录全文（§16 校验链五步在收录点执行——该记录本就自认证签名，无需新机制）。

- 实现（C11 收录面）：gossip 入站 `core/src/p2p/node/gossip.rs::handle_inbound_org_card`——信封 id 须等于记录 orgAddress → §16.3 五步校验链 → seq/publishedAt 冲突裁决后沉淀 `p2p:org-address:` 本地缓存（与 spark-overlay `org-address` 入站同径同库）；查询路径 = kernel `resolve_org_address` / `search_known_orgs`（org.md §16.4，读同一缓存），收录即接通。

## 7. indexer 查询信封（C10）

轻客户端经 p2p 向**启用 indexer 角色的节点**（节点配置开关，community-affairs §10 决策 4）发查询；未启用角色显式回 `indexer-disabled`。传输 = request-response 协议 `/spark/affairmeta/1.0.0`（单发单收，无 fan-out；indexer 寻址经 indexer 目录，见 §7.1）。

请求帧（固定键序紧凑 JSON；≤ 4 KB）：

```json
{
  "type": "affair-meta-query",
  "queryId": "<不透明字符串，1–64>",
  "payload": {
    "kind": "search",
    "query": "业委会 选举",
    "limit": 20,
    "region": "110105",
    "tags": ["hoa"]
  }
}
```

| 字段 | 约束 |
| --- | --- |
| `queryId` | 不透明，1–64 字符；响应原样带回（请求-匹配键） |
| `payload.kind` | 恒 `"search"`（扩展位；未知 kind 回 `bad-query`） |
| `payload.query` | 匹配文本，≤256 B；空白切词小写化（CJK 整体成词），词条在 title/tags/summary 子串命中分别 +3/+2/+1，同字段同词条只计一次 |
| `payload.limit` | 缺省/非正 → 20；硬上限 50 |
| `payload.region` | 可省；精确等于过滤（条目 region 槽位） |
| `payload.tags` | 可省；条目须包含全部所给标签（≤16 个、每个 ≤32 UTF-16） |

响应帧：

```json
{
  "type": "affair-meta-result",
  "queryId": "<回带>",
  "payload": {
    "results": [ {
      "affairId", "title", "summary", "tags", "region?",
      "metaSeq", "basisOpHash", "verified", "updatedAt", "score",
      "health": { ... }?
    } ],
    "complete": true
  }
}
```

- 排序 `(score 降序, affairId 升序)`——确定性口径：同一索引状态 + 同一查询，任何节点返回逐字节一致；
- `health`（健康信号，§10 决策 4 内核确定性推导，从本地 affair 日志复算）：`activeParticipantsTrend`（近 6 个活跃窗口各窗口活跃身份数，链上时间）、`adoption`（采纳集中度：totalAdoptions/topIdentity/topSharePermille，阶梯采纳口径）、`objectionCount`/`appealedCount`（异议条数 / 被异议指向的 distinct 操作数）、`lastActivityMs`（最近链上锚定活动）、`fork`（分叉谱系：headCount/maxDepthOps/heads）。响应节点无该事务日志副本时 `health` 缺席（字段不出现在结果对象里）；
- 错误响应：`payload = {"error": "indexer-disabled" | "indexer-not-covered" | "bad-query" | "rate-limited" | "unsupported"}`（reason 稳定字符串，客户端可依赖）；`indexer-not-covered` = 角色启用但查询超出其宣告的子集覆盖（§7.1），客户端据此换目录内其他 indexer 重查；
- 应答侧逐请求方限流（同一 peerId 最小间隔 5s）；请求帧/响应帧体积 ≤ 4 KB——响应帧超限时从结果尾部按序截断条数并将 `complete` 置 `false`，直至 ≤ 4 KB（截断确定：排序确定 → 同输入同输出，任何节点同样截断）。

**本地直查（loopback）**：内核门面 `Kernel::indexer_search(request_json)` 与 p2p 应答共用同一解析/分发函数（`core/src/index/query.rs`），轻客户端可不经网络在本地角色节点直查，结果与 p2p 路径天然一致。loopback 不做覆盖门控（本地用户查本机缓存不受角色服务范围约束）。

## 7.1 indexer 目录与子集覆盖（indexer-card）

indexer 节点**自公告名片**让轻客户端发现可用 indexer：`spark-affair-meta` 主题承载第三类信封 `type: 'indexer-card'`（domain `'affair'`、id = 节点 peerId），payload 线形：

```json
{
  "indexerV": 1,
  "peerId": "<libp2p peerId>",
  "regions": ["110105"],
  "topics": ["hoa"],
  "updatedAt": 1720000000000,
  "signature": "<base64>"
}
```

- **签名**：payload 以节点 **libp2p 私钥**签名（待签名载荷 = 去 `signature` 的固定键序紧凑 JSON，`indexerV → peerId → regions → topics → updatedAt`），验签公钥从 peerId 内嵌提取（node-announce 同款自证口径：证明「该 peerId 持有者宣告了这份覆盖」）；`regions`/`topics` 恒在（空即空数组），各维 ≤16 条、每条 1–32 UTF-16，payload 序列化 ≤ 4 KB；
- **覆盖语义**：`regions`/`topics` 双维度子集覆盖声明，空列表 = 该维度不限（两维皆空 = 全覆盖，与纯启用等价）。覆盖三处生效：①gossip 收录过滤——角色启用且覆盖非全量时，只收录覆盖子集内的元数据公告（索引内容与名片宣告一致）；②查询应答门控——受限维度要求查询带对应过滤且落覆盖内（regions 受限则 `region` 过滤必填且命中；topics 受限则 `tags` 非空且全部命中），否则回 `indexer-not-covered`；③名片宣告内容；
- **目录簿记**：收录键 `affmeta:dir:{peerId}`（本地键不进同步）；同 peerId 按 `updatedAt` 大者胜（发布方时钟只做新旧裁决），新鲜度按本地收录时刻（`lastSeenAt`）判 TTL = 15 min（自公告随 keepalive tick 以 node-announce 同节奏 5 min 重发）；容量上限 1024，超限按 `lastSeenAt` 最旧逐出。目录条目是**线索而非信任根**——查询结果确定性/可复算性不变，选错 indexer 的代价只是换一家重查；
- **选取**：按查询过滤条件做覆盖匹配后 peerId 升序取首（确定性：同目录状态任何节点选同一家）；
- **发现/查询路径**：门面 `Kernel::indexer_publish_card`（启用后即时宣告）/ `indexer_directory`（读本地目录）/ `indexer_query(peerId, request)`（未连接先按覆盖网邻居池地址直连，再走 `/spark/affairmeta/1.0.0`）；leaf 模式不发布名片（叶子不服务）。
- 实现：线形/覆盖/目录簿记 `core/src/index/directory.rs`；gossip 出入站 `core/src/p2p/node/gossip.rs`（`publish_indexer_card_now` / `handle_inbound_indexer_card`）+ tick 周期发布 `core/src/p2p/node/tick.rs`；应答门控 `core/src/kernel/host/mod.rs::handle_affair_meta_query`；壳层命令 `plugin_affairs_set_indexer_enabled` / `plugin_affairs_set_indexer_coverage` / `plugin_affairs_indexer_config` / `plugin_affairs_indexer_directory` / `plugin_affairs_indexer_query`（`app/src-tauri/src/commands/affairs.rs`）。

## 8. 验收向量（登记：`code/spec/vectors/community.json`）

| case 组 | 内容 | 状态 |
| --- | --- | --- |
| `metaAnnounce` | 固定公告 → canonical 逐字节 + 信封字段形态（type/domain/id 固定值） | 已产出（生成器自产） |
| `metaArbitrate` | (metaSeq, updatedAt) 裁决矩阵 + verified 覆盖守卫用例 | 已产出（C1 生成器自产回填；裁决纯逻辑 `core/src/affair/meta.rs`；暂存区/淘汰 C10 已落地 `core/src/index/staging.rs`） |
| `metaBasisVerify` | 公告 vs 日志修订链复算（verified/unverified/矛盾丢弃三态） | 已产出（C1 生成器自产回填；复算纯逻辑 `core/src/affair/meta.rs`；indexer 收录/查询 C10 已落地 `core/src/index/`） |

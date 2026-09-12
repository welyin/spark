# affair-sync（事务复制面）

> 状态：**C4 已落地（core/sync/affairsync + core/p2p 元数据面）**——出入站调度已接线（code/core/src/kernel/org_sync/mod.rs 出站触发）。依据：`总体方案`（wiki architecture/community-affairs.md） §6 方案 A1；事务容器见 [affair](affair.md)；元数据面见 [affair-metadata](affair-metadata.md)。
> 通用编码约定见 `community 总约`（wiki protocol/community/README.md）；命名红线同 affair.md（实现层一律 `affair`，与 `org:tx:` 严格分域）。

## 1. 定位与总体形态

事务**没有托管域**（产品第七节），由关注者复制保存：复制组 = **动态关注者集合**（A1）——对 orgsync 复制组机制的 scope 泛化，复用 hello/need/data 三信封反熵 + 逐条验签。

与 orgsync 的根本差异：**没有成员表/静态角色名册**。orgsync 的验签规则「from ∈ org:meta 成员表 + from ∈ 该集合复制组」泛化为「from ∈ 关注者」——本机只对本机已关注（`affair:follow:`）的事务参与反熵；数据面安全边界不靠名册，靠**逐条操作自带的签名链**（`verify_op` 入站校验链 + 创世 `verify_genesis` 全链）：伪造/篡改的记录逐条被拒，攻陷的「关注者」无法注入无效操作（对齐总体方案「非装机持有者可验证签名链与完整性」）。

affair 域**纯 append-only**（操作与引用不可撤销，affair.md §3/§10）：**无墓碑、无删除日志面**——orgsync 的 dlog/dseq/tombstone 机械层不随 scope 泛化带入（org 域 dlog 键域归 orgsync 独有）。

## 2. dm 信封（kind）

三信封走 dm 直连 `/spark/dm/1.0.0`（p2p-dm §19），dm kind 与 orgsync 同族命名：

| kind | 方向语义 |
| --- | --- |
| `affairsync-hello` | 摘要交换：本事务折叠 vv + DAG 头集合 |
| `affairsync-need` | diff 请求：本事务 knownVv |
| `affairsync-data` | 数据传输：创世/操作记录分批（key/value/meta） |

限流豁免同 orgsync 反熵族（hello→need→多批 data 背靠背往返，豁免清单见 p2p-dm §19）。

## 3. 线形

> 下列为信封 body（载荷）线形；dm 包装（顶层 `kind` 字段取 §2 的 kind 字符串）由发送方按 p2p-dm §19 封装，与 orgsync 同口径（build/parse 只针对 body）。

### 3.1 `affairsync-hello`

```json
{
  "affairId": "<64hex>",
  "vv": { "<nodeId>": 1 },
  "heads": ["<opHash>", "…"],
  "deviceClass": "pc"
}
```

| 字段 | 约束 |
| --- | --- |
| `affairId` | 目标事务 id（`^[0-9a-f]{64}$`） |
| `vv` | 折叠版本向量：`affair:rec:{affairId}` 与 `affair:op:{affairId}:*` 全部记录的 pmeta 逐条 merge 取 max（口径同 orgsync collect） |
| `heads` | 本地观察到的 DAG 头集合（`affair:head:` 内容，affair.md §3.3）；可空数组 |
| `deviceClass` | 缺省 `"pc"`（与 pdsync/orgsync 同字段口径） |

### 3.2 `affairsync-need`

```json
{ "affairId": "<64hex>", "knownVv": { "<nodeId>": 1 } }
```

### 3.3 `affairsync-data`

```json
{
  "affairId": "<64hex>",
  "records": [
    { "key": "affair:rec:<64hex>", "value": { "…": "创世记录" }, "meta": { "vv": {}, "ts": 0 } }
  ],
  "batchSeq": 0,
  "batchTotal": 1
}
```

- 记录线形同 orgsync-data（key/value/meta，`meta` 为 DocMeta 持久化形态）；无 `dseq`（无墓碑面，§1）；
- **key 白名单红线**：每条记录 key 必须等于 `affair:rec:{affairId}` 或以 `affair:op:{affairId}:` 为前缀，**不符整批拒收**（B3 同族红线：防被攻陷/故障的关注者覆写任意 sled 键）；`org:` 键恒拒收；
- 分批切分按单批字节上限（与 orgsync 同口径常量）。

## 4. 入站应用（affairsync-data 合入）

对批内每条记录按类型分链校验，**任一不过逐条拒收**（不影响同批其余记录）：

- **创世记录**（key = `affair:rec:{affairId}`）：`verify_genesis` 全链——结构 → affairId 复算匹配 → refs 自指禁令 → 验签 → 规则静态检查（§5.6，创世拒绝创建）。本地已有创世则按载荷一致性幂等去重（不一致拒收）。
- **操作记录**（key = `affair:op:{affairId}:{opHash}`）：`verify_op` 入站校验链（affair.md §3.2：结构 → affairId 匹配 → actor 绑定 → 验签 → payload 结构；**复制入站豁免 declaredAt 新鲜度**，见 affair.md §3.1——历史补齐不被 ±10min 窗口锁死）→ opHash 复算匹配键尾 → §11 主持人门槛（moderate/meta-revise 仅创世 initiator）→ **因果见证**：prevOpHash 与 vote/objection 指向必须是 affairId 或本地已知条目/暂存条目，**未知则持久暂存待补，不因乱序拒收**（affair.md §3.2）。
- **暂存与 drain**：暂存条目落 `affairsync:pend:{affairId}:{opHash}`；每批合入后重扫暂存区做 drain 循环，指向补齐即入有效集（语义同 C1 `OpLog` 的 pending/drain，持久化归本面；drain 复检同样豁免 declaredAt 新鲜度）。
- **时间口径**：复制保真靠签名 + opHash 链；一切时间语义由各副本本地存证锚定时刻承担（affair.md §7.2），对端 declaredAt 与 meta 时间戳不作任何判定依据。
- **落库**：接受的记录写本体 + pmeta（远端 meta 原样落盘）+ **逐条入本机存证链**（affair.md §3.3：`domain = "affair:{affairId}"`、创世 `collection = "genesis"` / 操作 `collection = "ops"`、`id = affairId|opHash`、`op = 'put'`）+ DAG 头集合维护（`affair:head:`：移除被引 prev、加入新头）。
- **关注门槛**：affairsync-data 的目标事务本机未关注 → 整批拒收（关注才复制，垃圾事务天然不扩散）。

## 5. 关注簿记与同步面层键

| 键 | 值 | 说明 |
| --- | --- | --- |
| `affair:follow:{affairId}` | `{"v":1,"followedAt":<ms>}` | 本地关注状态（affair.md §3.3，本地键不进同步） |
| `affairsync:dir:{affairId}:{rootId}` | `{"rootId","lastSeenMs","peerId"?}` | 关注者目录：入站 affairsync-hello 的 from 落账（复制流量中学的覆盖网线索，§6） |
| `affairsync:pend:{affairId}:{opHash}` | 操作条目 JSON | 乱序暂存（§4），补齐 drain 后删除 |

- 键前缀分域：`affair:`（affair 模块键构造函数）与 `affairsync:`（本面层簿记）均非 `org:`/`orgd:`，orgsync-data 白名单天然拒收；反向红线（本面拒收 `org:` 键）见 §3.3。

## 6. membership 发现（最小可用）

- **indexer 目录**：indexer 订阅 `spark-affair-meta` 即元数据全量副本（affair-metadata §1/§4），关注者发现/查询协议归 C10（indexer 查询报文不在本规格）；
- **覆盖网线索（本面最小可用）**：入站 `affairsync-hello` 的 `from`（rootId + 连接层 peerId + 时刻）落关注者目录（§5）；节点对某事务发起反熵时按目录条目获得对端线索。目录条目是**线索而非信任根**：数据面安全边界始终是 §4 逐条验签链；
- **元数据公告**：`spark-affair-meta` 主题的公告线形/裁决/验证分层归 [affair-metadata](affair-metadata.md) §2–§5；p2p 层入站只负责信封校验（§3 信封规则，不强制签名）+ 类型分流，不落业务库（暂存区归 C10）。

## 7. 反熵流程

```
关注者A ──affairsync-hello(vv, heads)──▶ 关注者B
   ◀──affairsync-need(knownVv)── 本地落后/并发
   ◀──affairsync-data(分批)────── 本地领先/并发（need 的应答）
```

diff 裁决四态（口径同 orgsync）：本地落后 → 回 need；本地领先 → 推 data；并发 → need + data 双向；相等 → 不动。hello/need 入站前提：本机已关注该事务；未关注静默丢弃。

> **heads 的定位**：`affairsync-hello` 的 `heads` 是对端 DAG 头观测的**携带信息**（供对端比对与排查分叉），**不参与 diff 裁决**——裁决仅以 vv 为准（C4 实现口径，最小可用；后续如需基于头的分叉裁决再升级）。

## 8. 与 orgsync 的对照（复用与泛化）

| 面 | orgsync | affair-sync |
| --- | --- | --- |
| 复制组 | 静态：成员表角色推导（all-members / data-accounts） | 动态：关注者集合（`affair:follow:` + 目录线索） |
| 验签 | from ∈ 成员表 + from ∈ 集合复制组 | 无名册：hello/need 只对本机已关注事务应答；data 逐条 verify_op/verify_genesis 签名链 |
| 信封 | orgsync-hello/need/data（orgId + collection） | affairsync-hello/need/data（affairId 单集合） |
| vv 折叠 | 按 (orgId, collection) 键域 | 按 affairId 键域（rec + ops） |
| dlog/墓碑 | 有（org 域 dlog + tombstone） | **无**（纯 append-only） |
| 落库 | apply_remote_update（schema 策略） | affair 入站校验链 + 存证锚定（无 schema 声明面） |
| membership 发现 | 成员表 + orgq 在线目录 | indexer 目录（C10）+ 覆盖网 hello 线索 |

## 9. 验收向量（登记：`code/spec/vectors/community.json`）

| case 组 | 内容 | 状态 |
| --- | --- | --- |
| `affairSync` | 三信封 body 逐字节（固定 affairId/vv/heads/meta/batch）+ parse 回读 + key 白名单拒收用例 | 已产出（C4 `core/examples/gen_community_affair_sync_vectors.rs` 自产回填） |

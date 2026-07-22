# 组织模块规格（organization）

> 来源：反向提取自 `desktop/src/main/organization/`（invite / node-info-claim / service / sync /
> transaction-store / types）与 `desktop/src/main/p2p/`（org-share-sync / org-pull-sync /
> org-recovery / org-share-snapshot / plugin-org-sync / p2p-node）、`desktop/src/main/bootstrap.ts`。
> 网络层消息格式（信封签名、直连协议帧、node-announce、邻居池）见 `p2p-messages.md`；
> rootId/公钥派生见 `identity.md`。

## 1. 身份锚点

- `rootId = sha256hex(原始 32 字节 Ed25519 根公钥)`（64 字符小写 hex，identity/root-id.ts:292）
- 根公钥在协议载荷中的编码：**base64（原始 32 字节，非 PEM）**
- 组织内所有 rootId 字段统一 `trim().toLowerCase()` 并须匹配 `^[0-9a-f]{64}$`

## 2. 邀请码（organization/invite.ts）

### 2.1 payload 字段（invite.ts:9-20）

```json
{
  "type": "spark-org-invite",
  "version": 1,
  "orgId": "org_<16hex>",
  "orgName": "<组织名，可空串>",
  "inviter": { "rootId": "<64hex 小写>", "peerId": "<可省>", "addresses": ["<multiaddr>", ...] },
  "createdAt": 1720000000000
}
```

### 2.2 编码（invite.ts:29-41）

- `base64url(JSON.stringify(payload) 的 UTF-8)`：`+`→`-`、`/`→`_`、**去掉 `=` padding**；紧凑 JSON 无空格
- 解码时 `-`→`+`、`_`→`/`，按 `(4 - len%4) % 4` 补 `=` 后 base64 解码
- **不签名、不含密钥**——邀请码不是 capability，仅作引导线索（invite.ts:1-7）

### 2.3 解析校验（invite.ts:44-89）

按序校验，任一不符抛出中文错误：

1. base64url 可解码且为合法 JSON
2. `type === 'spark-org-invite' && version === 1`
3. `orgId` 为非空字符串（trim 后使用）
4. `inviter.rootId` 匹配 `^[0-9a-f]{64}$`（先 trim 再 lowercase 后校验）
5. `inviter.addresses` 过滤非字符串/空串；`peerId` 非空才保留；**peerId 与 addresses 至少其一**
6. 有效期：`createdAt` 为 number 且 `createdAt > 0` 且 `Date.now() - createdAt ≤ 24h`
   （`ORG_INVITE_MAX_AGE_MS = 24*60*60*1000`，invite.ts:27）。
   ⚠️ 只查"过去 24h"，**未来的 createdAt 不设上限**（无 `Math.abs`）
- 归一化返回：`orgName` 缺省为 `''`，rootId 小写，addresses 过滤后数组

## 3. 组织记录（org:meta:<orgId>）

### 3.1 OrganizationRecord 字段（organization/types.ts:38-55）

```
orgId:            string   // `org_${randomBytes(8).toString('hex')}` → "org_" + 16 hex（service.ts:88-90）
name:             string   // trim + 连续空白归一为单空格（service.ts:13-19）
description:      string
basePluginDomain?: string  // 创建时必填，须以 "plugin:" 开头（service.ts:21-32）
createdAt / updatedAt: ms
createdBy:        string   // rootId
recoverySecret?:  string   // randomBytes(32).toString('hex') → 64 hex（service.ts:124）
members:          OrganizationMember[]
sync?:            OrganizationSyncState
```

OrganizationMember（types.ts:19-25）：

```
{ rootId, role: 'admin'|'member', joinedAt: ms, addedBy: rootId,
  nodeInfo?: { peerId?: string, addresses: string[] } }
```

OrganizationSyncState（types.ts:27-36）：

```
{ versions: { summaryVersion, membersVersion, memberDetailsVersion, transactionsVersion },
  sections: Array<'summary'|'members'|'member-details'|'transactions'>,
  lastSyncedAt: ms }
```

存储：LevelDB 键 `org:meta:<orgId>`（`ORG_META_PREFIX`，organization/constants.ts:1），值 = 记录 JSON。

### 3.2 角色与约束（service.ts）

- 角色仅 `admin` / `member`；创建者自动为唯一 admin（service.ts:125-132）
- 移除 admin 时若 admin 总数 ≤ 1 拒绝（必须保留至少一名 admin，service.ts:472-477）
- 添加成员：`rootId` 规范化后查重；重复添加视为"更新 nodeInfo"（未提供 nodeInfo 时保留原值，
  service.ts:223-266）；新成员 role 固定 `member`（service.ts:268-274）
- nodeInfo 归一化：peerId trim 后 < 8 字符拒绝；peerId 与 addresses 至少其一；
  全空视为未提供（可后续经 nodeInfoClaim 回填，service.ts:46-77）
- UI 展示排序：admin 优先，其余按 joinedAt 升序（sortMembers，service.ts:79-86）

### 3.3 事务记录（transaction-store.ts，与记录分离存储）

- 键 `org:tx:<orgId>:<createdAt>:<txId>`；`txId = randomBytes(8).toString('hex')`（16 hex）
- 记录：`{ txId, orgId, type: 'create'|'member-add'|'member-update'|'member-remove'|'delete',
  createdAt, actorRootId, targetRootId?, summary, payload? }`
- ⚠️ **事务目前不跨节点传播**：org-share 快照构建时 transactions 实参为 `[]`（sync.ts:59 缺省），
  接收侧 merge 也完全不写 org:tx: 键（sync.ts:93-164）；org:tx: 是纯本地审计日志

## 4. 组织同步快照与版本（organization/sync.ts）

### 4.1 OrganizationSyncSnapshot（sync.ts:12-24）

```
{ orgId,
  summary: { orgId, name, description, basePluginDomain, createdAt, createdBy, updatedAt,
             memberCount, adminCount, metadata?: Record<string,unknown> },
  members: [{ rootId, role, joinedAt, addedBy, nodeInfo?: { peerId?, addresses } }],
  transactions: OrganizationTransactionRecord[],
  sync: OrganizationSyncVersions }       // 仅 versions，无 sections/lastSyncedAt
```

`summary.metadata` = 记录中**非保留键**的剩余字段（保留键：orgId/name/description/basePluginDomain/
createdAt/createdBy/updatedAt/members/sync，sync.ts:26-48）——`recoverySecret` 借此随快照流动。

### 4.2 版本构造与比较

- `buildOrganizationSyncVersions(record, transactionsVersion = record.updatedAt)`（sync.ts:50-57）：
  四个版本字段**全部等于 `record.updatedAt`**，仅 transactionsVersion 可独立（实际取最近事务 createdAt）
- `isOrganizationSyncStale(local, incoming)`（sync.ts:166-177）：local 缺失 → true；
  否则 incoming 的**任一**字段严格大于 local 对应字段 → true。两个方向可同时为 true（分叉）

### 4.3 合并规则 mergeOrganizationSyncSnapshot（sync.ts:93-164）

- 成员按 rootId 合并：incoming 覆盖 existing 的同名字段，但 `nodeInfo` 为 undefined 时保留 existing 值
- 动态字段（metadata）：`{...existingDynamic, ...incomingDynamic}` 合并后删除全部保留键
- 固定字段以 incoming 快照为准；`updatedAt = max(existing, incoming)`；
  `basePluginDomain` 快照缺失时保留 existing
- `sync = { versions: snapshot.sync, sections: ['summary','members','member-details','transactions'],
  lastSyncedAt: Date.now() }`

### 4.4 normalizeIncomingSnapshot（p2p/org-share-snapshot.ts:4-23）

接收侧统一入口，兼容两种线形：

- 有 `summary` 且有 `sync` 且 `members` 为数组 → 原样视为快照（pull 响应路径）
- 否则按原始 OrganizationRecord 处理：`buildOrganizationSyncSnapshot(record, record.transactions ?? [])`——
  ⚠️ **版本被重建**：四字段全部塌缩为 `record.updatedAt`，发送方记录里的 transactionsVersion 丢失

### 4.5 线形不一致（重要）

- org-share **推送**上线的是**原始 OrganizationRecord**（带 `sync:{versions,sections,lastSyncedAt}`，
  org-share-sync.ts:393：`organization.sync ? organization : buildOrganizationSyncSnapshot(...)`，
  而 service 层所有记录都带 sync → 恒走原样分支）
- org-pull-org **响应**发的是 §4.4 重建的**快照**（org-pull-sync.ts:232）
- 两条路径最终都经 normalizeIncomingSnapshot + merge 落库，语义等价但**字节形状不同**；
  Rust 实现两者都必须接受

## 5. nodeInfoClaim（organization/node-info-claim.ts）

### 5.1 格式（node-info-claim.ts:16-24）

```json
{
  "type": "spark-node-info-claim",
  "version": 1,
  "rootId": "<64hex>",
  "publicKey": "<base64，原始 32 字节根公钥>",
  "nodeInfo": { "peerId": "<可省>", "addresses": ["<multiaddr>", ...] },
  "timestamp": 1720000000000,
  "signature": "<base64>"
}
```

### 5.2 签名（node-info-claim.ts:27-39、bootstrap.ts:28-48）

- **待签名载荷**（`buildNodeInfoClaimPayload`）= 固定键序紧凑 JSON：
  ```
  {"type":...,"version":...,"rootId":...,"publicKey":...,
   "nodeInfo":{"peerId":<peerId ?? null>,"addresses":<数组>},"timestamp":...}
  ```
  注意 `nodeInfo.peerId` 缺省时序列化为 **`null`**（`?? null`）；而线上 claim 对象本身
  （`JSON.stringify({...unsigned, signature})`）若 peerId 为 undefined 则**整个键被丢弃**——
  两种序列化不同，验签时统一经 buildNodeInfoClaimPayload 归一，故互认无碍
- 密钥：**root 身份 Ed25519 私钥**（`rootIdentityManager.signWithRootIdentity`，root-id.ts:747-757，
  tweetnacl detached）
- 输入字节 = 载荷字符串 UTF-8；输出 = 64 字节签名的 **base64**

### 5.3 校验规则 verifyNodeInfoClaim（node-info-claim.ts:62-104）

纯函数，按序：

1. 结构校验（type/version/字段类型；addresses 必须是数组；**不要求 peerId 存在**）
2. 新鲜度：`|now - timestamp| ≤ 10 min`（`NODE_INFO_CLAIM_MAX_AGE_MS = 10*60*1000`，:14；
   未来 10 分钟内也接受）
3. rootId 匹配 `^[0-9a-f]{64}$`
4. **身份绑定**：`sha256hex(base64decode(publicKey)) === rootId`（不符报 `public-key-root-mismatch`）
5. Ed25519 验签：payload（§5.2 重建）+ signature(base64) + publicKey(base64)，
   tweetnacl detached verify（root-id.ts:183-195）；公钥须 32 字节、签名须 64 字节

### 5.4 应用条件 applyNodeInfoClaim（service.ts:381-458）

在 §5.3 全部通过后，还须：

1. 若上下文带连接层 `remotePeerId` 且 claim 声明了 `nodeInfo.peerId`，两者必须相等（防代填他人地址，:393-400）
2. claimedNodeInfo 归一化后非空（peerId 或 addresses 至少其一）
3. 遍历本地全部组织记录：**本机当前用户是该组织 admin** 且 **claim.rootId 是该组织成员** 才落库；
   非管理员组织静默跳过（:411-418）
4. 与现有 nodeInfo 完全一致（peerId 相等且 addresses 的 JSON 序列化相等）则跳过，不 bump 版本（:420-425）
5. 落库：更新成员 nodeInfo、bump updatedAt、追加 `member-update` 事务（actor = claim 者本人）、
   重建 sync 版本；随后尽力向其余已知成员推送更新后快照（:427-456）
6. 入口侧还有一道前置闸：org-pull-list 只在 requesterRootId 是本地某组织**已知成员**时才处理
   其捎带的 claim（org-pull-sync.ts:165-184），未认证请求不触发验签与落库扫描

## 6. 加入流程（invite → 连接 → claim → 拉取）

1. 管理员 `addMember` 预录被邀请人 rootId（可暂无 nodeInfo），随后尽力向其已知地址推送快照；
   成员离线不视为失败（service.ts:216-309、537-571）
2. 管理员 `createOrgInvite`：payload.inviter = 本机 rootId + 当前 peerId/地址（service.ts:315-339）
3. 被邀请人 `acceptOrgInvite(code)`（service.ts:345-374）：
   - 解码校验邀请码（§2.3）；拒绝自己发给自己的邀请码
   - 构造自签 nodeInfoClaim（bootstrap.ts:28-48：rootId + 根公钥 + 本机 peerId/地址 + 当前时间戳）
   - `connectAndPull(inviter.nodeInfo, { nodeInfoClaim })` → 连接邀请人并执行 org-pull 对账（§9），
     claim 随 org-pull-list 请求捎带
   - 拉取完成后查本地 `org:meta:<invite.orgId>`：记录存在且自己为成员才算加入成功，
     否则报"请确认管理员已先将你的 RootID 录入组织成员"

## 7. org-share 推送流程（org-share-sync.ts:384-484）

发送 `syncOrganizationToMember(nodeInfo, targetRootId, organization)`：

1. `syncId = randomBytes(12).toString('hex')`（24 hex，:391）
2. 若已知目标 peerId 且存在历史 sync-state：stale 检查（⚠️ 因 §p2p-messages 13.2 的污染，
   该检查恒判不 stale，即**存在历史记录后推送恒被跳过**——现状如此，Rust 复刻时需决策是否带 bug 对齐）
3. `connectPeer` → 等待目标订阅 `spark-sync`（最长 5000ms，200ms 轮询，org-share-session.ts:44-68）
4. **直连优先**：`/spark/org-share/1.0.0` 逐地址尝试，响应 `ok && syncId 匹配` 即送达，
   写 sync-state 后返回（:320-382、435-442）
5. **pubsub 兜底**：向 `spark-sync` 广播 org-share 信封，重试节奏 `[0, 400, 1000, 2000, 3500]`ms 共 5 次，
   每次发布后等 ack 1500ms（:444-481）；收到匹配 syncId 的 org-share-ack 即送达
   （ack 可先于等待到达，有竞态缓存，org-share-session.ts:11-38）
6. 全部失败抛 `Organization sync ack timeout`

接收 `applyIncomingOrgShare`（:178-252，pubsub 与直连共用）：

1. payload 须有 `targetRootId` 与 `organization.orgId`
2. **本机当前 rootId 必须等于 targetRootId**（定向投递，不符静默丢弃）
3. 本机 rootId 必须在 organization.members（或 summary.members）中
4. `normalizeIncomingSnapshot` → `mergeOrganizationSyncSnapshot(existing, snapshot)` 落库 `org:meta:<orgId>`
5. 应用 pluginDocs（§11）
6. 回 ack：直连路径写响应帧；pubsub 路径广播 org-share-ack（domain='system'）

## 8. org-sync-state 记账

见 p2p-messages.md §11。org.md 侧补充：K 副本概览（§12）是该记录的唯一消费者；
admin 补副本推送（§12.4）与"是否跳过推送"判定（§7.2）也读它。

## 9. org-pull 拉取/对账流程（org-pull-sync.ts:298-467）

`reconcileFromPeer(nodeInfo, { nodeInfoClaim? })`：

1. 连接目标 → 发 `org-pull-list`（payload 带本机 rootId、本机 peerId、可选 nodeInfoClaim）
2. 对端响应可见组织列表 `{orgId, sync}`（成员校验通过才列出，见 §9.2）
3. 取"本地相关组织 ∪ 对端可见组织"逐个对账：
   - 本地有、对端列表没有 → 发 `org-pull-org` 确认：`status:'member'` 则合并落库并记 sync-state；
     `status:'removed'` 则**删除本地记录**；无有效响应且本地更新 → 反推 org-share
   - 双方都有 → 双向 stale 比较：本地严格更新（且对端不更新）→ 反推；完全等价 → 跳过；
     其余（对端更新或双方分叉）→ 发 `org-pull-org` 拉取合并
   - `status:'removed'` 的删除同样适用于此分支
4. 成功拉取后写 sync-state（规范 versions 形状，org-pull-sync.ts:288）
5. 返回计数 `{ checked, synced, removed, pushAttempted, pushed, pulled, skipped }`（synced === pulled）

### 9.2 响应侧成员校验 memberAuthStatus（org-pull-sync.ts:98-116）

- requesterRootId 不在 members → `{ ok:false, reason:'not-member' }`
- 成员记录**有** `nodeInfo.peerId` 时：`requesterPeerId`（请求声明值，缺省回退连接层 remotePeer）
  必须与之相等，否则 `peer-mismatch`
- ⚠️ requesterPeerId 是请求方自报值，无密码学绑定；真正的防伪是 claim 链路（§5.4）
- org-pull-org 对校验失败统一回 `ok:true, status:'removed'`——与"组织真被删除"**不可区分**，
  拉取方据此删除本地记录（peerId 漂移且 claim 未先行回填时会自我剔除；
  缓解：list 请求先处理 claim 再重读记录，org-pull-sync.ts:167-184）

## 10. org-recovery 流程

- token 算法与协议帧见 p2p-messages.md §8
- 恢复视图 `getRecoveryView`（service.ts:158-197）：当前用户为成员的每个组织一条
  `{ orgId, recoverySecret, memberNodeInfos }`（仅含 addresses 非空的成员 nodeInfo）
- 存量组织缺 recoverySecret 时由 **admin 惰性补齐**（随机 64 hex，bump updatedAt 后落库，
  经反熵扩散；非成员角色本轮跳过等待 gossip，service.ts:173-186）
- 触发与拨号口径见 p2p-messages.md §8.4；命中候选只拨号不写成员表

## 11. pluginDocs 随组织同步（p2p/plugin-org-sync.ts）

- 条目：`{ domain, collection, id, payload, meta: { vv, ts, nodeId? }, schema? }`（:7-14）
- 收集：扫 `doc:plugin:` 前缀键（键形 `doc:<domain=plugin:*>:<collection>:<id>`，:16-29），
  仅取 `payload.orgId === 目标 orgId` 且未标记同步禁用
  （`payload.__sync === false`，或 `__sync.disabled === true`，或 mode/strategy ∈ {local, none, disabled}，:31-48），
  meta 从本地 meta 键读取（须有 vv 与 ts），schema 从集合策略注册表读取
- 应用：逐条 `applyRemoteUpdate`（:126-147）
- 挂载点：org-share payload.pluginDocs（org-share-sync.ts:430）、org-pull-org 响应 pluginDocs（org-pull-sync.ts:239）

## 12. K 副本统计口径（org-share-sync.ts:107-176）

### 12.1 参数

- 副本目标 **K = 3**（`ORG_REPLICA_TARGET`，constants.ts:152；含本机）
- 新鲜窗口 **30 天**（`ORG_REPLICA_FRESH_WINDOW_MS = 30*24*60*60*1000`，constants.ts:161）

### 12.2 逐成员判定（getOrgSyncOverview）

对组织每个成员（按记录 members 顺序，rootId 非法跳过）：

- `state = org-sync-state[member.nodeInfo.peerId, orgId]`（成员无 peerId 则 state=null）
- `recentlySynced = state && (now - state.lastSyncedAt ≤ 30天)`
- `coversCurrent = state && currentVersions && !isOrganizationSyncStale(state.versions, currentVersions)`
  - currentVersions = `record.sync.versions`，缺失时以 `buildOrganizationSyncVersions(record)`（= updatedAt）兜底
  - 语义：sync-state 记录的版本仍覆盖当前组织版本（静默组织的健康副本不因 TTL 误判过期）
- **`everSynced = isSelf || recentlySynced || coversCurrent`**（本机恒为 true）
- **`syncedPeers` = everSynced 为 true 的成员数**；`totalMembers` = 有效成员数；
  返回 `lastSyncedAt = state?.lastSyncedAt ?? null`

判定逻辑是"30 天窗口内同步过"**或**"版本仍覆盖"二选一（constants.ts:154-161 的设计注释：
不能只用版本比较——每次编辑会瞬间翻转；也不能只用 TTL——静默组织不会刷新 sync-state）。

### 12.3 形状污染对统计的影响

share 路径写入的 sync-state 其 `versions` 是外壳对象（p2p-messages.md §13.2），
此时 `isOrganizationSyncStale(state.versions, currentVersions)` 中 local 四字段全 undefined，
比较恒 false → `coversCurrent` 恒 true → 该成员**永久计入** everSynced（绕过 30 天窗口）。
pull 路径写入的规范 versions 不受影响。

### 12.4 管理员补副本（p2p-node.ts:507-568）

keepalive 每 tick：遍历本机为 **admin** 的组织，`syncedPeers < K(=3)` 时，
向 `!isSelf && !everSynced` 且有 nodeInfo 的成员推送 org-share，**每组织每 tick 最多 2 个**。

## 13. 常量速查

| 参数 | 值 | 位置 |
|---|---|---|
| 邀请码有效期 | 24 h（未来时间不封顶） | invite.ts:27 |
| nodeInfoClaim 新鲜窗口 | ±10 min | node-info-claim.ts:14 |
| K（副本目标，含本机） | 3 | p2p/constants.ts:152 |
| 副本新鲜窗口 | 30 天 | p2p/constants.ts:161 |
| recovery timeBucket | 10 min | p2p/constants.ts:110 |
| recovery TTL / want / 触发 tick / 冷却 / 应答限流 | 2 / 8 / 3 / 10 min / 30 s | p2p/constants.ts:115-135 |
| org-share 订阅等待 / ack 等待 / 重试节奏 | 5000 ms / 1500 ms / [0,400,1000,2000,3500] ms | org-share-sync.ts:415,444,461 |
| syncId / txId / orgId / recoverySecret | 12B→24hex / 8B→16hex / 8B→"org_"+16hex / 32B→64hex | org-share-sync.ts:391、transaction-store.ts、service.ts:88-90,124 |
| org-sync-state / peer 活跃度保留期 | 90 天 / 90 天 | data-management/constants.ts:11-17 |
| keepalive 周期 | 60 s | bootstrap.ts:21 |

## 14. 已知不一致与坑（Rust 对齐必读）

1. **三条签名链并存，输入构造各不相同**：
   - pubsub 信封：临时 ed25519 密钥（PEM 公钥自证），签名输入 = 信封去 signature 的 JSON，键序=插入序
     （p2p-messages.md §3.3）
   - nodeInfoClaim：root 密钥，载荷固定键序且 `nodeInfo.peerId ?? null`（§5.2）；
     线上对象缺 peerId 时**丢键**，载荷里却是 `null`——重建载荷时必须走 `?? null` 归一
   - node-announce：libp2p 节点密钥，载荷固定键序（p2p-messages.md §5.2）
2. **org-share 推送 vs org-pull 响应的 organization 线形不同**（原始记录 vs 重建快照，§4.5）；
   且原始记录路径会把四版本塌缩为 updatedAt（transactionsVersion 丢失，§4.4）
3. **org-sync-state 两种形状共存** + share 路径 stale 检查恒不 stale（首次成功后再不推送）
   + coversCurrent 恒 true（§12.3、p2p-messages.md §13.2）。这是现状行为；
   Rust 重写需明确决策：逐 bug 对齐还是修复（修复会改变网络行为，需全链路评估）
4. **org-pull-org 的 'removed' 与'无权限'不可区分**：peer-mismatch 也会导致拉取方删除本地组织记录（§9.2）
5. **claim 只在三个条件下落库**：校验通过 + 本机是该组织 admin + 声明者是该组织成员（§5.4）；
   非 admin 节点收到 claim 静默丢弃——新成员地址只能经 admin 落库后随快照扩散
6. **事务记录不同步**：org:tx: 纯本地，快照 transactions 恒为 []，merge 不写事务键（§3.3）
7. **时间戳校验口径不统一**：claim/announce 用 `Math.abs`（未来 10 min 内可接受）；
   邀请码只查过去 24h，未来 createdAt 不设上限
8. **memberAuthStatus 的 peerId 绑定是自报值**（§9.2），不构成认证；认证依赖 claim 的签名链
9. 邀请码无签名：orgId/orgName 可被邀请人随意篡改，但成员资格校验始终在拉取侧
  （invite.ts:1-7 的设计说明）；accept 后按 invite.orgId 查本地记录判成功与否（service.ts:365-373）
10. **reconcile 反推的 targetRootId 恒为发起方自己**（org-pull-sync.ts:372/396 传入
    currentRootId）：接收侧定向校验（§7 第 2 条 targetRootId 必须等于接收方当前 rootId）
    使跨身份反推恒被静默丢弃，仅同身份多设备成立。**Rust 有意修复**：反推前先按对端
    peerId 在本地组织成员表反查目标 rootId，查不到才回退 TS 原值（同身份多设备路径
    不受影响；反查错误的最坏结果与 TS 相同——被拒，收敛仍由对端拉取兜底）
11. **removeMember / applyIncomingOrgShare 不触发 org-share 推送**（service.ts:460-498、
    org-share-sync.ts:178-252 均无 syncContext 调用）：移除经 org-pull `removed` 状态
    传播，接收方等待下次对账。推送仅发生在 addMember 与 claim 落库后（均为尽力而为，
    失败不阻断落库——TS 的"先推后落"顺序对结果无影响，Rust 拉平为落库后异步推送）

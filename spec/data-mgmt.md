# 数据自动管理规格（usage / cleanup / purge / watermark / exporter / service）

> 来源：反向提取自 `desktop/src/main/data-management/{constants,usage,cleanup,purge,watermark,exporter,service}.ts`
> 与 `desktop/src/main/ipc/data.ts`。Rust 实现必须逐字节对齐存储 key 前缀、JSON 字段名与全部数值参数。

## 1. 常量总表（constants.ts）

| 常量 | 值 | 含义 | 出处 |
|---|---|---|---|
| `TOMBSTONE_RETENTION_MS` | `90*24*60*60*1000` = 7,776,000,000（90 天） | lww tombstone 保留期 | constants.ts:11 |
| `PEER_RECORD_RETENTION_MS` | 同上（90 天） | p2p 节点活跃记录保留期 | constants.ts:14 |
| `ORG_SYNC_STATE_RETENTION_MS` | 同上（90 天） | p2p 组织同步记账保留期 | constants.ts:17 |
| `DATA_MAINTENANCE_INTERVAL_MS` | `60*60_000` = 3,600,000（1 小时） | 调度 tick 周期 | constants.ts:20 |
| `AUTO_CLEANUP_MIN_INTERVAL_MS` | `24*60*60_000` = 86,400,000（24 小时） | 自动清理最小间隔 | constants.ts:23 |
| `USAGE_WARN_TOTAL_BYTES` | `1*1024*1024*1024` = 1,073,741,824（1 GiB） | 软配额警告阈值（不拒绝写入） | constants.ts:26 |
| `DISK_FREE_WARN_RATIO` | `0.15` | 磁盘可用比例警告阈值 | constants.ts:29 |
| `KEY_RANGE_UPPER_BOUND` | `'\u{10FFFF}'` | 前缀范围扫描上界键 | constants.ts:36 |

⚠️ 扫描上界（`\xFF` vs `U+10FFFF`）：LevelDB 以 `keyEncoding:'utf8'` 比较**编码后字节**。
JS 字符串 `'\xFF'` 即 U+00FF（ÿ），UTF-8 编码为 `C3 BF`——任何前缀后首字节 > 0xC3
的 key（如中文 id，首字节 E4+）会被静默排除在扫描范围外。`U+10FFFF` 编码为 `F4 8F BF BF`，
是最大合法 UTF-8 码位，可覆盖全部合法 key。因此：

- **data-management 模块全部 6 处扫描统一用 `KEY_RANGE_UPPER_BOUND`（U+10FFFF）**：
  usage.ts:98、cleanup.ts:38、purge.ts:59/113/135、exporter.ts:27；
- **但 `LevelDB.queryRange` 的默认 end 仍是 `${prefix}\xFF`**（db/base.ts:200），
  且模块外大量调用方仍显式传 `\xFF`：ipc/db.ts:77、db/query.ts:4、db/collection.ts:346-361、
  p2p/plugin-org-sync.ts:67、p2p/overlay-peer-store.ts:123、p2p/organization-bootstrap-sync.ts:37、
  p2p/peer-activity-store.ts:182/214/281、p2p/p2p-node.ts:511、p2p/org-pull-sync.ts:125、
  organization/service.ts:508。即该缺陷仅在 data-management 内修复，其余路径对非 ASCII key 仍会漏扫。

## 2. 用量统计（usage.ts）

### 2.1 分类口径（classifyKey，usage.ts:49-58）

按 key 前缀归类，**顺序敏感**（更具体的前缀先判）：

1. `doc:evidence:` → `evidence`（存证链）
2. `doc:system:` → `system`（策略注册表、purge 水位线 `doc:system:purge-watermark:*`、
   审计日志 `doc:system:purge-log:*`、配置）
3. `doc:` → `documents`（业务文档 `doc:plugin:*` / `doc:<domain>:*`）
4. `idx:` → `indexes`（二级索引）
5. `meta:` → `syncMeta`（同步元数据，含 tombstone）
6. `org:` → `organization`（`org:meta:*` / `org:tx:*`）
7. `p2p:` → `p2p`（含 `p2p:peer:record:*`、`p2p:org-sync-state:*`）
8. 其余 → `other`

### 2.2 扫描与计量（collectDataUsage，usage.ts:92-121）

- 全库单遍扫描：`db.queryRange({ prefix: '', end: KEY_RANGE_UPPER_BOUND })`（usage.ts:98）。
  未传 `start`，`gte` 取默认 `''`；`end` 为**排他上界**（`lt`）。
- 每行字节数 = `Buffer.byteLength(key,'utf8') + Buffer.byteLength(value,'utf8')`（usage.ts:100）。
- 报告结构 `DataUsageReport`（usage.ts:34-46）：
  `{scannedAt, classes: Record<8类,{keys,bytes}>, totalKeys, totalBytes, disk, warnings}`。
- 警告判定（usage.ts:116-119）：
  - `usageExceeded = totalBytes > 1073741824`（严格 `>`）；
  - `diskLow = disk !== null && disk.freeRatio < 0.15`（严格 `<`）。

### 2.3 磁盘信息（measureDiskInfo，usage.ts:74-86）

- `statfs(path)`；`freeBytes = bavail * bsize`（**bavail**，非 bfree），`totalBytes = blocks * bsize`；
- 任一为非有限数或 `totalBytes <= 0` → 返回 `null`；`statfs` 异常静默返回 `null`；
- `freeRatio = freeBytes / totalBytes`。

### 2.4 cachedUsage 的刷新与失效（service.ts）

缓存字段 `DataManagementService.cachedUsage`（service.ts:19），刷新/失效路径：

- **tick**（service.ts:40-51）：每次 tick（1h）末尾无条件 `cachedUsage = await collectDataUsage(db, db.path)`；
  若本次 tick 跑了自动清理且删除总数 > 0，先置 `null` 再随即重采样（service.ts:45-48）。
- **runCleanupNow**（service.ts:54-59）：清理后置 `null`，**不立即重采样**。
- **invalidateUsage**（service.ts:73-75）：供 IPC 直调 purge 后手动失效（ipc/data.ts:120）。
- **getUsage**（service.ts:62-67）：缓存为 `null` 时现算并回填，否则直接返回缓存。

⚠️ 缓存是**纯内存**字段，进程重启即丢；`lastAutoCleanupAt` 同为内存字段（见 §6）。

## 3. 过期清理 L1（cleanup.ts）

只清"可重建 / 已终结"状态，业务文档一律不动。三类对象，共用判定式
`now - <时间字段> > <保留期>`（**严格 `>`**，恰好满 90 天不删）：

| 类别 | 扫描前缀 | JSON 条件 | 保留期 | 函数 |
|---|---|---|---|---|
| tombstone | `meta:`（全域） | `value.tombstone === true` 且 `typeof value.ts === 'number'` | 90 天 | cleanup.ts:49-62 |
| p2p 节点记录 | `p2p:peer:record:`（P2P_PEER_RECORD_PREFIX，p2p/constants.ts:30） | `typeof value.lastSeenAt === 'number'` | 90 天 | cleanup.ts:65-77 |
| 组织同步记账 | `p2p:org-sync-state:`（ORG_SYNC_STATE_PREFIX，p2p/constants.ts:146） | `typeof value.lastSyncedAt === 'number'` | 90 天 | cleanup.ts:80-92 |

- 扫描：`scanRows`（cleanup.ts:37-46）`queryRange({prefix, start: prefix, end: prefix + U+10FFFF})`，
  每行 `JSON.parse`；**解析失败的行 value 置 `null`，三类过滤器均跳过**（损坏行永不被清理）。
- 删除：各类别分别一次 `db.batch([{type:'del', key}...])`，无过期项则跳过 batch。
- `runAutoCleanup`（cleanup.ts:95-119）：`now = Date.now()` 取一次共用；三类各自 try/catch，
  单步失败仅 `console.warn` 不影响其余；返回 `{ranAt, tombstones, peerRecords, orgSyncStates}`，
  删除总数 > 0 时 `console.log`。
- tick 间隔：tick 每 1h 一次，但仅当 `now - lastAutoCleanupAt >= 24h` 才真正执行（见 §6）。

⚠️ tombstone 保留期的语义（cleanup.ts:14-19 注释）：tombstone 是 lww 删除收敛的依赖。
若全网副本都 GC 了同一 tombstone，离线超期节点重推旧 doc 会因本地 meta 缺失（LWW 判 remote 胜）
使文档网络级复活。90 天 = "最大离线窗口"取舍。

## 4. 手动清理 purge（purge.ts）

L2 级：仅管理员手动触发，只接受 `plugin:` 域。语义：删除指定域（可选限定集合）中
`meta.ts < beforeTs` 的全部本地副本（doc + idx + meta，含同时代 tombstone），
并把每个受影响集合的 purge 水位线抬升到 `beforeTs`。

### 4.1 selectExpiredMetas（purge.ts:50-92）

- 入参校验（顺序固定）：
  1. `domain` 必须以 `plugin:` 开头且长度 > 7，否则抛
     `Refused to purge non-plugin domain ...`（purge.ts:51-53）；
  2. `beforeTs` 必须是 `number` 且 `> 0`，否则抛（purge.ts:54-56）。
- 扫描前缀：`meta:{domain}:{collection}:`（指定 collection）或 `meta:{domain}:`（缺省），
  上界 U+10FFFF（purge.ts:58-59）。
- key 解析（purge.ts:65-71）：去掉 `meta:{domain}:` 后取**第一个 `:`** 为界，
  前段为 collection、后段全部内容为 id（id 含冒号亦精确）；`separator <= 0` 或在末尾的行跳过。
- ts 过滤（purge.ts:73-82）：`JSON.parse` 失败或 `ts` 非 number → 跳过（保守不删）；
  **选中条件为 `ts < beforeTs`（严格 `<`）**，`ts >= beforeTs` 跳过。
- 选中项记录 `{collection, id, key, bytes}`，`bytes` 仅计 meta 行自身 key+value。

### 4.2 buildPurgePlan 的 key 集合（purge.ts:95-148）

按 collection 分组，逐集合累加删除 op 与 freedBytes（均为 key+value 的 UTF-8 字节数）：

1. **doc**：扫 `doc:{domain}:{collection}:`，id 在选中集内 → 删（purge.ts:112-122）；
2. **meta**：选中集全删（含同时代 tombstone）（purge.ts:125-128）；
3. **idx**：扫 `idx:{domain}:{collection}:`（索引键格式
   `idx:{domain}:{collection}:{indexName}:{encValue}:{id}`），
   以**尾部 `:​{id}` 匹配**（`row.key.endsWith(':' + item.id)`）→ 删（purge.ts:134-144）。

⚠️ idx 尾部匹配的已知缺陷（purge.ts:130-133 注释如实记录）：若未来允许 id 内含冒号，
清理 id `"b"` 会误匹配 id `"a:b"` 的索引行；当前各环节产生的 id 均不含冒号。

### 4.3 previewPurgeDomainDocs（purge.ts:151-160）

复用 select + buildPurgePlan 但**不执行任何写**。返回
`{collections, affectedDocs, affectedBytes}`：`affectedDocs` 以 meta 条数计（每文档恰好一条），
`affectedBytes` 含 doc/meta/idx 三类合计。

### 4.4 purgeDomainDocs 执行流程（purge.ts:166-207）

1. `selected = await selectExpiredMetas(...)`；随后 `purgedAt = Date.now()`；
2. **`selected.length === 0` → 直接返回** `{domain, beforeTs, collections: [], removedDocs: 0, freedBytes: 0, purgedAt}`，
   **不抬水位线、不写审计日志**（purge.ts:171-173）；
3. `db.batch(ops)` 一次性删除（purge.ts:176）；
4. 逐 collection 调 `raisePurgeWatermark(db, domain, collection, beforeTs, 该集合删除数)`（purge.ts:179-182）；
5. 写审计日志（purge.ts:184-196），key = `doc:system:purge-log:{purgedAt}`（毫秒时间戳），
   value = `JSON.stringify({domain, collection: options.collection ?? null, beforeTs, collections, removedDocs, freedBytes, purgedAt})`；
6. 返回 `{domain, beforeTs, collections, removedDocs, freedBytes, purgedAt}`。

⚠️ 审计日志 key 以毫秒时间戳结尾，同毫秒内两次 purge 会后写覆盖先写。
⚠️ 选中→batch 非原子的竞态（purge.ts:13-16 注释）：期间若选中 id 收到 `ts >= beforeTs` 的远端
新写入会被一并删除；水位线不拦截它，靠后续反熵从其他副本补回（可自愈）。

## 5. purge 水位线（watermark.ts）

### 5.1 存储格式

- key：`doc:system:purge-watermark:{encodeURIComponent(domain + "/" + collection)}`（watermark.ts:25-29），
  复用 collectionSchemaKey 的 encodeURIComponent 技巧；系统域，插件经底层 db 接口无法篡改。
- value = `JSON.stringify(PurgeWatermarkRecord)`，字段（watermark.ts:14-23）：
  `{domain, collection, purgedBefore, purgedAt, removedDocs}`。
  - `purgedBefore`：该时间戳之前（严格小于）的文档已清理，远端重推一律拒绝；
  - `purgedAt`：最近一次清理执行时间；`removedDocs`：累计清理文档数。

### 5.2 读写规则

- `getPurgeWatermark`（watermark.ts:32-56）：key 不存在、JSON 损坏、或 `purgedBefore` 非 number
  → 返回 `null`；`purgedAt`/`removedDocs` 缺失或非 number 时**默认 0** 容忍；
  返回记录的 domain/collection 以入参为准（不信任存储值）。
- `raisePurgeWatermark`（watermark.ts:62-79）：**只升不降**——
  `purgedBefore = max(existing ?? 0, 新值)`；`purgedAt = Date.now()`；
  `removedDocs = (existing ?? 0) + 新增`。水位线永不被任何清理流程删除。
- 拦截判定 `isPurgedByWatermark`（watermark.ts:85-96）：
  - `remoteTs` 非 number 或 `<= 0` → **返回 false（放行）**；
  - 否则 `remoteTs < watermark.purgedBefore` → 拦截（**严格 `<`**；`ts == purgedBefore` 不拦截）。
- 拦截点在 `applyRemoteUpdate` 入口（db/sync.ts:195-198）：命中则跳过落地并
  `console.log('[sync] skip remote update: purged by watermark', ...)`。

⚠️ 与 selectExpiredMetas 的边界一致性：purge 删 `ts < beforeTs`（严格），水位线写入
`purgedBefore = beforeTs`，拦截 `remoteTs < purgedBefore`（严格）。因此 `ts == beforeTs`
的数据**既不会被删、也不会被拦**——两侧规则一致，无需特判。
⚠️ `remoteTs <= 0` 或缺失时水位线放行，是否拦截落到后续 LWW/append-only 逻辑判定。

## 6. DataManagementService（service.ts）

- 单例：`new DataManagementService(levelDB)`（bootstrap.ts:125），随核心服务启动
  `start()`（bootstrap.ts:257），db-close 时 `stopDataMaintenance()`（bootstrap.ts:127-129）。
- 调度器：`KeepaliveScheduler('data-maintenance', 3_600_000, tick)`（service.ts:22-24）。
  - `setInterval` 固定间隔，**start 后首个 tick 在 1 小时后才触发**（无立即执行）；
  - 防重入：上次 tick 未结束跳过本次；tick 抛错被调度器吞掉仅 warn；
  - timer 调了 `unref()`；start/stop 幂等（p2p/keepalive.ts:19-61）。
- **tick 流程**（service.ts:40-51）：
  1. `now = Date.now()`；
  2. `now - lastAutoCleanupAt >= 86_400_000` → 执行 `runAutoCleanup`，
     `lastAutoCleanupAt = now`；删除总数 > 0 则 `cachedUsage = null`；
  3. 无条件 `cachedUsage = await collectDataUsage(db, db.path)`。
  - `lastAutoCleanupAt` 初值 0（内存字段）→ **启动后第一个 tick 必然执行清理**。
- **runCleanupNow**（service.ts:54-59）：立即 `runAutoCleanup`；`lastAutoCleanupAt = Date.now()`；
  `cachedUsage = null`（不重采样，下次 getUsage 现算）；返回 `AutoCleanupResult`。
- **getUsage**（service.ts:62-67）：缓存优先，`null` 时现算回填。
- **invalidateUsage**（service.ts:73-75）：仅置 `null`，供绕过门面的写路径（IPC purge）调用。

## 7. 导出（exporter.ts）

- 口径：全库逻辑 dump（非 LevelDB 目录拷贝）；不做周期自动备份；**不含 RootID 身份**
  （identities/ 目录单独存放且密码加密，身份备份走助记词）。
- `ExportDump` 结构（exporter.ts:17-22）：
  `{formatVersion: 1, app: 'spark-desktop', exportedAt: number, entries: Array<{key, value}>}`，
  key/value 均为原始字符串。
- 扫描：`queryRange({prefix: '', end: KEY_RANGE_UPPER_BOUND})`（exporter.ts:27），含系统域
  （水位线、审计日志、策略注册表）在内的**全部 key**。
- 写文件（exporter.ts:37-42）：`JSON.stringify(dump)`（无缩进）以 utf8 `writeFile`；
  返回 `{path, entries, bytes}`，`bytes` 为 JSON 文本的 UTF-8 字节数。
- 导出路径约定在 IPC 层（ipc/data.ts:57-71）：`dialog.showSaveDialog`，
  `defaultPath = spark-export-{stamp}.json`，`stamp = new Date().toISOString().replace(/[:.]/g,'-').slice(0,19)`
  （UTC，形如 `2026-07-21T22-33-16`），过滤器 `[{name:'JSON', extensions:['json']}]`；
  用户取消返回 `{cancelled: true}`，否则 `{cancelled: false, path, entries, bytes}`。

## 8. IPC 流程与权限（ipc/data.ts）

全部 5 个 channel 第一行均为 `requireSystemDomain(event)`（helpers.ts:25-30）：
调用方窗口必须绑定**系统域**，否则抛 `Access denied: system domain required`。
随后 `ensureReady()`：`levelDB` 未打开则 `ensureCoreServicesStarted()`（ipc/data.ts:18-22）。

| Channel | 处理 | 权限要求 |
|---|---|---|
| `data-usage` | `dataManagementService.getUsage()` | 系统域 |
| `data-cleanup-now` | `dataManagementService.runCleanupNow()` | 系统域 |
| `data-export` | 保存对话框 + `writeExportDump` | 系统域 |
| `data-purge-preview` | 见下 | 系统域 + 组织成员（**不要求管理员**） |
| `data-purge-execute` | 见下 | 系统域 + **组织管理员** + 已导出确认 + K 副本充足 |

### 8.1 data-purge-preview(orgId, beforeTs)（ipc/data.ts:73-86）

1. `resolveOrg(orgId)`（ipc/data.ts:25-35）：`organizationService.listMine()` 中按 orgId 查找，
   找不到抛 `Organization not found or not a member`；`basePluginDomain` 为空抛错；
2. `previewPurgeDomainDocs(levelDB, {domain: org.basePluginDomain, beforeTs})`；
3. 返回 `{orgId, domain, beforeTs, preview, replica, isCurrentUserAdmin}`，
   其中 `replica = getReplicaOverview(orgId)`（P2P 未初始化或未启动 → `null`）。

⚠️ preview **不校验管理员**：任何组织成员可预览影响面，`isCurrentUserAdmin` 仅作为
返回字段供 UI 决定是否放行下一步。

### 8.2 data-purge-execute(orgId, beforeTs, confirmExported)（ipc/data.ts:88-126）

校验顺序固定，任一失败即抛错：

1. `resolveOrg(orgId)`；
2. **管理员判定**：`org.isCurrentUserAdmin !== true` → 抛
   `Only organization admins can purge historical data`。
   `isCurrentUserAdmin` 的来源（organization/service.ts:573-586）：
   `members.find(m => m.rootId === currentRootId)?.role === 'admin'`；
3. `confirmExported !== true` → 抛（⚠️ 仅为渲染进程传入的布尔确认，无导出事实核验）；
4. `getReplicaOverview(orgId)` 为 `null`（P2P 未启动）→ 抛 `purge refused`；
5. `replica.syncedPeers < replica.replicaTarget` → 抛副本不足错误
   （overview 结构 `{orgId, replicaTarget, syncedPeers, totalMembers, members[]}`，
   p2p/org-share-sync.ts:107-117）；
6. **并发护栏**：`purgeInFlight`（`Set<string>`，ipc/data.ts:16）按 `basePluginDomain` 去重，
   同域进行中抛错；`finally` 中移除；
7. `purgeDomainDocs(levelDB, {domain: basePluginDomain, beforeTs})`——
   ⚠️ **IPC 永远不传 `collection`**，模块层的单集合清理能力当前不可达；
8. 成功后 `dataManagementService.invalidateUsage()`（ipc/data.ts:120）。

## 9. 已知不一致与坑（如实记录）

1. **扫描上界双标**：data-management 内统一 U+10FFFF，但 `queryRange` 默认上界（db/base.ts:200）
   与模块外 12 处调用仍是 `\xFF`（U+00FF，C3 BF），非 ASCII key（首字节 > 0xC3）在那些路径被静默漏扫。
2. **purge 全库扫面外的 `''` 前缀**：usage.ts:98 与 exporter.ts:27 用 `prefix:''` + U+10FFFF 上界，
   `end` 为排他（`lt`）——key 恰好等于或以 `\u{10FFFF}` 开头的极端 key 不会被统计/导出（实际不产生此类 key）。
3. **清理对损坏行保守跳过**：cleanup 三类过滤器都要求 JSON.parse 成功；损坏行永不清理、也不计入统计之外的处理。
4. **空 purge 不留痕**：`selected.length === 0` 时不抬水位线、不写审计日志。
5. **审计日志 key 毫秒冲突**：`doc:system:purge-log:{purgedAt}` 同毫秒两次 purge 互相覆盖。
6. **confirmExported 无核验**：仅渲染进程布尔，主进程不验证导出真的发生过。
7. **preview 不鉴权管理员**：仅 execute 强制 `isCurrentUserAdmin`。
8. **单集合 purge 不可达**：`PurgeOptions.collection` 仅模块层支持，IPC 层恒为全域清理。
9. **水位线放行非法 ts**：`remoteTs <= 0` / 非 number 时 `isPurgedByWatermark` 返回 false。
10. **idx 尾部匹配缺陷**：`endsWith(':' + id)` 对含冒号 id 会误匹配（当前 id 不含冒号，purge.ts:130-133 已注明）。
11. **缓存与清理状态纯内存**：`cachedUsage`/`lastAutoCleanupAt`/`purgeInFlight` 进程重启即丢；
    首个 tick 在 start 后 1 小时才触发，且因 `lastAutoCleanupAt = 0` 必然立即执行一轮清理。
12. **runCleanupNow 不重采样**：缓存置 `null` 后等下次 `getUsage` 现算，期间 UI 读到的用量是现算结果。

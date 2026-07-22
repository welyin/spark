# 同步与存证规格（schema / sync / evidence）

> 来源：反向提取自 `desktop/src/main/db/{schema,sync,evidence}.ts`。
> Rust 实现必须逐字节对齐 canonical JSON 与哈希链规则；以 `vectors/sync-evidence.json` 验收。

## 1. Canonical JSON（最高优先级，所有哈希的根基）

`normalizeObject(value)` 规则（evidence.ts:22-31）：

- `undefined` → 字符串 `"undefined"`
- `null` → 字符串 `"null"`
- 非 object（number/string/boolean）→ `JSON.stringify(value)`
- object → 新对象：key 按 `Object.keys(value).sort()`（JS 字符串排序=UTF-16 code unit 序），
  每个 value **先递归 normalize 再作为值**放入，最后 `JSON.stringify(ordered)`

⚠️ 关键语义：object 的递归值在 JS 里是 normalize 后的**字符串**（嵌套对象被序列化成字符串嵌入）。
即 `normalizeObject({a:{b:1}})` = `{"a":"{\"b\":1}"}` —— 内层对象变成 JSON 字符串值。
Rust 实现必须复刻这个"嵌套字符串化"行为，不能做成常规 canonical JSON。

⚠️ JS `JSON.stringify` 细节需对齐：数字格式（整数无小数点、浮点按 JS Number→String 规则）、
字符串转义（非 ASCII 不转义、控制字符短转义）、无空格。数组会落入 object 分支
（`typeof [] === 'object'`，key 为 "0","1",...）。
⚠️ key 序的真实行为（已由向量固化，以此为准）：normalizeObject 按字典序 sort，但
`JSON.stringify` 对**整数型 key**（canonical array index，< 2^32-1）恒按**数值升序**输出，
与插入/排列顺序无关——所以 `[0..10]` 输出为 `0,1,2,…,9,10` 数值序，而非字典序 `"10"<"2"`。
字典序仅对非整数型 key（如 `"b","a","A"`）与 ≥2^32-1 的数字字符串 key 体现
（参见 vectors 用例 `array-indices-numeric-order`、`integer-like-keys-ordering`）。
Rust 侧序列化器必须复刻：先分离整数型 key（数值升序）再排其余 key（字典序）。

## 2. 存证链（evidence）

条目字段：`seq, prevHash, domain, collection, id, op('put'|'delete'), dataHash, payloadHash, metaHash, hash, timestamp, nodeId`

- `payloadHash` = sha256hex(normalizeObject(payload))；payload 为 null/undefined → null
- `metaHash` = sha256hex(normalizeObject(meta))；同上
- `dataHash` = sha256hex(normalizeObject({domain, collection, id, op, payloadHash, metaHash}))
- `entry.hash` = sha256hex(normalizeObject({seq, prevHash, domain, collection, id, op, dataHash, payloadHash, metaHash, timestamp, nodeId}))
- 链：`seq` 从 1 递增；`prevHash` 指向前一条目 hash，首条为 null
- 存储 key：`doc:evidence:proof:{seq 左补零至 12 位}`；头指针 `doc:evidence:head` = `{seq, hash}`
- 校验：从 1 遍历到 head.seq，逐条验 prevHash 与重算 hash

## 3. 集合策略注册表（schema）

- 策略：`append-only`（默认/治理强制）| `lww`
- 归一化（resolveSchemaDeclaration）：
  - syncStrategy 必须是两值之一，否则抛错
  - `governance=true` 且非 append-only → 抛错（禁止降级）
  - append-only → `enableEvidence` 强制 true；lww → 取声明值（默认 false）
- 未声明集合默认策略：`{append-only, governance:false, enableEvidence:true}`
- 存储 key：`doc:system:collection-schema:{encodeURIComponent(domain + "/" + collection)}`
- 集合名正则：`^[A-Za-z0-9_-]+$`
- 声明幂等：同策略重复声明返回既有记录；冲突声明抛错；**一旦声明不可变更**
- 同步消息携带的 schema 仅经 sanitizeSchemaHint 合法化后作**瞬时兜底**（本地未声明时），
  **永不写入注册表**（防远端锁死/降级本地策略）
- sanitizeSchemaHint：syncStrategy 非法 → undefined；governance=true 且非 append-only → undefined

## 4. 版本向量与 LWW（sync）

- meta：`{vv: {nodeId: counter}, ts: number, nodeId?}`，key `meta:{domain}:{collection}:{id}`
- `compareVersionVectors(local, remote)` → 'local'|'remote'|'concurrent'|'equal'
  （逐 key 取大比较；双null → 'equal'）
- `resolveConflictByLWW(localTs, remoteTs)`：null 按 0；`>` 严格比较，相等 → 'equal'
- `mergeVersionVectors`：逐 nodeId 取 max（append-only 幂等去重后促进收敛）

### applyRemoteUpdate 流程

1. **purge 水位线拦截**：`remoteMeta.ts < 水位线` → 拒绝落地（防已清理数据回灌）
2. schema hint 仅 sanitize 后作兜底；解析生效策略
3. append-only 分支：
   - 远端删除 → 拒绝（告警）
   - 本地无此 doc → 接受写入（doc + meta + 索引 + evidence[op=put]）
   - 本地已有且 `payloadHash(local) === payloadHash(remote)` → 幂等去重，合并 vv（取大）与 ts（取大），有变化才写 meta
   - 本地已有且载荷冲突 → 拒绝保留本地（告警）
4. lww 分支：
   - `cmp==='remote'`：落地远端（put：写 doc+索引 diff+meta+evidence；delete：删 doc+索引、写 tombstone meta `{vv,ts,tombstone:true}`+evidence[op=delete]）
   - `cmp==='local'` / `'equal'`：不动
   - `cmp==='concurrent'`：按 ts 裁决 LWW；remote 胜出走同上落地（含 evidence），否则不动
   - 注意：enableEvidence 的 lww 集合，cmp=remote 与 concurrent-remote 两分支都写 evidence（已修齐）

## 5. 验收向量（vectors/sync-evidence.json）

1. normalizeObject：嵌套对象/数组/null/undefined/数字/中文串 → 精确输出字符串
2. payloadHash/dataHash/entryHash 固定输入 → 固定 hash（≥3 组）
3. 三条目链式构建 → seq/prevHash/hash 链精确匹配
4. compareVersionVectors 全分支（local/remote/concurrent/equal/双null）
5. resolveSchemaDeclaration 全分支（含抛错用例）

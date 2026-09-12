# p2p-org-recovery

## 8. 直连协议 `/spark/org-recovery/1.0.0`（org-recovery.ts）

### 8.1 恢复 token（org-recovery.ts:33-41、constants.ts:110）

```
timeBucket = floor(nowMs / 600000)                      // 10 分钟桶，十进制整数（JS number → string，无前导零）
token      = sha256hex(`${orgId}:${recoverySecret}:${timeBucket}`)
```

- 输入字节 = 上述**冒号拼接**字符串的 UTF-8（注意分隔符是 `:`，orgId 形如 `org_<16hex>`，recoverySecret 为 64 hex）
- 输出：sha256 的 **hex**（64 字符小写）
- 有效 token 集合 = 当前桶 + 上一桶两个 token（消除桶边界漏配）；发起查询时用当前桶 token

### 8.2 消息格式

- 请求（org-recovery.ts:143）：`{"type":"org-recovery-query","token":<64hex>,"ttl":<int>,"want":<int>}`
- 响应：`{"ok":bool,"type":"org-recovery-response","peers":[{"peerId"?:string,"addresses":string[]}], "reason"?:string}`

### 8.3 响应侧（org-recovery.ts:59-116）

1. 读请求超时 3000ms；type/token 校验（token 必须匹配 `^[0-9a-f]{64}$`），不符回 `ok:false`
2. 同一请求方服务间隔 ≥ 30s（`RECOVERY_QUERY_MIN_INTERVAL_MS`，constants.ts:135），命中回 `ok:false, reason:"rate-limited"`
3. want 归一：缺省/非法 → 8，上限 8（`RECOVERY_QUERY_WANT`，constants.ts:130）
4. 命中：遍历本机恢复视图（当前身份为成员的组织，见 [org-recovery](../org/org-recovery.md) §10），
   token ∈ activeRecoveryTokens 即返回该组织 `memberNodeInfos` 前 want 条（仅含有地址的成员）
5. 未命中且 `min(max(0,ttl), RECOVERY_TTL=2) > 0`：向**除请求方外**的已连接邻居取前 2 个，
   以 `ttl-1` 转发查询，结果按 peerId 去重合并地址后截断到 want；ttl≤0 回空

### 8.4 请求侧（org-recovery.ts:119-159、p2p-node.ts:453-504）

- 触发条件（keepalive 内）：组织"全员不可达"连续 3 个 tick（`RECOVERY_TRIGGER_CONSECUTIVE_TICKS`，constants.ts:125），
  且距上轮查询 ≥ 10 min（`RECOVERY_COOLDOWN_MS`，constants.ts:120；**冷却为全局单值，非每组织**）
- 每轮：恢复视图前 3 个组织 × 已连接邻居前 3 个，ttl=2、want=8；读响应超时 3000ms
- 候选过滤：peerId 或地址须存在，地址滤空截 20；合并后最多取 16（`RECOVERY_QUERY_WANT*2`）；
  每轮最多拨号 4 个候选；命中只拨号，**不写组织成员表**（组织校验仍走 pull/claim 链路）

# p2p-neighbors

## 10. 邻居记录

### 10.1 覆盖网邻居池 OverlayPeerRecord（overlay-peer-store.ts:20-29）

存储键 `p2p:overlay:peer:<peerId>`（constants.ts:35），值 JSON：

```
{ peerId, addresses: string[], firstSeenAt: ms, lastSeenAt: ms,
  source: 'connect'|'exchange'|'announce'|'org'|'mdns',
  verified: boolean,                 // announce 验签通过即 true；只升不降（sticky）
  lastDialResult?: 'success'|'failure',
  addrMeta?: { [addr: string]: AddrScore } }   // M9 可选附加字段
```

`AddrScore`（M9 地址记分卡，connection-policy M9）：
```
{ successCount: number, lastSuccessAt: ms|null, validCount: number, lastValidAt: ms|null }
```

- 合并规则（:84-108）：按 peerId 合并地址（去重、trim、滤空，每 peer 截 20 条 `MAX_ADDRESSES_PER_PEER`）；
  每次 remember 刷新 lastSeenAt；firstSeenAt 保留首值
- 容量 200（`OVERLAY_POOL_MAX`，constants.ts:40）；超限淘汰：未验证者优先淘汰（同组内最久未见先走），
  全部已验证时才淘汰最久未见的验证条目；拨号失败不触发淘汰（:162-180）
- **线形兼容（M9 红线）**：`addresses: Vec<String>` 保持原样不动（FriendRecord/邻居池经 pdsync 同步，
  协议守护）；`addrMeta` 为可选附加字段，缺省零分自然迁入旧数据。pdsync 对端为旧版本时转发/覆盖会丢
  `addrMeta`（serde 忽略未知字段后重写）——接受该降级，零分重建即可
- 拨号抽样（:145-156）：排除给定 peerId 集，verified 优先、其余按 lastSeenAt 降序
- 覆盖网拨号目标：活跃连接 < 4（`OVERLAY_DIAL_TARGET`）时补拨，每 tick 预算 2 次
  （`OVERLAY_TICK_DIAL_BUDGET`，constants.ts:45-50；p2p-node.ts:301-338）

### 10.2 节点活跃度 PeerActivityRecord（p2p/types.ts:60-73、peer-activity-store.ts）

存储键 `p2p:peer:record:<peerId>`（constants.ts:30），值 JSON：

```
{ peerId, addresses: string[],
  firstSeenAt, lastSeenAt, lastConnectedAt: ms|null, lastDisconnectedAt: ms|null,
  successCount, failureCount, consecutiveFailureCount?: number,
  cumulativeConnectedMs, currentSessionConnectedAt?: ms, lastError?: string }
```

- `rememberNodeInfo(result)`：'seen' 仅刷地址与 lastSeenAt；'success' 累计 successCount、
  置 lastConnectedAt、清零 consecutiveFailureCount；'failure' 累计 failureCount、
  consecutiveFailureCount+1（旧数据缺省时的基线：successCount==0 ? failureCount : 0）、记录 lastError
- 清除（:17、131-142）：`consecutiveFailureCount ≥ 10` 且"完全不活跃"
  （successCount==0 && cumulativeConnectedMs==0 && 无当前会话 && lastConnectedAt==null）时整条删除
- 连接结算：markConnected 记 currentSessionConnectedAt（幂等）；markDisconnected 把会话时长累入
  cumulativeConnectedMs 并置 lastDisconnectedAt
- **打分公式**（computePriority，:201-204）：
  ```
  priority = cumulativeConnectedMs + successCount*60000 - failureCount*30000 - max(0, now - lastSeenAt)
  ```
  无记录的候选按 `Number.MIN_SAFE_INTEGER` 处理（排最后）
- 到期清理：lastSeenAt 超 90 天删除（data-management/constants.ts:14、cleanup.ts:65-77）

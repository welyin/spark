# org-recovery
> token 算法与协议帧见 p2p-org-recovery。
## 10. org-recovery 流程

- token 算法与协议帧见 [p2p-org-recovery](../p2p/p2p-org-recovery.md) §8
- 恢复视图 `getRecoveryView`（service.ts:158-197）：当前用户为成员的每个组织一条
  `{ orgId, recoverySecret, memberNodeInfos }`（仅含 addresses 非空的成员 nodeInfo）
- 存量组织缺 recoverySecret 时由 **admin 惰性补齐**（随机 64 hex，bump updatedAt 后落库，
  经反熵扩散；非成员角色本轮跳过等待 gossip，service.ts:173-186）
- 触发与拨号口径见 [p2p-org-recovery](../p2p/p2p-org-recovery.md) §8.4；命中候选只拨号不写成员表

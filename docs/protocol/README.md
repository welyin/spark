# 协议规格（docs/protocol）

> **已定稿**的线形规格：wire format、存储键定义、签名/信封格式、golden vectors 登记。
> 设计动机与方案选型归 [../architecture/](../architecture/)；协议过程稿、known-issues、cosign 联签记录留在 wiki `protocol/` 区。
> 归属裁决见 [../README.md](../README.md) §三。
> 内部结构对齐 product 分层：foundation（身份/存证/数据/网络）→ community（组织与成员凭证）→ affairs（公共事务）→ plugins（插件分发）。

## foundation/

- [foundation/identity.md](foundation/identity.md) —— 助记词与派生路径、SLIP-0010、rootId、域身份、v2/v1 备份加解密、用户资料
- [foundation/sync-evidence.md](foundation/sync-evidence.md) —— canonical JSON、存证链条目与链式规则、集合策略注册表、版本向量与 LWW 裁决、存证锚定与导出包
- [foundation/data-mgmt.md](foundation/data-mgmt.md) —— 数据分级常量、用量统计、L1 过期清理、L2 手动 purge、purge 水位线、全库导出

### foundation/p2p/

- [foundation/p2p/p2p-envelope.md](foundation/p2p/p2p-envelope.md) —— pubsub 信封（P2PMessageBody）与签名规则
- [foundation/p2p/p2p-netstack.md](foundation/p2p/p2p-netstack.md) —— 网络栈、gossipsub topic、帧约定、版本探测、keepalive 与连接策略
- [foundation/p2p/p2p-node-announce.md](foundation/p2p/p2p-node-announce.md) —— node-announce 节点通告
- [foundation/p2p/p2p-peer-exchange.md](foundation/p2p/p2p-peer-exchange.md) —— peer-exchange 直连协议
- [foundation/p2p/p2p-org-recovery.md](foundation/p2p/p2p-org-recovery.md) —— org-recovery 直连协议（组织失联恢复查询）
- [foundation/p2p/p2p-neighbors.md](foundation/p2p/p2p-neighbors.md) —— 邻居记录与活跃度打分
- [foundation/p2p/p2p-dht.md](foundation/p2p/p2p-dht.md) —— DHT 三件套：公共 DHT 承载、组织级私有 DHT、自认证组织地址记录的 DHT/gossip 承载
- [foundation/p2p/p2p-dm.md](foundation/p2p/p2p-dm.md) —— dm 直连协议 `/spark/dm/1.0.0`（1:1 消息、好友请求、设备通知、限流豁免）
- [foundation/p2p/p2p-app-messages.md](foundation/p2p/p2p-app-messages.md) —— 应用消息（服务号模型，本地协议不走网络）
- [foundation/p2p/p2p-org-mail.md](foundation/p2p/p2p-org-mail.md) —— org-mail 直连协议 `/spark/org-mail/1.0.0`（跨组织网关邮箱）
- [foundation/p2p/personal-data-sync.md](foundation/p2p/personal-data-sync.md) —— pdsync 个人域自设备同步（向量时钟、M3 epoch 选择性密钥轮换）

## community/（组织与成员凭证）

- [community/credential.md](community/credential.md) —— 资格凭证 schema、注销列表（append-only 签名日志）、验证人信任声明
- [community/read-gate.md](community/read-gate.md) —— 读授权门禁：readPolicy 扩展、orgq-req 凭证呈现段、验证链
- [community/policy.md](community/policy.md) —— 策略引擎 B1 最小声明式规则集、名册开放声明、准入策略声明

### community/org/

- [community/org/org-invite.md](community/org/org-invite.md) —— 邀请码编码与解析校验
- [community/org/org-address.md](community/org/org-address.md) —— orgSecret、网关、组织根密钥对与自认证地址记录
- [community/org/org-node-card.md](community/org/org-node-card.md) —— 节点名片线形（手动恢复连接）
- [community/org/org-recovery.md](community/org/org-recovery.md) —— org-recovery 组织侧流程（恢复视图、recoverySecret 惰性补齐）

## affairs/（公共事务）

- [affairs/affair.md](affairs/affair.md) —— affair 事务容器：创世记录、操作日志 DAG、集体决策机制、决议产物、时间语义、存储键
- [affairs/affair-sync.md](affairs/affair-sync.md) —— 事务复制面：关注者反熵（affairsync-hello/need/data）与逐条验签
- [affairs/affair-metadata.md](affairs/affair-metadata.md) —— 议题元数据面：`spark-affair-meta` gossip、公告线形与修订链验证、indexer 收录

## plugins/（插件分发）

- [plugins/plugin-dist.md](plugins/plugin-dist.md) —— 插件分发主规格：仓库地址 id、spark-plugin.json、URL 模板与镜像展开、双源交叉验证、安装流程
- [plugins/plugin-announce.md](plugins/plugin-announce.md) —— 广播索引：plugin-announce topic、PoW/TTL、relay 资历制、懒惰核查

# p2p-dm

## 19. 直连协议 `/spark/dm/1.0.0`（1:1 消息与好友请求）

应用层 1:1 投递通道：聊天消息、已读回执、撤回通知、好友申请、申请通过与
申请回复、资料同步、组织邀请与邀请应答、成员移除通知。
request-response 模式，帧约定同 §4（写一帧 JSON、读一帧 JSON、无长度前缀），
协议读超时 10 s（`DM_READ_TIMEOUT_MS`），命令侧外层超时 15 s（略大于一次完整
逐地址尝试量级：协议 10 s + 拨号余量）。入站按 peer 间隔限流
（`DM_MIN_INTERVAL_MS` = 1 s，命中回 `rate-limited`）：**控制类信封
（read/recall/friend-accept）与同步/传输类信封豁免**，内容类
（chat/friend-request/friend-reply/profile-sync/org-invite/
feed）计入限流（避免「发消息+已读回执」连发被误伤）。完整豁免清单见下：控制类
read/recall/friend-accept；组织控制类（阶段四A P2/P3 新增，低频控制面信封，
friend-accept 同族先例）org-invite-reply/org-member-removed——join 编排
（reply + orgsync-hello 连发）与紧随的 orgq-req 共享同一 1 s 桶会被确定性
误限流；自设备同步类 pdsync-hello/need/data/
attachment-req/attachment-resp/contact-sync/conv-sync/profile-sync/
device-sync；组织同步类 orgsync-hello/need/data（~~orgkey-deliver~~
已随 C7 encrypted 轴退役删除）；
事务复制面反熵类（C4，[affair-sync](../community/affair-sync.md) §2）affairsync-hello/need/data——
关注者反熵 hello→need→多批 data 与 orgsync 同构，豁免口径同族；
**orgq 应答 orgq-resp**（P3 起豁免——应答由请求方主动拉起，洪泛面由
orgq-req 侧限流守住，orgq-req 不豁免口径不动）；
**跨联系人 blob 分块传输类 feed-blob-req/feed-blob-resp**（多信封往返的
数据传输通道，豁免避免被 1 s 桶确定性误限流）；限流器容量上限 1024 条
（满时回收过期条目，仍满则整体清空，防内存无界）。
> 修正：`pdsync-attachment-req/resp` 此前未在豁免清单内，属遗漏，本版补登；
> `feed-blob-req/resp` 随 §19.7 新增并豁免。
> 修正（阶段四A P2/P3）：`org-invite-reply`/`org-member-removed`/`orgq-resp`
> 三 kind 新增入豁免清单（理由见上），此前按内容类计入限流。

入站处理（验签/落库）spawn 出
swarm 事件循环线程执行，应答经完成通道送回后再 send_response——存储 IO
不再阻塞 ping/gossip/其它协议。Rust 实现：
`core/src/p2p/node/dm.rs`（传输）、`core/src/kernel/dm_envelope.rs`（信封）、
`core/src/kernel/inbound_dm.rs`（入站编排）。

注意：dm 与 org-share 两个 request-response behaviour 的 `OutboundRequestId`
各自从 1 递增会互相碰撞，应答/失败必须按协议来源（attempt kind）隔离匹配
（`core/src/p2p/node/swarm_events.rs`），否则「边同步边聊天」时 org ack 会被
当成 dm 应答。

### 19.1 信封（应用层，透明承载于请求帧）

```json
{ "kind": "chat|read|recall|friend-request|friend-accept|friend-reply|profile-sync|system/device-notice|org-invite|org-invite-reply|org-member-removed|feed|feed-blob-req|feed-blob-resp",
  "from": "<rootId>", "to": "<rootId>", "ts": 1720000000000,
  "body": { "...": "按 kind 定义；E2E 加密时见 19.1.1" },
  "pubKey": "<base64>", "sig": "<base64>" }
```

- 签名载荷 = **固定键序 body/from/kind/to/ts 的紧凑 JSON 串**，签其 UTF-8 字节
  （Ed25519 Pure，根身份密钥；与 §3.3 的临时自证密钥不同，**dm 签名绑定根身份**）。
  跨端对照基准见 golden vector：`code/spec/vectors/dm_envelope.json`
  （测试 `core/tests/dm_envelope_vectors.rs`）。
- `pubKey` = 根身份 ed25519 公钥原始 32 字节的 base64（与 `verify_ed25519_signature` 口径一致）；
  入站必须满足 `sha256hex(pubKey) == from`（rootId 定义），否则拒收。
- `ts` 参与签名且入站校验时效：`ts <= 0` 一律拒收；`|ts - now| > 10 min`
  （`ENVELOPE_TS_WINDOW_MS`，饱和算术比较，极端值不溢出）拒收
  （reason `stale`）——防重放（重放已撤回消息的原始 chat 信封会被窗口拦下；
  窗口内的同 id 重放再由 §19.2 chat 的按消息 id 去重兜底）。
- 入站校验顺序：字段齐全 → `to` 指向本机 → ts 时效 → pubKey 与 from 绑定 → 验签。

#### 19.1.1 信封 body E2E 加密（全 kind 统一，已落地）

DM 信封 body 加密为密文，线形（对任一 kind 的 body 统一替换）：

```json
{ "kind": "chat|feed|...",
  "from": "<rootId>", "to": "<rootId>", "ts": 1720000000000,
  "body": { "encrypted": true, "ciphertext": "<base64>", "nonce": "<base64>" },
  "ephPub": "<可选，base64 临时 X25519 公钥，见下方轮换>",
  "pubKey": "<base64>", "sig": "<base64>" }
```

- **算法**：AES-256-GCM；密钥 = X25519 DH 共享密钥派生。**方向无关**
  （2026-08-11 架构师裁决）：共享密钥经 HKDF-SHA256 派生 32B AES-256
  会话密钥，**info 构造**为 `spark-dm-e2e-v1:{min}:{max}`——`from`/`to`
  按**字典序排序**（小在前）后拼接，故 A→B 与 B→A 派生**同一份**会话密钥，
  会话密钥表每对 peer 一条记录、不区分方向。HKDF salt 取全零 32B
  （RFC 5869 默认无 salt）。X25519 DH 复用 orgsync 的
  `ed_pk_to_x25519` / `ed_sk_to_x25519` 基础设施（**root 密钥直接转换**，
  2026-08-11 架构师裁决：发送方用接收方 **root 公钥**（信封 `pubKey`
  字段，`ed_pk_to_x25519`）+ 自己 **root 私钥**派生共享密钥。不使用域身份
  派生——dm-e2e 域身份导致对端公钥不可从 root 公钥推导，root 直接转换零
  协议变更、存量好友立即可用）。对端 root 公钥来源：入站验签时信封
  `pubKey` 顺手记入密钥表 `peerRootPub`；出站读取该字段做 X25519 转换，
  无记录视为内部错误（不静默降级明文）。
- **签名载荷**：签名对**线形 body**（即上面的密文对象）做。**键序**：
  无 `ephPub` 时为 body/from/kind/to/ts；**携带 `ephPub` 时插在 body 之后**
  ——body/ephPub/from/kind/to/ts（`ephPub` 参与签名，防中间人替换临时
  公钥）。接收方**先验签后解密**（保持既有入站校验顺序，验签不依赖解密，
  防篡改 + 防重放；解密由接收方以本地会话密钥完成）。

##### 长期会话密钥恒等不轮换：前向保密由 per-message 临时密钥承担（2026-08-11 架构师裁决 + 二轮修订）

root DH（我方 root 私钥 × 对端 root 公钥 X25519）是**确定性**的——无论何时
重跑派生都得到**同一份**长期会话密钥。因此：

- **长期会话密钥恒等（identity-stable），不轮换**。所谓「24h 重协商」若只重跑
  root DH，产出的是同一份密钥，旧 current 移入 history 只是**同值堆积**，不产生
  新密钥、无任何安全增益；
- **前向保密由 per-message 临时密钥承担**：每条消息发送方生成独立临时 X25519
  密钥对，临时公钥随信封 `ephPub` 携带（base64，32 字节，与 `pubKey`/`sig`
  并列且**参与签名**，见上键序——防中间人替换临时公钥后注入已知会话）。本次
  消息用「我方临时私钥 × 对端 root 公钥 X25519」派生**本次独立的瞬时会话密钥**
  加密；消息之间密钥互不相关——泄露任一消息的瞬时会话密钥只影响该条，不影响
  其它消息。

**临时交换派生**（双方均已升级到临时交换，root 密钥直接转换）：

- 发送方生成临时 X25519 密钥对，临时公钥随信封 `ephPub` 携带；
- 发送方用「**我方临时私钥** + 对端 root 公钥 X25519」派生；
- 接收方用「**我方 root 私钥** + 对端 `ephPub`」派生；
- 二者 DH 相等（X25519 DH 交换性），得到同一份本次瞬时会话密钥。

**回退规则（定死）**：本期发送方**总是**生成临时密钥对并携带 `ephPub`。
接收方总是用「我方 root 私钥 + 对端 `ephPub`」派生。仅当**对端未升级**
（信封无 `ephPub`，旧对端仍走 root 直接转换 DH）时，发送方回退为「我方
root 私钥 + 对端 root 公钥」的 root 直接转换 DH——此时**无前向保密但保持
跨版本兼容**（本期强制加密不降级，故对端虽旧、仍能解密，只是本次密钥非
临时）。两种路径产出的会话密钥都写入同一份会话密钥表（每对 peer 一条，
方向无关）。

**`lastSeen` 语义（诚实化修订）**：会话密钥表字段 `lastSeen` 记录**上次
写入会话密钥表的时间**（密钥协商 / 对端 root 公钥积累 / 记录刷新等写盘
动作），**不代表**「上次成功加密/解密」——加解密路径不刷新它（避免每次
通讯高频写盘）。协议**不再**以 `lastSeen` 触发任何安全相关的密钥轮换：
长期密钥恒等，轮换无意义。`historyKeys`（容量上限 **8**，`HISTORY_KEY_CAP`）
仅作**离线密文时段簿记**保留；因长期密钥恒等，历史条目为同值，离线密文
始终可用任一历史/当前条目解密。

**实现侧待办（报回协调者）**：① `dm_e2e::ensure_session_key` 的 24h
（`KEY_REFRESH_MS`）轮换分支产出的历史密钥为同值、且推进 `currentSince`/
`lastSeen`——建议去除该空转分支，长期密钥协商一次后原样复用（写盘仅需保留
`currentKey`，`historyKeys` 可不写或写同值）；② 相应简化 `historyKeys` 簿记
（不影响解密正确性，纯存储冗余收敛）。

- **个人设备间共享**：同一用户多个个人设备经 pdsync 共享同一份 1:1
  会话密钥（含历史密钥，供多设备解密各自时段离线密文）；
- **向后兼容**：旧版本对端收到加密信封回 `unknown-kind` 或 `invalid-body`，
  发送方可选择降级为明文——**第一期强制加密不降级**（避免中间人降级攻击，
  见 social-feed §11 已决）；
- golden vectors：`code/spec/vectors/dm_envelope.json` 第 2 条（E2E 加密信封
  线形 + 签名字节）与新增第 6 条（携带 `ephPub` 的轮换信封，锁定
  body/ephPub/from/kind/to/ts 键序 + 签名字节）；HKDF 方向无关派生命令见
  `code/spec/vectors/dm_e2e.json`。

### 19.2 各 kind 的 body 与语义

| kind | body | 接收侧行为 |
|---|---|---|
| `chat` | `{ spaceKey, message }`（message 为完整消息记录） | 拉黑则拒（`blocked`）；组织空间校验发送方是组织成员（`not-member`）；会话 `dm:{from}` 幂等建立、消息落库、未读+1、推送 ChatReceived 事件。**安全约束**：① senderId 强制绑定信封 `from`（忽略对端自报值，防伪造渲染成「我」）；② 按消息 id 幂等去重（`msg:byid:` 索引直取，索引缺失回退扫描）——同 id 重投/重放直接回 `ok:true`，不重复落库、未读不重复 +1、不覆盖已投递/已撤回内容；③ `created_at > now + 10 min` 或 `created_at <= 0` 拒收（`invalid-message`，远未来消息把会话钉在列表顶部、负值破坏存储键序）；④ 文本正文超 16 KiB（`MAX_TEXT_BYTES`，UTF-8 字节，与 1 MiB 帧上限配套）拒收（`invalid-message`）；⑤ `message.link`（链接预览，对端自报字段）入站与出站同口径收敛：五字段 trim 后按字符限长截断（url 2048 / title 256 / description 512 / siteName 64 / domain 253），url 为空或非 http(s) scheme 整条丢弃（`javascript:`/`data:` 等 scheme 是渲染面注入向量）——接收方只展示卡片、不访问链接（抓取只在发送方壳层做，见 ui-messages.md §6）；⑥ **隐含确认**：个人空间收到非朋友消息且我有指向 from 的 pending 出站申请 → outbox 置 accepted + `merge_friend_record` 合并式建朋友 + FriendRequestAccepted 事件，消息照常落库（friend-accept 回执丢失时由真实聊天行为兜底确认） |
| `read` | `{ spaceKey }` | 拉黑则拒；组织空间校验发送方是组织成员（`not-member`）；把该会话中自己发出的 sent/delivered 消息置 read，推送 ChatStatus(peerRead)；无实际改动（会话不存在/无可回执消息）时静默 `ok:true` 不发事件 |
| `recall` | `{ spaceKey, messageId }` | 拉黑则拒；组织空间校验发送方是组织成员（`not-member`）；仅当存储消息的 senderId == 信封 `from` 才置 recalled（归属校验，防对端撤回我方消息）；归属不匹配/消息不存在按幂等处理（`ok:true`，不发事件）。入站不判 2 分钟窗口，窗口由发送方本地约束 |
| `friend-request` | `{ requestId, nickname, message, source, avatar?, nodeInfo?{peerId,addresses} }` | 拉黑则拒；同 rootId pending 幂等更新（nickname/message/source 非空才覆盖、**peer 为 None 时保留原值**——对端重试不带 nodeInfo 不抹寻址信息）；落 `ct:req:in:`，推送 FriendRequestReceived；应答携 `nickname`。**申请 id 为复合形式 `{from}:{requestId}`**（存储键带发送者命名空间，两个发送者同毫秒撞 id 不再互相覆盖）；事件与 overview 暴露给前端的即复合 id；幂等更新只刷 `updatedAt`、保留首次到达的 `createdAt`。**from 已是朋友时不产生申请**：直接回发 friend-accept 重确认（防 accept 回执丢失卡 pending），对端自报的 avatar 过 `validate_avatar` 才采纳进朋友记录 |
| `friend-accept` | `{ requestId, nickname, avatar?, nodeInfo? }` | **先校验**：已拉黑回 `blocked`；出站申请存在 && 仍为 pending/replied（对方已回复询问的 replied 态同样可接受）&& `record.rootId == from`，三者任一不满足回 `invalid-body`、不改状态不建朋友。**requestId 复合 id 归一化**：接受方回发前先 strip 本地 inbox 复合 id 的 `{from}:` 前缀还原原始 id；接收侧同样 strip 一次以兼容旧对端直接回传复合 id。通过后 outbox 置 accepted、**合并式**建朋友（`merge_friend_record`）：已有 FriendRecord 保留本地资料（备注/标签/分组/照片/addedAt/permission），仅刷新非空 nickname、过 `validate_avatar` 的 avatar 与 Some 的 peer；推送 FriendRequestAccepted |
| `friend-reply` | `{ requestId, text }` | 好友申请的来回回复（接收方询问/申请方回答同 kind；text trim 后为空或超 16 KiB 回 `invalid-body`）；拉黑回 `blocked`；接收侧按本端记录匹配方向：outbox 命中（`rootId == from` 且 pending/replied → thread 追加 + 置 replied + 事件 FriendRequestSent）或 inbox 复合 id `{from}:{requestId}` 命中（pending → thread 追加、status 不变 + 事件 FriendRequestReceived），皆不命中回 `invalid-body`；计入限流（内容型，与 friend-request 同档）。**已知边界**：① body 无消息 id，窗口内重放会产生重复 thread 条目（status 流转本身幂等）；② thread 条数上限 100 条（超出丢弃最旧——thread 是跨消息累积的对端可控存储面，按 peer 限流只能减速不能封顶）。**出站命令**：接收方主动发起询问 `contact_ask_request`（inbox 复合 id 查记录、要求 pending、thread 追加 from=me 后 status 保持 pending、信封 requestId 去 `{from}:` 前缀还原原始 id）、申请方答复 `contact_reply_request`（outbox、要求 replied、thread 追加 from=me 后 status 回 pending），投递均带退避重试（`DM_RETRY_DELAYS`） |
| `profile-sync` | `{ nickname, avatar? }` | 资料变更广播：资料写路径触发向全部朋友广播 + 朋友建连时主动推送。接收侧：非朋友静默 `ok:true`；nickname 非空才覆盖、avatar 过 `validate_avatar` 才采纳；无变化静默不发事件；有变化 upsert 朋友记录 + 推送 FriendProfileUpdated（`{rootId, nickname, avatar?}`）；计入限流 |
| `org-invite` | `{ inviteId, inviteCode, orgId, orgName, orgDescription?, orgAvatar?, inviterNickname, inviterAvatar? }` | 组织邀请（流程见 `org-join`（待 A53 改写，wiki protocol/org/）§6）：自邀请（from==本机）回 `invalid-body`；拉黑回 `blocked`；inviteId/inviteCode/orgId/orgName/inviterNickname 任一缺失回 `invalid-body`；幂等 upsert 入站邀请记录 `org:inv:in:{orgId}:{peerRootId}`（终态不重置）→ personal 系统会话 `sys:notice` 追加链接卡片（url `spark-org-invite://{inviteId}`，按消息 id 去重）→ 未读+1 + ChatReceived + OrgInviteReceived 事件；展示字段均为对端自报，成员资格校验在 accept 拉取侧完成；计入限流 |
| `org-invite-reply` | `{ inviteId, orgId, accept, nickname, avatar? }` | 邀请应答：拉黑回 `blocked`；orgId 缺失或 accept 非 bool 回 `invalid-body`；出站邀请记录存在且 pending → 置终态（accepted|declined，**终态不可逆**）→ OrgInviteUpdated（完整 `OrgInviteRecord`）事件；记录不存在或已终态静默 `ok:true`；**限流豁免**（P2/P3 起入组织控制类豁免，见 §19 首部清单） |
| `org-member-removed` | `{ orgId, targetPeerIds }` | 成员移除定向通知（admin → 被移除成员，阶段四A P2 新增；完整线形与触发/兜底见 `org-join` §7（待 A53 改写，wiki protocol/org/））：`targetPeerIds` 是**发送侧补投自用的寻址快照**（被移除者出成员表后 flush 反查用），收端不消费。拉黑回 `blocked`；orgId 缺失/非字符串回 `invalid-body`；本地仍持记录时发送方须为装配视图中的 **admin**，否则回 `rejected` 不擦除；本地已无记录幂等 `ok:true`；通过即本地擦除该组织（whole + 成员条目 + orgq 现场）+ OrgRemoved（`{"orgId"}`）事件；**限流豁免**（P2/P3 起入组织控制类豁免，见 §19 首部清单）。**老版本盲区**：未实现本 kind 的节点回 `unknown-kind`，移除传播由成员条目墓碑 + whole 双写经 orgsync 收敛兜底（P3 前另有 legacy org-pull `removed` 分支同口径擦除） |
| `system/device-notice` | `{ kind: "device_joined", deviceId: string, deviceName: string, ts: number }` | 新设备加入通知（契约 `device-revocation-and-recovery` §2.1（wiki architecture/identity/device-revocation-and-recovery-contracts.md））：新设备配对确认后向全部已知自设备广播（信封 `from == to == 本机 rootId`），另在「新设备窗口」（本地标记，24 h）内与自设备建连时定向补发；按 `deviceId` 幂等（前端 store 去重）。**接收侧零副作用**：不改任何授权/连接状态、不入消息库、不进 pdsync——仅校验 `from == 本机 rootId` 且 `body.kind == "device_joined"` 且 `deviceId` 非空后推送 `DeviceNoticeReceived` 事件（本地通知 + 设备页红点）。`deviceId` 为发送方 peerId。**best-effort**：通知不补投，离线设备收不到；设备清单本身经 device-sync/pdsync 反熵可达，通知只负责「提醒」。**老版本盲区**：未实现本 kind 的节点收到后回 `unknown-kind`，不展示红点/通知，无其他影响。**撤销语义不闭拢提示**：设备撤销（§19.5 `revokedAt` + 连接层黑名单）的语义在**全端升级前不闭拢**——未识别 `revokedAt` 的旧版本节点黑名单不生效，且其外发记录无粘性保护；撤销保障以全端升级为前提 |
| `system/recovery` | 三态自设备延迟恢复信封（契约 `device-revocation-and-recovery` §3（wiki architecture/identity/device-revocation-and-recovery-contracts.md），实施设计 `m4-m5-mobile-plan` §4.2（wiki architecture/identity/））：<br>`initiated`：`{ kind:"initiated", op, requestId, deadline, fromDevice }`（`op ∈ {reset_password, pair_new_device}`，`deadline` 为发起方本地时钟毫秒）<br>`vetoed`：`{ kind:"vetoed", requestId, fromDevice }`<br>`committed`：`{ kind:"committed", op, requestId, fromDevice }` | M5 延迟恢复通道，信封 `from == to == 本身份 rootId`（自设备管道）；`fromDevice` 为发送方 peerId 标注，仅用于展示/日志。接收侧：**veto 采 fail-safe**——`vetoed` 只要 `requestId` 非空即无条件落墓碑（乱序兜底），幂等；**initiated/committed 采 fail-closed**——四字段齐 + `op` 合法 + `deadline` 正数才生效，未知 `requestId` 的 `committed` 静默 `ok:true`。`initiated` 接收方以本地到达时间 + `(deadline - 信封 ts)` 独立计算否决窗（clamp 到 (0, 7d]），不信发起方单一时钟。三条消息不加限流豁免（与 device-notice 同口径，全生命周期各发一次，1s 桶无冲突）。**best-effort**：发起/否决/生效广播均不补投，离线设备错过 veto 无妨——发起方 confirm 只信本机收到的 veto。**老版本盲区**：未实现本 kind 的节点收到后回 `unknown-kind`，无否决能力也无展示，全端升级前 M5 语义不闭拢（与 M1/M2 同型） |

`spaceKey`：白名单校验，仅接受 `'personal'` 或 `'org:org_<16hex>'`（小写十六进制
orgId）；非法值回 `invalid-body`——冒号注入的 `"personal:x"` 之类值既不能绕过
成员/拉黑校验，也不会落进 personal 会话前缀扫描范围。

拉黑与 FriendRecord 解耦：个人空间用独立拉黑集合（`ct:blocked:{rootId}`，
陌生人也可拉黑，删除朋友不解除拉黑）；组织空间拉黑记在成员附加资料的
`blocked` 字段。

### 19.3 应答线形

成功 `{"ok":true}`（friend-request 附 `"nickname"`）；失败
`{"ok":false,"reason":"<reason>"}`，reason 枚举：
`invalid-envelope / not-for-me / bad-pubkey / bad-signature / stale / blocked / not-member / invalid-body / invalid-message / unknown-kind / rate-limited / invalid-request / internal-error / dm not supported`（宿主未实现）。
其中 `invalid-request`（请求非合法 JSON）、`rate-limited`（限流命中）、
`internal-error`（宿主内部错误的统一掩码——含「无已解锁身份」等，原始错误
仅本地日志不外泄）由 p2p 传输层产生，不进入应用层。

发送侧状态机对齐 ui-messages §3.3：落库 `sending` 后 `message_send_text`
**立即返回**（投递 spawn 到后台任务，不阻塞命令与 kernel 锁）→ 应答 `ok:true`
置 `delivered`；`ok:false`/超时/不可达置 `failed`；`read` 回执置 `read`。
终态回写是 **compare-and-set**（仅当当前仍为 `sending` 才写），重发后旧投递
任务的迟到回写不会覆写新终态；`message_resend` 对 `failed`/`sending` 放行
（恢复崩溃卡死的消息），**拒绝已撤回消息**（防对端「复活」已撤回内容）。
终态经 `P2pEvent::ChatStatus`（`{spaceKey, convId, messageId, status}`）回写，
前端按事件更新。**离线暂存已落地**（social-feed §6.4，2026-08）：对端不可达即
密文落入 `dm:pending:` 队列自动补投（个人经 pdsync 同步到发件人自设备，任一台
在线设备上线后 flush），消息保持 `sending`，前端显示「对方离线，将在其上线后
送达」；补投成功置 `delivered`；`message_resend` 保留为手动兜底。网关转发
（§8.4 网关层）仍为后续专项。

**离线暂存队列（dm_offline）存储键线形**（`core/src/kernel/dm_offline/`，值
为待补投的**已加密完整 dm 信封** + `spaceKey`/`convId`/`messageId` 供补投成功
后 compare-and-set 回写）：

- 个人空间：`dm:pending:{toRootId}:{messageId}`（个人经 pdsync `dm:pending`
  category 扩散到发件人自设备，任一台在线设备上线后 flush 补投）；
- 组织空间：`org:dm:pending:{orgId}:{toRootId}:{messageId}`（org-sync 网关代收
  通道就绪后接入，本版先按个人空间同构落地）；
- 生命周期：TTL **7 天**（过期丢弃不补投）、单 recipient 上限 **100** 条、
  全局上限 **1000** 条（超出按 `createdAt` 升序淘汰最旧）；
- `messageId` 为明文（E2E 加密后信封 body 是密文，不能从 body 提取）——chat 用
  消息 id、feed 用 `feed:{feedId}` 区分同一 recipient 的多条 pending，避免互相覆盖。

好友申请同样异步化：`contact_send_request` 落库 pending 后**立即返回**，投递
spawn 后台任务——送达（应答 ok）回填对方 nickname，失败/不可达置 `failed`；
终态经 `P2pEvent::FriendRequestSent`（`{request}`）回传，前端按 id upsert。
重试用同一 requestId 再调 sendRequest 即可：内核识别 outbox 已有记录走重试
路径，**复用已存的 peer 寻址**（名片来源的申请重试不丢地址）、重置 pending。
申请记录带 `updatedAt`（内容更新/状态流转刷新，前端按它倒序）。

### 19.4 自己作为联系人（个人空间多端同步）

- 个人空间通讯录恒含 `rootId == 自己` 的联系人条目；给自己发消息即「同步到用户所有节点」。
- 设备配对：A 设备扫 B 设备的节点名片发送 `friend-request`（from==to==同一 rootId）；
  接收侧校验 `from == 自己` 后**自动接受**——直接落设备联系人记录（不产生申请），
  并向 `body.nodeInfo` 回发 `friend-accept`，双方互得设备记录（rootId==自己，peer=对端设备）。
- 自消息：本地落库即 `delivered`（本机副本天然送达），随后向全部设备记录逐个直发
  `chat`（from==to==自己）；接收侧正常验签落库到会话 `dm:{自己rootId}`，**不计未读**。
  线上信封的 `message.senderId` 恒为真实 rootId；`'me'`/`'我'` 只是内核事件与
  DTO 视图层的映射（ChatReceived 事件与水合列表同口径），不入线形。
- 设备离线时该设备投递失败不排队；离线设备恢复后的历史同步依赖后续个人空间同步机制。
- **设备清单模型已落地**（social-feed S4，2026-08）：`FriendRecord.peer: Option<PeerRef>`
  升级为 `peers: Vec<PeerRef>`（旧数据 `#[serde(from)]` 自动升级为单元素列表，幂等）；
  握手（friend-request/accept nodeInfo）写入首台、自设备经 DeviceRecord 聚合；
  `resolve_conv_peer` / `self_device_peers` / 全部 `.peer` 使用点遍历 `peers` 择优。
  nodeInfo 未扩列表（评估结论：握手时发起方仅一台设备，单 peer 已足，多设备由
  DeviceRecord 积累，非协议变更）。

### 19.5 设备记录（`DeviceRecord`）与撤销字段

`device-sync` 信封与本地 `device:` 存储使用同一 `DeviceRecord` 线形。字段规格：

```json
{
  "peerId": "<libp2p peerId>",
  "deviceUid": "<128bit hex；旧版本记录缺省>",
  "deviceName": "...",
  "os": "Windows|macOS|Linux|Android|iOS",
  "osVersion": "...",
  "arch": "x86_64|aarch64",
  "macs": ["..."],
  "appVersion": "0.2.1",
  "updatedAt": 1720000000000,
  "lastSeenAt": 1720000000000,
  "revokedAt?": 1720000000000
}
```

- 字段与 `core/src/device/mod.rs::DeviceRecord`（serde camelCase）一一对应；`deviceUid`/`appVersion`/`osVersion` 带 `serde(default)` 兼容旧记录。
- `revokedAt`（可选）：设备被撤销的 Unix 毫秒时间戳；缺省表示未撤销。
- **序列化兼容**：旧版 JSON 无 `revokedAt` 字段时必须能反序列化为 `None`；新增字段必须带 `serde(default)` / `skip_serializing_if = "Option::is_none"`。
- **撤销粘性（三条合入通道全部生效，`revokedAt` 只增不减）**：① `upsert_self`（本机重采集）保留本地已有 `revoked_at`；② `apply_remote`（device-sync DM 通道）本地已撤销而远端干净时合入值保留本地 `revoked_at`；③ **pdsync 反熵通道**（`pdsync-data` 对 `device:` 键）执行与 ② 等价的粘性合并，先于通用 `apply_personal_remote` 裁决——缺 ③ 时「被撤销设备的干净新记录经离线中间设备反熵回流」可洗掉知情方的撤销标记（裁决记录见 `m1-m2-implementation-plan` §5.4（wiki architecture/identity/））。
- **寻址过滤**：构造自设备拨号目标（`list_self_device_peer_infos`）时必须跳过 `revoked_at.is_some()` 的记录，已撤销设备不再作为候选节点。
- **连接层黑名单**：`revoked_at.is_some()` 的 peerId 在入站连接/入站 DM/入站 challenge/出站拨号四点被拒并即时断连；黑名单判定依据本地清单，传播延迟内（离线设备未学到标记前）旧连接存在窗口。
- 安全日志（`security:log:{ts}:{kind}`）本地 append-only，记录 `device_revoke_initiated` / `device_revoke_effective`，不进 pdsync。

### 19.6 feed 信封：社交定向投递

`feed` 把一份业务 payload 定向投递给 `to`（收件人 rootId），供社交类插件
（朋友圈/群聊/论坛）复用。body（加密前）线形：

```json
{ "topic": "{pluginId}:{sub}", "feedId": "<1-64 字符全局唯一 id>",
  "payload": "<任意 JSON，紧凑序列化后 ≤ 32 KiB>", "replyTo": "<可省，回执语义>" }
```

body 入站约束（违反回 `invalid-body`）：

- `topic`：字符串，`{pluginId}:{sub}` 形态，总长 ≤ 128，字符集
  `[a-z0-9:._-]`；出站侧校验前缀 == 调用方插件 id；
- `feedId`：字符串，1–64 字符，发送方生成的全局唯一 id；
- `payload`：任意 JSON 值，紧凑序列化后 ≤ 32 KiB；
- `replyTo`：可省字符串——互动/回执语义，指向原 `feedId`。

幂等与防重放：

- 信封级：`ts` ±10 min 窗口沿用（§19.1）；
- 应用级：接收方按 `(from, feedId)` 在收件箱内查重，重复投递去重；
- 插件侧幂等兜底：业务集合以内容 id（如朋友圈 postId）为键，重复投递天然覆盖。

限流口径：

- `feed` 计入 DM 入站按 peer 限流（1 s）；
- `feed-blob-req` / `feed-blob-resp` 豁免（§19.7 分块传输通道）。

### 19.7 feed-blob 拉取：跨联系人 blob 分块传输

`feed-blob-req/resp` 用于接收方按需拉取 feed 正文引用的二进制 blob
（缩略图/完整图），由 `feed` 原作者的 rootId 服务。线形对齐
`pdsync-attachment-req/resp`（线形见 `plugin-data-api` §4（wiki architecture/plugins/））：

```json
{ "kind": "feed-blob-req",
  "body": { "hash": "<blob sha256>", "offset": 0 } }
{ "kind": "feed-blob-resp",
  "body": { "hash": "<blob sha256>", "offset": 0, "data": "<base64>", "totalBytes": 10485760 } }
```

- 请求方：`readBlob(hash)` 本地未命中且该 hash 有 feed 来源记录 → 向来源
  rootId 发 `feed-blob-req {hash, offset}`，逐块拉取（offset = 已收字节数）；
- 服务方：入站校验 from 是本机朋友且本地确有该 hash → 分块应答
  `{hash, offset, data, totalBytes}`（data 为 base64，块长 3 的倍数可拼接）；
  缺本体回 `{hash, offset, missing: true}`（请求方节流后重试）；
- 接收方：按序追加，收齐做尺寸 + SHA-256 校验，通过提升为本体；
- 单 hash 体积上限 32 MiB；
- 幂等：以 `(from, hash, offset)` 幂等合块，乱序块不落；
- 鉴权：hash 即能力（256 bit 内容寻址不可枚举）；请求方必须是本机朋友；
- 限流：`feed-blob-req/resp` **豁免**入站按 peer 限流（多信封往返的数据
  传输通道，避免被 1 s 桶误限流），逐 hash 节流在应用层：**仅对首块**
  （`offset == 0`）限流——`BLOB_REQ_THROTTLE_MS` 窗口内同 hash 首块请求被
  跳过；续拉块（`offset > 0`）是一次活跃拉取的连续往返，**不限流**
  （防对端窗口内反复发起同 hash 首块拉取防洪）。请求方与服务方同口径。

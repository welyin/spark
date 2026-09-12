# Rust 单测与集成测试

> 本文档从 [testing.md](testing.md) 拆分出来，独立覆盖 Rust 侧模块单测与集成测试。

```bash
cd code/core && cargo test          # 测试全绿，数量以仓库实测为准
```

- 模块单测：identity / storage / schema / sync / evidence / collection / org / data-mgmt / p2p / contact / message 各模块内联单测（另有 `unit_*.rs` 聚合文件），含 p2p/plugin_announce（广播声明 codec/PoW/校验链/索引 store）；
- 集成测试（`code/core/tests/`，按域拆分为 `kernel_*.rs` / `p2p_*.rs` / `sync_apply_*.rs` 多文件）：
  - 身份与数据：`kernel_identity.rs`（身份全生命周期、备份与助记词恢复）、`kernel_docs.rs`（文档写入+存证+重启）、`kernel_purge.rs`（purge 守卫、用量与清理）；
  - 组织：`kernel_org.rs`（组织创建/副本概览）、`kernel_invite.rs`（邀请/claim，含真实 P2pNode 扮演邀请方的 accept_invite 全流程与双 kernel 互连对跑的组织同步编排）；
  - 网络：`kernel_network.rs` / `kernel_p2p.rs`（P2P 启停与事件）、`p2p_basic.rs` / `p2p_dht.rs`（真实协议栈多节点场景）；
  - 消息与通讯录（新链路）：`kernel_message.rs`（dm 发送失败落库/重发/撤回窗口/入站持久化）、`p2p_dm.rs`（**双节点 dm 投递与好友申请互推对跑**，真实 `/spark/dm/1.0.0` 直连）、`kernel_contact.rs`（好友/申请/标签分组/拉黑、申请发送解析与重启持久化）；
  - 插件分发：`p2p_plugin_announce.rs`（`/spark/plugin-announce/1.0.0` gossipsub 声明广播、PoW 校验、relay 资历门控）；
  - 同步裁决：`sync_apply_append_only.rs` / `sync_apply_lww.rs`（水位线拦截、append-only 幂等/拒绝、LWW 分支）。

桌面壳层：

```bash
cd code/app/src-tauri && cargo test --lib   # 命令层 *_inner 直调 + 市场服务 + 懒惰核查
```

- 市场服务（`src-tauri/src/market/tests/`）：install / updates / reconcile / wire / e2e（官方签名链）+ repo（仓库锚定：id 解析规范化、URL 模板、镜像展开、声明双源交叉、id 一致性校验）+ sideload（.spkg 侧载：inspect/import、整包与逐文件哈希复核）+ uninstall（卸载与墓碑防复活）；
- 广播懒惰核查（`src-tauri/src/announce_verify.rs` 内联单测）：核查队列、verified 终态回写内核索引。

跨进程 e2e（真实 P2P 协议栈多实例全链路）：

```bash
cd code/core && cargo build --example e2e_node
cd code && python3 scripts/e2e/run_all.py    # 全量；可加场景名过滤，如 run_all.py chat
```

- `core/examples/e2e_node/`：stdio JSON 行驱动的 kernel 节点（业务命令直通
  kernel 同步 API，p2p 事件异步透出，`--data-dir`/`--preferred-port` 参数；
  2026-09 新增数据命令组 data-declare/save/delete/get/query、
  grant/revoke/list-access、org-publish-access-key、org-set-member-role、
  org-fold-vv，见 `org-o2-acceptance`（验收报告，wiki testing/））；
- `scripts/e2e/`：Python3 纯标准库场景运行器（wait_event/poll_until 断言，
  不用固定 sleep）。9 个场景：friends（申请/接受/复合 id/拉黑拒收）、chat
  （收发/未读/已读/撤回/离线 sending 入队→resend 兜底 + 事件驱动自动补投/
  link 截断与非法 url 丢弃）、
  profile（资料变更推送）、org（DM 邀请全流程 + 系统会话卡片 + 双向快照同步 +
  组织昵称/logo 传播 + **P2 join 新通道**：无预录寻址成员经 orgsync 收敛加入、
  自写条目端点回填）、org_data（O2 双端：org 集合声明先行 + 写/删/墓碑后再写
  靶心 + 折叠 vv 收敛）、org_orgq（O2 三端：成员 orgq 写/删受理 + D1 折叠失明
  回归 + encrypted 密钥分发 + 复制组收敛；2026-09-02 晚 F1/F2/F3 修复后
  规避编排全移除——B 全程普通成员直收声明、accessKey 背靠背发布、单次 grant）、
  org_remove（**P2 L3 成员移除**：在线移除 OrgRemoved+本地擦除、离线移除
  pending 补投+擦除、成员墓碑收敛不复活）、
  org_sync_soak（**F6 回归哨兵，P3 改写**：原 stall 场景的注入目标 S2
  reconcile/org-pull 已随 legacy 出站停发删除，改写为无注入稳态 soak——
  45s 窗断言 tick 心跳持续、queue_depth ≤3、hello 间隔 ≤55s；注入命令
  fault-org-pull-blackhole 留存于 e2e_node 待内核评估 S1/S3 注入面）、
  devices（同身份设备配对自动接受 + 自消息跨设备同步）。
  各轮次实测/复跑记录（2026-09 F6/F7/F8 收口、P2/P3 联调、最终回归）归
  `org-o2-acceptance`（验收报告，wiki testing/），本文不重复登记。
- 已知边界：好友申请「接收方主动发起询问」无出站命令（xfail，p2p-messages §19.2
  边界③）；org-add-member 后立即发邀请存在拨号竞争（邀请 dm 尽力投递无重试，
  场景内等推送落地再发，UI 连发同样可能踩到，待内核加重试/串行化）；内容型
  dm kind 共享 per-peer 1s 限流桶（脚本按限流节奏发）。

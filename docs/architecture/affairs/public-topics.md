# 架构设计：公共议题（indexer、Git 工作流与公开名片）

> 模板：产品目标 → 现状 → 差距 → 目标设计 → 迁移路径 → 验收。对应产品文档：[../../product/affairs/public-topics.md](../../product/affairs/public-topics.md)（定稿）。

## 一、产品目标（来自 product/affairs/public-topics.md）

1. 事务打开"公开发布"即进全网公共目录；搜索只索引标题 / 简介 / 标签（产品级承诺）；indexer 任何人可运行、一键切换，匹配与聚合是内核确定性函数（换 indexer 只换可用性，不存在排序操纵）；
2. **Git 工作流（product/todo #24）**：项目仓库经文件同步分发为**只读镜像**（无独立 Git 协议层）；本地 git clone 自便；改动走 PR（子事务 + bundle / patch 附件）；维护者本地合并后同步分发；桌面装 git CLI、移动端 isomorphic-git 只读；
3. **组织公开名片**：签名的公开名片进公共目录，字段逐个可选、公开粒度组织自决；
4. **自我审查 indexer 层**（product/todo #22③）：indexer 可以不索引任何内容——只影响自己用户的搜索结果，换 indexer 即换视图；
5. 健康信号 = 本地事务日志确定性复算（indexer 无法编造）；不做全局唯一可读名。

## 二、现状（核实日期：2026-09-08）

**wiki**：`wiki/product/public-topics.md`、`docs/protocol/affairs/affair-metadata.md`（元数据面与健康信号字段级；§6 组织名片"收录即接通"、扩展可选段归组织名片插件定义）。Git over P2P 无独立设计文档，仅存一行式表述（`wiki/product/development_plan.md:72`、`wiki/product/public-topics.md:87`），且 `docs/product/roadmap.md` 与 `docs/product/README.md` 仍称其为"内核能力"——与 #24 拍板冲突（见 §五.2 待修订清单）。

**代码**：

- **indexer（C10 已实现）**：`core/src/index/`（匹配 / 聚合确定性函数、健康信号计算）、`spark-affair-meta` 元数据面；
- 元数据面 gossip（C4）：`sync/affairsync/` + `p2p/node/gossip.rs`（白名单整批拒收、逐条验签、关注门槛）；
- **org-card intake 已落地（2026-09-08, commit 186f7a2）**：`gossip.rs:248-319`——`type='org-card'` 分流 `handle_inbound_org_card`（信封 id == orgAddress → org-address §16.3 五步校验链 → seq/publishedAt 冲突裁决 → `p2p:org-address:` 缓存沉淀），测试覆盖有效/过期/篡改/id 错位/畸形（`p2p/node/tests.rs:1352-1440`），查询经 `resolve_org_address` / `search_known_orgs` 读同一缓存（收录即接通）；
- **indexer 开关 Tauri 已注册（同 commit）**：`commands/affairs.rs:249-253` `plugin_affairs_set_indexer_enabled` → `kernel.set_indexer_enabled`（`lib.rs:342` 注册）；**未接前端**（`code/app/src` 零引用），且**启用态为会话级内存、默认关闭、重启即失**（`index_ops.rs:17` 头注自承）——设置项持久化缺失；
- 文件 blob 面：内容寻址 blob（foundation/personal-data 篇 §4.2 落地后可用；Git 对象天然内容寻址、直接兼容）。

## 三、差距

| # | 差距 | 出处 |
| --- | --- | --- |
| G1 | org-card intake 已接线，剩余：名片**扩展字段**（简介/成员组织注册表/公开议题列表/联系方式）线形与索引、org-card **发布通路**（向 spark-affair-meta 主题发布侧）、indexer 目录收录呈现 | 09-06 零调用清单（09-08 部分已消） |
| G2 | indexer 开关：前端设置界面缺失 + 启用态未持久化（会话级内存、重启即失） | 同上 |
| G3 | Git 工作流：只读镜像分发 + PR（子事务 + bundle）| product/todo #24 |
| G4 | indexer 索引侧过滤（运营者本地规则） | product/todo #22③ |
| G5 | **组织域内容处置操作缺失**（#22②：组织对本域内容的折叠/移除，公示 + 留痕 + 可申诉，内核新增走公示延迟）——自我审查三层中的组织层 | product/todo #22② |

## 四、目标设计

### 4.1 org-card 扩展与发布（G1）

- **intake 已落地（现状）**，本篇不再设计收录链；剩余三件：
  - **扩展字段线形**：名片可选段（简介/成员组织注册表/公开议题列表/联系方式）按 affair-metadata §6 既定分工归**组织名片插件**定义，内核只承载 org-address 记录全文透传；公开粒度组织自决（product/membership 开放授权框架的公开发布特例：名片即"对全网公开档"的声明）；
  - **发布通路**：组织侧向 spark-affair-meta 主题发布 org-card 信封（复用 org-address 记录签名，发布侧当前缺失）；
  - **indexer 呈现**：索引名片（与议题同目录）；验证失败 / 签名包 degraded 标注如实呈现。

### 4.2 indexer 启停接线（G2）

Tauri 命令已在（`plugin_affairs_set_indexer_enabled`）；剩余两件：① **前端设置界面**（任意节点一键成为 indexer；桌面全节点同时可自建本地索引、完全不依赖任何 indexer——既有能力，仅暴露开关）；② **启用态持久化**（`set_indexer_enabled` 写入 kernel 存储键，重启保持——现状会话级内存是如实标注的临时态）。

### 4.3 Git 工作流（G3，#24 全部）

```
权威仓库（项目议题引用）= blob 集合，只读镜像分发（A1 blob 层 + 事务关注者复制组，持有即做种）
  → 任何节点本地 git clone（只读镜像源）→ 本地随意提交
  → 回流：PR = 父子引用子事务 + bundle/patch 附件（blob，内容寻址）
  → 维护者本地 git am / merge（单点、串行）→ 写回权威仓库 → 同步分发
```

- **只读镜像**：权威仓库的 `.git` 作为只读 blob 集合分发（事务内容面"持有即做种"语义）；**写撕裂面在结构上不存在**——没有任何节点对镜像执行写操作（应用层只读挂载 + 维护者本地副本是唯一写点）；
- **PR 载体**：`affair` 子事务（关系类型 = 父子，affair-model 既有四种引用），附件 = git bundle（单文件，含提交序列）或 patch 系列，存 blob 并以 cid 入事务操作；
- **合并**：维护者（议题规则声明的写权集合）本地执行 merge 后推送新镜像版本；多维护者串行约定（合并权本身可走延迟否决公示，与 governance 决策结构一致）；
- **工具链**：桌面随代码仓库应用安装 / 检测本地 git CLI；移动端 isomorphic-git（纯 JS）只读浏览与 diff 查看；
- **wiki 侧**：Git over P2P 无独立设计文档可标，待修订清单见 §五.2。

### 4.4 indexer 索引侧过滤（G4）

- indexer 本地声明式过滤规则（关键词 / 来源 / 标签黑名单，indexer 运营者自配，存 indexer 本地不入同步）；
- 过滤只影响该 indexer 的搜索结果——**不索引 ≠ 删除**，条目仍在元数据面（其它 indexer 与直连节点可见）；这正是"换 indexer 即换视图"的产品语义，也与责任落点互为表里：indexer 运营者对自己索引与呈现的内容负责，过滤只影响自己的视图；
- 不做协议字段（防止被误读为全网审查能力）。

### 4.5 组织域内容处置操作（G5，#22②）

- **处置操作**：组织对**本域内容**（本组织域内的事务本体 / 集合条目）的折叠 / 移除——新增操作类型，效果 = 本域数据面不再呈现（与"删除只有不再有人提供一种语义"不冲突：处置是组织域自管面的呈现与分发停止，已流出到关注者/公共面的副本收不回）；
- **硬约束**：处置属组织级动作，走**公示延迟**（governance 篇公示延迟禁令的适用范围，与开放声明同族）；全程**留痕**（处置操作入存证链，被处置内容哈希可考）；
- **可申诉**：成员可经事务申诉机制（affair-model 四种引用之申诉）发起复核，推翻处置走集体决策；
- **边界**：仅本域；公共议题（已公开发布）不在此列——公共面只有 indexer 视图过滤（§4.4）与社会途径。

## 五、迁移路径

1. org-card 扩展 / indexer 开关：接线与持久化，无迁移；
2. Git：新工作流随代码仓库应用插件落地（catalog 里程碑一）；无代码退役（Git over P2P 从未实施）。**文档修订已完成（2026-09-08，用户拍板）**：`docs/product/roadmap.md`（里程碑一验收与内核能力清单）、`docs/product/README.md`（内核能力列举）、`wiki/product/development_plan.md:72`、`wiki/product/public-topics.md:87` 均已按 #24 口径改写；
3. indexer 过滤：indexer 侧配置项新增，无协议变更；
4. 组织域处置操作：新增操作类型（append-only 自然兼容），存量组织默认无处置记录（一切照旧）。

## 六、验收

- **向量**：org-card 线形与签名验证（有效 / 篡改 / degraded 标注）；无 Git 协议向量（无协议层是本设计的要点）；
- **单测**：org-card intake 五步链、indexer 开关状态、过滤规则仅影响本地索引结果；
- **集成**：两节点——A 发布 org-card → B 的 indexer 收录可搜；议题引仓库 → B clone → 提交 → PR 子事务 + bundle → A（维护者）merge → B 同步到新镜像；indexer 过滤前后搜索对比；
- **回归**：C10 indexer 确定性匹配 / 健康信号、C4 元数据面白名单与验签全部用例。

---

> 关联：product/todo #24（§4.3）、#22③（§4.4）；architecture/affairs/affair-model.md（PR = 父子引用子事务）；architecture/community/membership.md §4.1（镜像分发复用的数据面）；catalog 代码仓库应用行（插件载体）。

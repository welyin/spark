# 架构设计：经济层总纲（内核对象、模块布局与分期）

> 模板：产品目标 → 现状 → 差距 → 目标设计 → 迁移路径 → 验收。对应产品文档：[../../product/economy/README.md](../../product/economy/README.md)（定稿）。覆盖 product/todo #8（经济内核，里程碑一前置）。

## 一、产品目标（来自 product/economy/README.md）

1. 内核只守**宪章层不变量**：唯一序列与防伪、状态机不可回退、销毁见证、发行恒等式、**发行必须依据经事务决议授权的发行策略**（策略立改废走决议；策略内发行内核确定性执行；裁量发行逐次决议）、无私钥直接发行接口、规则公开义务、票权不可买、无对私兑现；
2. 政策层全归组织（规则包插件，闭源不得执行）；模板层无隐式默认；
3. 唯一正确答案判据：结构性强制 / 证据性支持 / 不介入三手段；不做争议仲裁（只有"挂起等裁决凭证"状态原语）；
4. 排期：经济内核是**里程碑一（自举）前置**（roadmap §三"前置"段）；前两个里程碑只做**单组织闭环**（Q13），跨组织归其后。

## 二、现状（核实日期：2026-09-08）

**代码**：经济模块**为零**（`core/src` 无 economy / credits 模块命中；`credential` 模块的 issuance 字样为凭证签发语义，与经济无关）。

**可复用资产**：

| 资产 | 位置 | 用途 |
| --- | --- | --- |
| 存证链与锚（已建成） | `core/src/evidence/` | 积分流水 append-only + 销毁日志 + 恒等式可审计 |
| 事务决议与规则化发行触发（已建成；evi:resolution 为 affairs 篇 A21 新增项） | `core/src/affair/`（decide / rulechain） | 发行策略的立改废决议、触发事件（PR 合并 / 采纳）来源 |
| 网关邮箱（已建成） | org-mail | 跨组织核销凭证送达（v 后段） |
| 定点数（无先例，新约定：u128 刻度整数，禁浮点） | 本篇 §4.1 `fixed.rs` | 全部金额运算 |
| 母稿 §9 技术映射 | 根目录《共同体经济系统设计.md》（保留） | 数据结构与信封设计底稿——**注意两处过时**：§9"复用 orgkey-acl"（C7 已退役，需重新选型）、§10 v2"永久贡献档案与内部资本账户"（不采纳：product/economy/credits 定稿口径为归插件 / 系统外；product/roadmap §五 v2 残留表述已于 2026-09-08 随用户拍板清理对齐） |

注：经济规则包 trigger 金额表达式的求值**不挂 policy 引擎**（interpretation 篇裁定其领地为"可见性与开放"一族）——求值归 economy 内核自有的声明式表达式求值器（与 `fixed.rs` 同纪律，本篇裁定）。

## 三、差距

经济层全部为零（G1）；且母稿与产品有两处口径差（G2：orgkey-acl 替代选型；G3：贡献档案 / 出资账户不进内核——架构以 docs/product 为准，母稿相应段落不采纳）。

## 四、目标设计

### 4.1 模块布局

新建 `core/src/economy/`（**纯逻辑层**，与 org / affair / policy 同纪律：now_ms 注入、不碰网络）：

```
economy/
  types.rs       # 积分批次、序列号条目、兑付单、往来账、协议记录线形
  issuance.rs    # 发行（策略触发求值 / 裁量决议校验 / 恒等式）
  redemption.rs  # 兑付单状态机 + 销毁日志
  ledger.rs      # 往来账 / 净额 / 平仓登记（v 后段）
  agreements.rs  # 双边协议图、汇率整数比、净应收上限（v 后段）
  supply.rs      # 最小供给声明（结构化 / 版本化）
  reserve.rs     # 不可分割储备账户结构（无按份私分出口）
  fixed.rs       # 定点数：u128 刻度整数、整数比连乘、末步取整（方向规则包声明）
  health.rs      # 健康度确定性指标（capital-defense 篇 §4.1）
  winddown.rs    # 出清原语：状态冻结 / pro-rata / 存证（capital-defense 篇 §4.2）
```

### 4.2 内核对象清单（第四类一等对象：规则包）

| 对象 | 要点 | 分册 |
| --- | --- | --- |
| 积分批次 / 序列号 | 记名、唯一序列号、状态机 issued→held→locked→burned；批次绑定规则包版本 | credits |
| 发行策略 | 规则文本 + 插件引用 + 版本 + 决议授权（evi:resolution 锚）；触发事件须存证链可验证 | credits |
| 兑付单 | proposed→accepted→delivered→burned + rejected / timeout / disputed（挂起等裁决凭证） | credits |
| 最小供给声明 | 结构化 / 版本化 / 留痕；内核不核实真伪，只算比值 | credits |
| 双边协议 / 往来账 / 净额 | 相互受理、净应收上限、平仓、协议解除 | inter-org |
| 不可分割储备 | 进得去、无按份私分出口的账户结构；是否设立属政策层 | credits / capital-defense |
| 出清原语 / 健康度指标 | 状态冻结、pro-rata、确定性指标（证据档） | capital-defense |

### 4.3 分期（与 roadmap / Q13 对齐）

- **里程碑一前置（本批）**：credits 篇全部（单组织闭环：发行 / 兑付 / 恒等式 / 兑付单 / 供给声明 / 储备 / 分流）；
- **里程碑二后**：inter-org 篇（直接双边）→ capital-defense 篇（出清 / 健康度）→ v2 代理链（HTLC，远期）。

## 五、迁移路径

绿地无迁移。落地顺序：types + fixed → issuance → redemption → supply / reserve →（v 后段）ledger / agreements。母稿保留至经济内核落地后再评估删除（product/economy/README 头部约定）。

## 六、验收

- 各分册验收节为准；总纲级：发行恒等式任何成员随时可复算（确定性内核函数 + 独立复算工具）、定点数全组向量（刻度 / 连乘 / 末步取整 / 同离散值）。

---

> 分册：[credits.md](credits.md)（本批）、[inter-org.md](inter-org.md)、[fiat-compliance.md](fiat-compliance.md)、[capital-defense.md](capital-defense.md)（后三篇为后段设计目标）。

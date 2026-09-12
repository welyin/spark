# 架构设计：存证（链、锚、导出包与名册快照）

> 模板：产品目标 → 现状 → 差距 → 目标设计 → 迁移路径 → 验收。对应产品文档：[../../product/foundation/evidence.md](../../product/foundation/evidence.md)（定稿）。

## 一、产品目标（来自 product/foundation/evidence.md）

1. 关键数据形成只增不减的链式存证，锚定可引用；存证与同步正交；
2. **导出包 + 独立核验工具**：没装 Spark、不联网的一方（街道办）也能验证签名与完整性；
3. **分层诚实三层**：① 完整性（未篡改 + 签名有效）② **成员资格**（签名者当时确为成员 / 有角色——导出包附名册存证锚快照，product/todo #6，Q05 前置）③ 业务资格（验证人凭证）；
4. 交付路径：CLI + 打印报告（EV2 已决：网页版不排期；打印 + 手工签字为最经济稳妥路径）。

## 二、现状（核实日期：2026-09-08）

**wiki**：`wiki/architecture/sync/evidence-anchoring-export.md`（2026-09 阶段四F **已实施**）；`sync-and-evidence.md`。

**代码**：

- 存证链 `core/src/evidence/chain.rs`（append / verify / headHash + canonical + golden vectors）；
- 锚定 `evidence/anchor.rs`（节点锚记录 LWW + 组织锚根默克尔派生 + 分叉检测）；导出 `evidence/export.rs`（导出包构建 + 五步核验）；`kernel/evidence_ops.rs`（锚定触发 + 导出 API）；**核验 CLI `src/bin/evidence-verify.rs`**；`commands/evidence.rs`（head / verify / entry / export）；
- **名册快照已建成**：`core/src/affair/snapshot.rs`——`SnapshotPayload`（阶梯名册快照 / 组织名册快照 @ 存证锚点）、`verify_ladder_roster`（:126，**仅 Ladder 形态**，OrgRoster 形态恒 false）、`member_set_hash`（:99，org-signature §3 口径）；`verify_ladder_roster` 已有生产调用（`kernel/affair_ops.rs:181` `resolve_snapshot_roster`，ladder 形态法定人数快照的决议求值路径）；**未接线的缺口**是 OrgRoster 形态成员集无生产来源（`affair_ops.rs:178` 如实标注）与导出包侧名册段未建；
- 名册承诺口径已有：`org-signature.md` §3（memberSetHash = 排序成员条目数组哈希，条目 = identity + role）；
- golden vectors：`code/spec/vectors/evidence-anchor.json`。

**缺口**：~~导出包只覆盖分层诚实的第①层（完整性），不附名册快照~~ **已补齐（2026-09-08 A11/A12，G1/G2/G3 核销）**：导出包 roster 段（exportV 2，构建编排 `kernel/evidence_ops.rs`、核验第六步 `evidence/export.rs verify_roster_layer`、golden vectors `code/spec/vectors/evidence-roster.json`）+ CLI `--report` 打印报告（`evidence/report.rs`）。**签名回查口径（2026-09-08 主控裁定）**：导出者**在册即可**（成员资格可证），不强制 admin——导出是读侧行为，任何成员对自己持有的数据天然可导出；「角色不符必败」的本意是**包内治理签名的签名者**角色须满足该签名对应策略（如治理签名 admin）。

## 三、差距

| # | 差距 | 出处 |
| --- | --- | --- |
| G4 | org_user_id 切换后快照条目的 identity 语义（域公钥哈希）需在 schema 上平滑 | Q20（community 篇） |

## 四、目标设计

### 4.1 导出包扩展：roster 段

导出包新增 `roster` 段（可选但治理场景默认携带）：

```json
"roster": {
  "memberSetHash": "<64hex>",
  "anchor": { "orgId": "org_<…>", "anchorRoot": "<64hex>", "ts": 1720000000000 },
  "snapshot": [ { "identity": "<64hex>", "role": "admin" } ],
  "anchorProof": [ "<inclusion proof  siblings>" ]
}
```

- **构建路径**：roster 段由导出方从本地 `OrganizationRecord.members`（identity + role）直接构建，memberSetHash 复用 `snapshot.rs:99` `member_set_hash()`——**不依赖** affair 推导路径"OrgRoster 成员集无生产来源"的已知缺口（两条路径互不阻塞）；
- **anchorProof 素材来源**：名册承诺须先在链上——构建 roster 段时若该名册承诺尚未锚定，导出方先追加一条 roster 存证条目并触发锚定（"先写条目后锚"同 affairs 篇 evi:resolution 关闭路径），inclusion proof 从组织锚根的默克尔树生成；任何核验方对已知锚记录重算默克尔根验证；
- **核验第六步**（evidence-verify 扩展）：复算 memberSetHash 匹配（`member_set_hash` 同算法）→ 复算 anchorRoot 匹配 → **对包内每条签名回查"签名者在快照中、当时角色满足规则"**（新写的包内名册回查逻辑，G2 接线点——不复用 `verify_ladder_roster`，它只验 Ladder 形态）；
- **分层诚实表述固定化**：核验报告按三层分别给结论——① 完整性（链 + 锚 + 签名）② 成员资格（名册快照回查）③ 业务资格（验证人凭证，若随包附加业主凭证等）；任一层不通过都如实分列，不搞"总体通过"的模糊话术。

### 4.2 打印友好报告（EV2 落点）

- `evidence-verify` 新增 `--report`：输出单页可打印文本（案件摘要、签名清单[签名者 identity + 当时角色 + 有效性]、名册快照摘要、三层结论、锚引用）；
- 设计约束：纯文本、等宽友好、不依赖任何 Spark 语义注释（街道办人员读得懂）；
- 里程碑二路径 = 懂技术成员跑 CLI → 打印 → 相关人员手工签字 → 提交（patterns 配方五既定排序①）。

### 4.3 org_user_id 平滑（G4）

快照条目 `identity` 字段语义 = "组织内的成员标识"（schema 不绑定 rootId）：Q20 切换后条目自然变为 org_user_id（域公钥哈希），包结构与核验算法不变；切换期核验工具按快照自带 identity 验证签名公钥对应关系即可（签名包分量自带 publicKey，signer == sha256hex(publicKey) 既有绑定）。

## 五、迁移路径

1. 导出包版本字段 `exportV: 1 → 2`（含 roster 段）；旧核验工具遇到 v2 包提示"请升级核验工具"（fail-closed，不静默跳过 roster 段）；新工具验 v1 包时第②层如实标注"本包不含名册快照"；
2. 无存量数据迁移（存证链与锚记录不动，仅导出格式演进）；
3. snapshot.rs 接线随导出构建路径上线即激活，无独立迁移。

## 六、验收

- **向量**：roster 段逐字节（固定名册 → memberSetHash 固定值）、anchorProof inclusion 正反用例、篡改 snapshot / anchor / memberSetHash 各必败、v1/v2 包互验行为；
- **单测**：roster 段构建（本地名册 → memberSetHash 与 `member_set_hash` 一致）、第六步回查（在册 admin 通过 / 非成员必败 / 角色不符必败）、报告生成；
- **集成**：端到端——组织内完成一次签名表决 → 锚定 → 导出 → **离线环境** evidence-verify 全绿 → `--report` 打印件人工可核；
- **回归**：`evidence-anchor.json` 向量、锚定 / 分叉检测、导出五步核验既有用例全绿。

---

> 关联：product/todo #6（本条）；architecture/community/membership 篇（org_user_id 决定快照 identity 内容）；`org-signature.md` §3（名册承诺口径复用）；`affair/snapshot.rs`（G2 接线对象）；architecture/economy/credits.md §六（A37：导出包积分流水核验扩展，随经济内核批次补设计——恒等式"给第三方复算"的载体）。

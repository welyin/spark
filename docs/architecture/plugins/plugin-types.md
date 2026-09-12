# 架构设计：插件类型与插件间契约（含契约发布物）

> 模板：产品目标 → 现状 → 差距 → 目标设计 → 迁移路径 → 验收。对应产品文档：[../../product/plugins/plugin-types.md](../../product/plugins/plugin-types.md)（定稿）。覆盖 product/todo #9、#23。

## 一、产品目标（来自 product/plugins/plugin-types.md）

1. 四类解释对象（事务类型 / 资格验证 / 经济规则包 / 应用视图）+ 两类横切件（门槛 / 策略插件）；
2. **插件间：可监督、可组合，但不直接互通**——互为数据孤岛，协作经内核中介调用对方**按契约公开的接口**，逐次校验、永不直接打通数据；监督插件无数据特权（只读接口聚合驾驶舱，未暴露则优雅降级）；
3. **契约发布物**：为治理策略提供**可锚定的数据快照**（插件签名 + 存证锚 + 时点），供策略声明引用为资格 / 权重输入；求值器只验证来源与完整性，不理解语义；**不存在求值器直读插件集合的通道**（product/todo #23 拍板）；
4. 验证类强制开源（L1 拒装 L0）；闭源不得执行经济 / 治理判定。

## 二、现状（核实日期：2026-09-08）

**wiki**：`wiki/architecture/plugins/plugin-interop.md`（2026-07 **设计草案、未实现**：manifest `provides` / `consumes` 声明、OpenRPC schema、内核桥中介逐次校验、版本 range + 优雅降级、接口面克制、同一插件互通是特例）。

**代码**：

- 插件运行时与桥：iframe 视图沙箱 + QuickJS 后台沙箱 + bridge（call / subscribe / event）+ SDK；`plugin_data` 8 命令（declareCollection 数据 API）；
- SDK 门面 C9 已落地：`sdk.affairs` / `sdk.credentials`（最小命令层 + 权限位）；
- **契约层零实现**（无 provides/consumes 解析、无 RPC 中介路由）；
- 凭证体系已实现（插件产物 B 的载体）；治理求值（`affair/decide.rs`、rules 求值）已落地。

## 三、差距

| # | 差距 | 出处 |
| --- | --- | --- |
| G1 | 插件间契约层未实现（provides / consumes + RPC 中介 + 版本协商 + 优雅降级） | product/todo #9 |
| G2 | **契约发布物**（可锚定数据快照）无类型与格式——治理策略引用的输入源缺失 | product/todo #23 |
| G3 | 监督驾驶舱首场景（公共账本只读接口 + 监督审计聚合）依赖 G1 | catalog 里程碑二 |

## 四、目标设计

### 4.1 契约层落地（G1，以 plugin-interop.md 为基础设计）

按其草案直接转实施，四点不变：schema 机器可读（OpenRPC）、内核桥中介逐次校验（永不直连）、优雅降级（`required: false` 缺失即降级不阻塞）、接口面克制。落地三步：

1. **manifest 解析与登记**：`provides` / `consumes` 字段解析，安装时静态校验（schema 存在、版本 range 可解）；提供方接口注册进内核路由表（`{pluginId, interface, version}`）；
2. **RPC 中介**：bridge 新增 `interop.call { interface, method, params }`——内核按路由表转发提供方 QuickJS 实例，逐次校验（消费方已声明 consumes 且 range 兼容、提供方在线、调用在权限内）；提供方不在线 / 未装 → 标准降级错误码。**readonly 权限分级**：schema 方法标注 `readonly: true/false`；监督类插件申请 `plugin:call:readonly` 权限，运行时强制只能调 readonly 方法——监督场景"无数据特权"的结构落点；
3. **类型生成（可选增强）**：从 schema 生成消费方 TypeScript 类型（开发体验，非运行时必需）。

### 4.2 契约发布物（G2，#23 核心设计）

**发布物 = 契约接口的一种内建返回类型**（不是新通道，是契约层的一个标准 schema）：

```json
{
  "snapshotV": 1,
  "pluginId": "<发布方>",
  "interface": "<接口名>",
  "dataHash": "<64hex>",
  "data": { /* 快照内容（如股权表） */ },
  "anchor": { "orgId": "org_<…>", "anchorRoot": "<64hex>", "ts": 1720000000000 },
  "sig": "<插件域身份签名，载荷 = 除 sig 外全部字段的 canonical>"
}
```

- **产生**：提供方插件经其契约接口按需签发（如 `shares.snapshot()`），或按策略声明的周期预发布（存证锚时点写入 `anchor`）；
- **消费**：治理策略声明引用发布物**接口与参数**（如 `shares.snapshot(登记日)`），**决议锚定时绑定当次快照的具体 dataHash**（资格 / 权重输入——类比 patterns 配方二"股权登记日"：策略不定死哈希，决议实例才绑定）；求值器（governance 篇 §4.5 接口约束）验证三件事——插件签名（来源）、dataHash 与 data 一致（完整性）、`anchor.ts` 早于决议开始 T0（时点）——**不理解 data 语义**；
- **确定性**：`anchor` 是时点与链根承诺；发布物副本可得性靠同步分发 + 签名自认证（锚不担保副本分发）——同一 dataHash 同一内容，所有节点求值结果一致；
- **反直读**：求值器不接受任何"集合引用"输入——只接受发布物（结构保证）。

### 4.3 首批场景与顺序

1. **监督驾驶舱**（里程碑二）：公共账本 / 签名表决 / 信息公示各自暴露只读接口（`provides`），监督审计插件聚合（`consumes`，全部 required: false）——契约层的首个生产场景；
2. **股权加权**（patterns 配方二）：股权插件暴露 `shares.snapshot()` 发布物接口，治理策略引用；
3. **项目组合**（catalog 里程碑一）：项目插件经库依赖复用基础件（库包机制见 runtime-and-trust 篇 §4.1，与契约层互补：库 = 构建期复用代码，契约 = 运行时互通数据）。

## 五、迁移路径

1. manifest 新字段向后兼容（旧插件无 provides/consumes = 纯孤岛，行为不变）；
2. 契约层与发布物随首批场景（监督驾驶舱）上线即激活，无存量迁移；
3. `plugin-interop.md` 转实施时以本篇 §4.2 补入发布物 schema（wiki 同步）。

## 六、验收

- **向量**：provides / consumes manifest 解析（版本 range 可解 / 冲突）、RPC 中介信封、发布物线形（sig 载荷逐字节、dataHash 一致性、时点判定）；
- **单测**：路由表登记 / 注销、逐次校验拒绝（未声明 consumes / range 不兼容 / 提供方离线降级码）、**readonly 强制（监督插件持 `plugin:call:readonly` 调写方法必拒）**、求值器对发布物三验证（伪造签名 / 篡改 data / 快照晚于 T0 各必败）、**直读集合输入拒绝**；
- **集成**：监督审计插件聚合三个业务插件只读接口（一个未暴露 → 对应区域"未启用"优雅降级）；股权快照 → 治理求值 → 权重一致（两节点复算一致）；
- **回归**：iframe / QuickJS 沙箱、bridge 既有通路、declareCollection 数据域隔离全部用例。

---

> 关联：product/todo #9（§4.1）、#23（§4.2）；architecture/affairs/governance.md §4.5（求值器侧接口约束）；architecture/plugins/runtime-and-trust.md（库包 = 构建期复用，与本篇运行时契约互补）；`plugin-interop.md`（契约层基础设计）。

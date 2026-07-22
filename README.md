# Spark（星火）

面向业主自治与社区协作的去中心化应用：组织化数据同步、链式存证、插件化业务。
单 Rust 内核 + Tauri 双端壳（PC / 移动端），monorepo 管理。

- 产品定位、设计文档、开发计划：见 [wiki](https://github.com/welyin/spark/wiki)
- 协议字节级权威规格：见 [`spec/`](spec/)

## 仓库布局

```
code/
├── core/          # spark-core：Rust 内核 crate（不依赖任何壳层框架，可独立测试）
│   ├── src/       # identity / storage / schema / sync / evidence / collection
│   │              # org / data-mgmt / p2p / kernel（壳层门面）
│   └── examples/  # lab_node.rs：p2p-lab interop 场景驱动例程
├── spec/          # 协议规格（五份）+ golden vectors（从旧 TS 实现反向提取，逐字节对齐）
├── desktop/       # Tauri 2.x PC 壳（命令层内嵌内核 + Vue3/Element Plus 前端）
├── plugins/       # 插件（weibo-core）+ 打包签名脚本 + 发布说明
├── mobile/        # Tauri 移动壳（阶段④待建，Android 先行）
└── .github/       # 插件发布 workflow（GitHub Releases + Ed25519 信任链）
```

> 旧世界（Electron+TS 实现）在仓库外的 `desktop/`（冻结、将退役）与 `desktop-wiki/`，仅供对照。

## 快速开始

```bash
# 内核测试（Rust ≥ 1.97，rustup 安装）
cd core && cargo test

# PC 壳开发运行（需要 Node 18+ 与 Rust）
cd desktop && npm install && npm run tauri dev

# PC 壳测试与构建
cd desktop && npm run build
cd desktop/src-tauri && cargo test

# 前端单元测试
cd desktop && npx vitest run
```

## 当前状态

- ✅ 阶段①②：协议规格 + golden vectors；内核全模块移植（282 测试全绿）；与旧 TS 实现双端互通验证（四场景 PASS）
- ✅ 阶段③（开发完成）：Tauri PC 壳 54 命令、插件市场（file:// 安装链路端到端验证）、组织同步编排（双 kernel 对跑）
- ⏳ 阶段④：移动端（Android 先行）
- ⏳ 阶段⑤：北极星业务（议事投票插件 + 存证导出 + 试点）

详细计划与验收标准：wiki [development_plan](https://github.com/welyin/spark/wiki/dev/development_plan)。

## 插件

微博插件（weibo-core）为插件体系示例：源码在 `plugins/weibo-core`，打包/签名/发布/安装流程见
[plugins/README.md](plugins/README.md) 与 wiki 插件开发指南。

## 纪律

- 协议行为以 `spec/` 为权威；改动协议必须同步更新 spec 与 golden vectors
- 内核 crate 不依赖 Tauri/Electron 任何 API
- 旧 TS 工程冻结：只修 bug，不加功能

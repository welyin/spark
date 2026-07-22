# Spark（星火）

面向业主自治与社区协作的去中心化应用：真实身份自持、组织化数据同步、链式存证、插件化业务。

单 Rust 内核 + Tauri 双端壳（PC / 移动端），monorepo 管理。

- 文档（产品定位、设计、协议规格、开发计划）：[wiki](https://github.com/welyin/spark/wiki)

## 特性

- **身份自持**：BIP39 中文助记词 + 加密二维码备份，设备级多用户，域身份派生隔离
- **组织同步**：邀请码入网、K 副本冗余、断连自愈（全局覆盖网 + 定向恢复查询）
- **链式存证**：写入即存证，哈希链可校验、可导出
- **插件体系**：插件域数据隔离、签名打包、插件市场（Ed25519 信任链）
- **P2P 网络**：rust-libp2p（yamux / gossipsub / mDNS / relay / AutoNAT / UPnP），双栈 IPv4+IPv6

## 仓库布局

```
├── core/      # spark-core：Rust 内核 crate（身份/存储/集合同步/存证/组织/数据治理/P2P/门面层）
├── spec/      # golden vectors（协议规格文档在 wiki protocol/ 专区）
├── desktop/   # Tauri 2.x PC 壳（Rust 命令层 + Vue3/Element Plus 前端）
├── plugins/   # 插件（weibo-core 微博）与打包签名脚本
├── mobile/    # Tauri 移动壳（规划中，Android 先行）
└── .github/   # 插件发布 workflow
```

## 快速开始

```bash
# 内核测试（Rust，rustup 安装）
cd core && cargo test

# PC 壳开发运行（Node 18+ 与 Rust）
cd desktop && npm install && npm run tauri dev

# PC 壳测试与前端构建
cd desktop/src-tauri && cargo test
cd desktop && npx vitest run && npm run build
```

## 插件开发

插件源码放在 `plugins/` 下，经编译期自注册加载；打包、签名、发布与安装流程见 wiki
[插件开发指南](https://github.com/welyin/spark/wiki/dev/plugin_development)。

## 工程纪律

- 协议行为以 wiki protocol/ 规格为权威；改动协议必须同步更新规格与 golden vectors
- 内核 crate 不依赖任何壳层框架 API，可独立构建与测试
- 提交前：`cargo test`（core 与 src-tauri）、`cargo clippy --all-targets -- -D warnings`、`npx vitest run`、`npm run build` 全绿

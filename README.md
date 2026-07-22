# 星火 Spark

星火（Spark）是一款面向基层社区自治的分布式协作工具底座，基于 P2P 对等网络构建，无中心化服务器，数据由社区成员共同持有与管理。

项目以「群众自主掌握公共事务工具」为核心出发点，为业委会筹建、业主议事表决、邻里互助、公共账目公开等场景，提供抗关停、防篡改、保隐私的技术支撑，让基层自治的规则与数据真正回归全体成员。

星星之火，可以燎原。从一个小区起步，让每个社区都能拥有属于自己的自治基础设施。

## 核心特性

- **分布式无中心架构**：基于 rust-libp2p 构建对等网络，无单点故障，数据多节点冗余备份，组织断连可自愈。
- **数据主权完全归用户**：原始业务数据仅存于用户本地设备与所在组织成员节点，链上仅留存哈希存证。
- **插件化可扩展骨架**：核心程序提供网络、身份、存证等基础能力，所有业务功能通过插件实现。
- **原生适配社区自治规则**：支持可验证的电子签名与投票存证，贴合业委会选举、公共决策的合规要求。
- **跨端协同的分层设计**：单一 Rust 内核，桌面端承担全节点职能，移动端提供轻客户端体验。

## 技术栈

- 应用框架：Tauri 2.x（PC / 移动端同一内核）
- 内核：Rust（spark-core，身份 / 存储 / 同步 / 存证 / 组织 / P2P 全模块）
- 前端技术：Vue 3 + TypeScript + Vite + Element Plus
- P2P 网络：rust-libp2p（yamux / gossipsub / mDNS / relay / AutoNAT / UPnP，双栈 IPv4+IPv6）
- 本地存储：sled（文档集合 + 链式存证日志）
- 加密体系：Ed25519 非对称加密 + BIP39 中文助记词 + scrypt 密钥派生

> 更详细的架构、设计与协议说明，请查阅 [Spark Wiki](https://github.com/welyin/spark/wiki)。

## 仓库布局

```
├── core/      # spark-core：Rust 内核（身份/存储/集合同步/存证/组织/数据治理/P2P）
├── spec/      # golden vectors（协议规格文档在 wiki protocol/ 专区）
├── app/       # 应用壳（Tauri 2.x，PC 与移动端同一工程 + Vue 3 前端）
├── plugins/   # 插件（weibo-core 微博）与打包签名脚本
└── .github/   # 插件发布 workflow
```

## 快速开始

### 环境要求

- Rust（rustup 安装，stable）
- Node.js >= 18.0.0
- 操作系统：Windows 10+ / macOS 11+ / Linux 主流发行版

### 本地开发

```bash
# 克隆仓库
git clone https://github.com/welyin/spark.git
cd spark

# 内核测试
cd core && cargo test

# 应用壳开发运行
cd ../app && npm install && npm run tauri dev
```

### 测试与构建

```bash
# 应用壳 Rust 命令层测试
cd app/src-tauri && cargo test

# 前端单元测试与生产构建
cd app && npx vitest run && npm run build

# 打包当前平台安装包
cd app && npm run tauri build
```

## 文档

详细文档见 [GitHub Wiki](https://github.com/welyin/spark/wiki)，主要入口：

- [架构设计](https://github.com/welyin/spark/wiki/architecture)
- [协议规格（字节级权威）](https://github.com/welyin/spark/wiki/协议规格)
- [插件开发指南](https://github.com/welyin/spark/wiki/plugin_development)
- [开发计划](https://github.com/welyin/spark/wiki/development_plan)
- [测试体系](https://github.com/welyin/spark/wiki/testing)

## 插件开发

星火采用「核心骨架 + 插件应用」的开放架构，所有业务功能均可通过插件扩展。插件基于 TypeScript + Vue 3 开发，运行于独立插件域，通过标准化 SDK 调用底层能力；插件包经 Ed25519 签名后通过插件市场发布与安装。

详细开发规范请参考 [插件开发指南](https://github.com/welyin/spark/wiki/plugin_development)。

## 参与贡献

欢迎以任何形式参与项目建设：

- 提交 Issue 反馈 Bug、提出功能建议
- 提交 PR 修复问题、优化代码、新增能力
- 开发适配不同场景的功能插件
- 参与文档翻译、使用教程编写

## 开源协议

本项目基于 **MIT** 协议开源，详见 [LICENSE](./LICENSE) 文件。

## 免责声明

1. 本项目仅用于合法的基层社区自治、业主公共事务协商等合规场景，严禁用于任何违反法律法规的活动。
2. 使用本项目所产生的所有行为与后果，由使用者自行承担，项目开发团队不承担相关法律责任。
3. 请使用者严格遵守所在地区的法律法规与物业管理相关规定，依法依规开展自治活动。

---

星星之火，可以燎原。

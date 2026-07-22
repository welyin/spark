# Spark Core —— Rust 内核重写方案与计划

> 状态：阶段②已完成（内核全模块 + Rust↔TS 互通验证通过），阶段③待启动
> 决策人：wely
> 目标：单一 Rust 内核，Tauri 双端壳（桌面 + iOS/Android），替换现有 TS 主进程

## 1. 背景与动机

现有实现为 Electron + TypeScript 主进程（`desktop/src/main/`）。随着产品要实际落地测试，
只有 PC 端的使用门槛过高（业主群体），移动端成为硬需求。PC 与移动端的内核逻辑几乎相同，
若各写一份将长期维护两套代码。因此决定：

- **终态 = 一个 Rust 内核 + 一套 Vue UI + 两个薄壳（桌面/移动）**
- TS 主进程最终整体删除，不做双内核并行维护
- 当前无生产用户，可硬切换，不做 LevelDB 数据迁移

## 2. 终态架构

```
┌─────────────────────────────────────────────┐
│ Vue UI（现有 renderer，基本复用）              │
│ window.electronAPI → invoke 适配层（签名不变） │
├─────────────────────────────────────────────┤
│ Tauri 2.x 壳（桌面 / iOS / Android）          │
│ 薄命令层：参数校验、事件转发                    │
├─────────────────────────────────────────────┤
│ spark-core（纯 Rust crate，不依赖壳层框架）     │
│ 身份 / 存储抽象 / 集合与同步策略 / 存证链       │
│ 组织与 K 副本 / 数据治理 / P2P（rust-libp2p） │
└─────────────────────────────────────────────┘
```

### 内核边界

**包含**：身份（BIP39/SLIP-0010/域身份/备份加密）、存储抽象 trait、集合 schema 注册与
LWW/append-only 裁决、版本向量、存证链、组织（邀请码/nodeInfoClaim/org-share/org-pull/K 副本）、
数据治理（用量统计/过期清理/域名 purge/水位线）、全套 P2P 协议（pubsub 消息、直连协议、
peer exchange、node announce、org recovery、NAT 穿透）。

**不包含**：UI、插件视图与插件 JS 逻辑（留在 WebView，SDK 的数据/签名调用桥到内核，
权限校验在内核侧）、自动更新、托盘等壳层能力。

### 平台差异

- 桌面：sled 或 SQLite 存储后端；常驻 P2P 节点；承担 K 副本。
- 移动：SQLite 存储后端；iOS 后台会被挂起 → **手机不计入 K 副本**，轻客户端定位，
  在线时同步，家庭台式机承担常驻副本。Android 先行。

## 3. 关键工程难点与对策

| 难点 | 对策 |
|---|---|
| canonical JSON 序列化（最高优先级） | JS `JSON.stringify` 与 serde_json 的 key 序/数字格式差异会导致存证哈希断裂。阶段①先从 TS 反向提取精确规则并生成 golden vectors，Rust 侧逐字节对齐 |
| 算法逐位对齐 | 身份派生、哈希、签名、版本向量裁决、存证链、邀请码/claim 格式，全部以 golden vectors 验收 |
| 多路复用器 | rust-libp2p 已弃 mplex，直接上 yamux；无互通包袱（TS 版将被替换） |
| 存储抽象 | `StorageBackend` trait：get/put/delete/prefix scan/batch；桌面 sled/SQLite，移动 SQLite |
| 插件 JS 宿主 | 插件视图留 WebView；SDK 桥到内核；投票插件开发时按新边界验证 |

### 身份模块精确规格（已从 TS 提取，实现时照此对齐）

- 派生路径 `m/44'/607'/0'/0'/0'`，BIP39 passphrase `'Polykey'`
- 256 位熵 24 词；词表 chinese_simplified，恢复兼容 english
- SLIP-0010：master = HMAC-SHA512(key="ed25519 seed", seed)；
  子节点 data = 0x00‖parent.key‖ser32(idx+0x80000000)BE，HMAC-SHA512(parent.chainCode, data)
- rootId = sha256hex(ed25519 publicKey)；keypair = nacl fromSeed(slipNode.key)
- 域身份：sha256(domain) → idxA/idxB = readUInt32BE(0/4) & 0x7fffffff → 路径 `root/${idxA}'/${idxB}'`
- v2 存储：scrypt(N=32768, r=8, p=1, keyLen=32, maxmem=64MB, salt 16B) + aes-256-gcm(iv 12B)
- v1 legacy：pbkdf2(210000, sha512, 32) + aes-256-cbc(iv 16B)（只读兼容）
- StoredRootIdentity 含 nickname（必填 ≤24 字符）/ avatar（data:image/ 前缀 ≤200KB，可空）

## 4. 五阶段计划

### 阶段① 协议规格 + golden vectors
- 在 `spec/` 逐模块写规格文档（identity / schema-sync / evidence / org / data-mgmt / p2p-messages）
- 从 TS 实现反向提取 golden vectors（JSON：输入 → 预期输出/哈希/密文结构）
- TS 侧先跑通向量提取脚本
- **验收**：每个算法至少 3 组向量，TS 测试全绿

### 阶段② spark-core 逐模块移植
- `cargo init` workspace：`identity → storage+schema/sync+evidence → org → data-mgmt → p2p`
- 每模块对 golden vectors 验收
- P2P：改造现有 p2p-lab，做 Rust↔TS 双端互通实验（overlay/invite/recovery 场景）
- **验收**：Rust 单测全绿 + 双端互通实验通过

### 阶段③ Tauri PC 壳切换
- 新建 Tauri 2.x 工程，命令层内嵌 spark-core
- renderer 加 invoke 适配层（接口签名不变）
- 跑全量测试后删除 TS 主进程
- **验收**：PC 端完整功能可用，双端（新旧机器不算，硬切）互连同步正常

### 阶段④ Tauri 移动壳
- Android 先行：存储后端、P2P 前台服务、邀请码扫码加入
- iOS 随后：后台约束适配（不计 K 副本、前台同步）
- **验收**：手机可注册/加入组织/查看与提交数据

### 阶段⑤ 回北极星
- 投票插件 + 存证导出 + 试点（按 wiki development_plan）

## 5. 纪律

- **TS 侧冻结**：重写期间只修 bug，不加新功能
- 每个阶段完成验收后再进下一阶段
- 内核 crate 不依赖 Tauri/Electron 任何 API，保证可独立测试

## 6. 目录规划

> monorepo：PC 端、移动端、内核统一在本目录（code/）下，单一 git 项目管理。

```
code/
  README.md          ← 本文档
  spec/              ← 阶段①：协议规格 + golden vectors
    identity.md sync-evidence.md p2p-messages.md org.md data-mgmt.md
    vectors/         ← JSON 向量文件
  core/              ← 阶段②：Rust 内核 crate（spark-core，lib）
    src/identity/ storage/ schema/ sync/ evidence/ org/ p2p/ data-mgmt/
    examples/lab_node.rs  ← p2p-lab interop 场景驱动例程
  desktop/           ← 阶段③：Tauri PC 壳（待建；renderer 复用现有 Vue UI）
  mobile/            ← 阶段④：Tauri 移动壳（待建，Android 先行）
```

注：仓库根另有旧 `desktop/`（Electron+TS，独立 git 仓库），阶段③切换完成后退役删除。

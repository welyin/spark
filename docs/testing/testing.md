# 测试体系

> 分层验收：字节级向量 → 模块单测 → 集成对跑 → 双端互通 → 市场 e2e → GUI 人工验证。任一层的协议行为变更，先改 spec 与向量，再改实现。

本文档已拆分为多个独立文档，按测试层级组织：

| 层级 | 文档 | 一句话说明 |
| --- | --- | --- |
| 字节级 | [golden-vectors](golden-vectors.md) | canonical JSON 序列化、身份派生、版本向量、dm 信封等线形向量的逐字节对齐 |
| 模块单测 + 集成 | [rust-unit-integration](rust-unit-integration.md) | core 模块单测、kernel 集成测试、跨进程 e2e（stdio 驱动真实 kernel 实例） |
| 双端互通 | `rust-ts-interop`（执行记录，wiki testing/） | Rust↔TS 双端互通实验记录（阶段②收官，PASS） |
| 前端单测 | [renderer-vitest](renderer-vitest.md) | vitest：invoke 适配层、插件桥/沙箱、市场探索、消息/通讯录 mock 接入层 |
| 市场 e2e | [market-e2e](market-e2e.md) | 插件市场端到端（file:// 链路）：安装/更新/校验/侧载/卸载 |
| 组织 O2 联调 | `org-o2-acceptance`（验收报告，wiki testing/） | 双端/三端真实联调验收（orgsync 折叠/墓碑/orgq 受理，2026-09-02 PASS） |
| GUI 人工验证 | [gui-verification-checklist](gui-verification-checklist.md) | 阶段③收尾清单：身份/组织/消息/通讯录/市场/插件沙箱/设置/数据治理 |
| 真实网络 | [real-network-checklist](real-network-checklist.md) | UPnP/DCUtR/AutoNAT/IPv6 试点前验证清单（真机实测记录留 wiki testing/real-network-verification.md） |

> 原 §8「Android 真机调试（Windows）」已移至 `wiki troubleshooting/android-debug-windows.md`，因其内容主体为环境搭建与踩坑记录，不属于测试体系。

# 测试规范（docs/testing）

> 测试体系与方法规范：测试分几层、怎么写、覆盖范围要求。
> 测试执行记录、验收报告、真机验证结果留在 wiki `testing/` 区。
> 归属裁决见 [../README.md](../README.md) §三。

## 索引

- [testing.md](testing.md) —— 测试体系总览：分层验收（字节级向量 → 模块单测 → 集成对跑 → 双端互通 → 市场 e2e → GUI 人工验证）与各层文档导航
- [golden-vectors.md](golden-vectors.md) —— 字节级向量规范：每算法至少 3 组、Rust 逐字节对齐、「先改向量再改实现」原则与执行入口
- [rust-unit-integration.md](rust-unit-integration.md) —— core 模块单测、kernel 集成测试、壳层命令层测试、跨进程 e2e（stdio 驱动真实 kernel 实例）的运行方法
- [renderer-vitest.md](renderer-vitest.md) —— 前端 vitest：invoke 适配层、插件桥/沙箱、市场探索、消息/通讯录 mock 接入层
- [market-e2e.md](market-e2e.md) —— 插件市场端到端（file:// 链路）可复用执行命令：安装/更新/校验/侧载/卸载
- [gui-verification-checklist.md](gui-verification-checklist.md) —— GUI 人工验证清单：身份/组织/消息/通讯录/市场/插件沙箱/设置/数据治理/自动更新
- [real-network-checklist.md](real-network-checklist.md) —— 试点前真实网络验证清单：UPnP/DCUtR/AutoNAT/IPv6 四项（实测记录留 wiki）

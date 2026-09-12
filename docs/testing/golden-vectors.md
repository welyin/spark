# Golden vectors（字节级对齐的根基）

> 原则：任一层的协议行为变更，**先改 spec 与向量，再改实现**（协议规格本体见 [../protocol/](../protocol/)）。

- 标准：每个算法至少 3 组向量，Rust 侧逐字节对齐；
- 执行入口：`code/core` 的 `tests/identity_vectors.rs` / `org_vectors.rs` / `sync_evidence_vectors.rs` / `dm_envelope_vectors.rs` 等 `*_vectors.rs` 随 cargo test 自动跑；
- 向量清单以 code/spec/vectors/ 目录为事实源（现行 15 个）。

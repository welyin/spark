//! core 其余模块单元测试（自 `src/**` 迁移：只依赖 spark-core 公开 API 的用例）。

#[path = "unit_core/exporter.rs"]
mod exporter;
#[path = "unit_core/identity_file.rs"]
mod identity_file;
#[path = "unit_core/identity_slip10.rs"]
mod identity_slip10;
#[path = "unit_core/kernel_identity.rs"]
mod kernel_identity;
#[path = "unit_core/watermark.rs"]
mod watermark;

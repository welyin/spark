//! spark-app 单元测试（自 `src/**` 迁移：只依赖 spark-app 公开 API 的用例）。
//!
//! 直调 `pub(crate) fn *_inner` 或用例模块含 pub(crate)/私有项的测试保留在 src/ 下。

#[path = "unit_app/dto.rs"]
mod dto;
#[path = "unit_app/permissions.rs"]
mod permissions;
#[path = "unit_app/semver.rs"]
mod semver;

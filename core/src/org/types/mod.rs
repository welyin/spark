//! 组织记录类型与归一化规则（对齐 desktop/src/main/organization/types.ts 与
//! service.ts 的 normalize* 辅助函数）。
//!
//! 存储：键 `org:meta:<orgId>`（[`ORG_META_PREFIX`]），值 = 记录 JSON。
//!
//! ## 动态字段（extra）
//!
//! TS 的 `OrganizationRecord` 允许携带任意额外键（`recoverySecret` 就是其一）：
//! 快照构建时保留键之外的字段全部流入 `summary.metadata`（见 snapshot.rs）。
//! Rust 侧以 `#[serde(flatten)] extra` 捕获这些动态键——`recoverySecret` 与
//! `orgSecret`（org.md §13）因此**不是**具名字段，而是经
//! [`OrganizationRecord::recovery_secret`] / [`OrganizationRecord::org_secret`]
//! 等访问器读写的动态键，与 TS 行为逐键一致。
//!
//! 代码组织：存储键/归一化/id 与密钥生成在 `normalize`，成员角色/节点信息/
//! 成员记录与排序在 `member`，组织记录与同步版本/状态在 `record`，组织视图
//! （`toView`）在 `view`；单测在 `core/tests/unit_org/types.rs`。

mod member;
mod normalize;
mod record;
mod view;

pub use member::{
    OrganizationMember, OrganizationNodeInfo, OrganizationRole, normalize_node_info,
    normalize_optional_node_info, sort_members,
};
pub use normalize::{
    ORG_META_PREFIX, generate_org_secret, generate_organization_id, generate_recovery_secret,
    is_valid_root_id, normalize_plugin_domain, normalize_root_id, normalize_text, organization_key,
};
pub use record::{
    OrganizationRecord, OrganizationSyncSection, OrganizationSyncState, OrganizationSyncVersions,
};
pub use view::{OrganizationRecordFlattened, OrganizationView};

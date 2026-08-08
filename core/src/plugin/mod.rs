//! 插件后台运行时（QuickJS 沙箱）：内核承载插件常驻逻辑的 JS 引擎层。
//!
//! 边界原则：内核不认识 AI/CLI 等任何具体业务，只提供「运行插件代码 +
//! 白名单能力」这一抽象；引擎（QuickJS，纯 C 嵌入，二进制增量约 1~2MB）
//! 对本模块而言是通用基础设施，地位等同网络栈。
//!
//! - 单插件线程与 QuickJS 实例、熔断与 JS 桥见 [`runtime`]；
//! - 宿主能力面（存储镜像、消息回复等 capability 分发）见 [`host_env`]；
//! - 运行注册表与 bot 会话消息路由见 [`registry`]；
//! - kernel 门面（启停/状态/事件路由接线）见 `kernel::plugin_ops`。

mod error;
mod host_env;
mod registry;
mod runtime;

pub use error::PluginError;

pub(crate) use error::Result;
pub(crate) use host_env::PluginHostShared;
pub(crate) use registry::{PluginRuntimeRegistry, is_valid_plugin_id};
pub(crate) use runtime::spawn_plugin_runtime;

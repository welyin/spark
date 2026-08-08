//! spark-core：Spark Rust 内核。

pub mod identity;

pub mod collection;
pub mod contact;
#[path = "data-mgmt/mod.rs"]
pub mod data_mgmt;
pub mod device;
pub mod evidence;
pub mod kernel;
pub mod message;
pub mod org;
pub mod p2p;
pub mod plugin;
pub mod schema;
pub mod storage;
pub mod sync;

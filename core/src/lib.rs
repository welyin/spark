//! spark-core：Spark Rust 内核。

pub mod identity;

pub mod collection;
#[path = "data-mgmt/mod.rs"]
pub mod data_mgmt;
pub mod evidence;
pub mod kernel;
pub mod org;
pub mod p2p;
pub mod schema;
pub mod storage;
pub mod sync;

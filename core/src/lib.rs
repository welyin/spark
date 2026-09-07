//! spark-core：Spark Rust 内核。

pub mod identity;

pub mod affair;
pub mod collection;
pub mod contact;
pub mod content;
pub mod credential;
#[path = "data-mgmt/mod.rs"]
pub mod data_mgmt;
pub mod device;
pub mod dm_e2e;
pub mod dm_offline;
pub mod epoch;
pub mod evidence;
pub mod index;
pub mod kernel;
pub mod log_bridge;
pub mod message;
pub mod org;
pub mod p2p;
pub mod plugin;
pub mod plugindata;
pub mod policy;
pub mod pw;
pub mod recovery;
pub mod schema;
pub mod storage;
pub mod sync;
pub mod sys;

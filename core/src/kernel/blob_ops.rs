//! blob 层（A1 内容寻址 / A2 配额驱逐 / A3 健康度）的 `Kernel` 门面：
//! 壳层命令与后续 UI 的入口。只读聚合 + 配额配置，不含处置动作
//! （副本不足只提醒不处置，personal-data §4.5/Q06 口径）。

use super::{Kernel, Result};
use crate::sync::blob::{self, BlobHealth};

impl Kernel {
    /// 域级副本健康度摘要（确定性聚合；任何节点凭相同账本复算一致）。
    pub fn blob_health(&self) -> Result<BlobHealth> {
        Ok(blob::blob_health(self.require_storage()?)?)
    }

    /// 本机生效 blob 配额（字节；用户配置或设备类默认，协议 §15.1）。
    pub fn get_blob_quota(&self) -> Result<u64> {
        Ok(blob::get_blob_quota(self.require_storage()?)?)
    }

    /// 设置用户 blob 配额（字节；`None` 清除配置回落设备类默认）。
    pub fn set_blob_quota(&mut self, bytes: Option<u64>) -> Result<()> {
        Ok(blob::set_blob_quota(self.require_storage_mut()?, bytes)?)
    }
}

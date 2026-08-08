//! 通讯录门面（标签 / 分组 / 组织分组树）：`Kernel` 的薄封装命令，
//! id 一律由前端生成传入（contact 服务层的 `*_with_id` 变体落库）。
//!
//! 自 `contact_ops.rs` 拆出（文件长度上限，§2.1），同属 [`Kernel`] 的
//! 通讯录 API；好友申请出站/入站编排在 `contact_ops.rs`。

use super::{Kernel, Result};
use crate::contact::{ContactGroup, ContactService, ContactTag, OrgGroupNode};
use crate::p2p::node::system_now_ms;

impl Kernel {
    /// 新建标签。个人空间变更后向自设备广播 contact-sync 快照。
    pub fn contact_tag_create(&mut self, space: &str, id: &str, name: &str) -> Result<ContactTag> {
        let __io = std::sync::Arc::clone(&self.io_lock);
        let _io = __io.lock().unwrap_or_else(|e| e.into_inner());
        let tag = ContactService::create_tag_with_id(
            self.require_storage_mut()?,
            space,
            id,
            name,
            system_now_ms(),
        )?;
        if space == "personal" {
            self.broadcast_contact_sync();
        }
        Ok(tag)
    }

    /// 重命名标签。
    pub fn contact_tag_rename(&mut self, space: &str, id: &str, name: &str) -> Result<()> {
        let __io = std::sync::Arc::clone(&self.io_lock);
        let _io = __io.lock().unwrap_or_else(|e| e.into_inner());
        ContactService::rename_tag(self.require_storage_mut()?, space, id, name, system_now_ms())?;
        if space == "personal" {
            self.broadcast_contact_sync();
        }
        Ok(())
    }

    /// 删除标签（从所有资料中摘除）。
    pub fn contact_tag_delete(&mut self, space: &str, id: &str) -> Result<()> {
        let __io = std::sync::Arc::clone(&self.io_lock);
        let _io = __io.lock().unwrap_or_else(|e| e.into_inner());
        ContactService::delete_tag(self.require_storage_mut()?, space, id, system_now_ms())?;
        if space == "personal" {
            self.broadcast_contact_sync();
        }
        Ok(())
    }

    /// 新建个人空间扁平分组。
    pub fn contact_group_create(&mut self, id: &str, name: &str) -> Result<ContactGroup> {
        let __io = std::sync::Arc::clone(&self.io_lock);
        let _io = __io.lock().unwrap_or_else(|e| e.into_inner());
        let group = ContactService::create_group_with_id(
            self.require_storage_mut()?,
            id,
            name,
            system_now_ms(),
        )?;
        self.broadcast_contact_sync();
        Ok(group)
    }

    /// 重命名分组。
    pub fn contact_group_rename(&mut self, id: &str, name: &str) -> Result<()> {
        let __io = std::sync::Arc::clone(&self.io_lock);
        let _io = __io.lock().unwrap_or_else(|e| e.into_inner());
        ContactService::rename_group(self.require_storage_mut()?, id, name, system_now_ms())?;
        self.broadcast_contact_sync();
        Ok(())
    }

    /// 删除分组（组内朋友复位为未分组）。
    pub fn contact_group_delete(&mut self, id: &str) -> Result<()> {
        let __io = std::sync::Arc::clone(&self.io_lock);
        let _io = __io.lock().unwrap_or_else(|e| e.into_inner());
        ContactService::delete_group(self.require_storage_mut()?, id, system_now_ms())?;
        self.broadcast_contact_sync();
        Ok(())
    }

    /// 拖拽重排分组（越界夹紧）。
    pub fn contact_group_move(&mut self, id: &str, to_index: usize) -> Result<()> {
        let __io = std::sync::Arc::clone(&self.io_lock);
        let _io = __io.lock().unwrap_or_else(|e| e.into_inner());
        ContactService::move_group(self.require_storage_mut()?, id, to_index, system_now_ms())?;
        self.broadcast_contact_sync();
        Ok(())
    }

    /// 新建组织分组（`parent_id` 为 `""` 挂根层；父不存在返回 `Ok(None)`）。
    pub fn contact_org_group_create(
        &mut self,
        space: &str,
        parent_id: &str,
        id: &str,
        name: &str,
    ) -> Result<Option<OrgGroupNode>> {
        let __io = std::sync::Arc::clone(&self.io_lock);
        let _io = __io.lock().unwrap_or_else(|e| e.into_inner());
        Ok(ContactService::create_org_group_with_id(
            self.require_storage_mut()?,
            space,
            parent_id,
            id,
            name,
        )?)
    }

    /// 重命名组织分组。
    pub fn contact_org_group_rename(&mut self, space: &str, id: &str, name: &str) -> Result<()> {
        let __io = std::sync::Arc::clone(&self.io_lock);
        let _io = __io.lock().unwrap_or_else(|e| e.into_inner());
        ContactService::rename_org_group(self.require_storage_mut()?, space, id, name)?;
        Ok(())
    }

    /// 删除组织分组（子节点提升一层）。
    pub fn contact_org_group_delete(&mut self, space: &str, id: &str) -> Result<()> {
        let __io = std::sync::Arc::clone(&self.io_lock);
        let _io = __io.lock().unwrap_or_else(|e| e.into_inner());
        ContactService::delete_org_group(self.require_storage_mut()?, space, id)?;
        Ok(())
    }

    /// 拖拽移动组织分组：`new_parent_id` 缺省（None）为同级重排（向后兼容）；
    /// `Some(parent)` 为跨级移动（`""` = 根层），成环/目标父不存在时静默忽略
    /// （口径见 `ContactService::move_org_group`）。
    pub fn contact_org_group_move(
        &mut self,
        space: &str,
        id: &str,
        to_index: usize,
        new_parent_id: Option<&str>,
    ) -> Result<()> {
        let __io = std::sync::Arc::clone(&self.io_lock);
        let _io = __io.lock().unwrap_or_else(|e| e.into_inner());
        match new_parent_id {
            None => {
                ContactService::move_org_group_sibling(self.require_storage_mut()?, space, id, to_index)?;
            }
            Some(parent_id) => {
                ContactService::move_org_group(
                    self.require_storage_mut()?,
                    space,
                    id,
                    parent_id,
                    to_index,
                )?;
            }
        }
        Ok(())
    }
}

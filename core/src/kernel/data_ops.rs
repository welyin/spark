//! P6 声明式数据 API 的 `Kernel` 门面（iframe 桥 / Tauri 命令侧入口；
//! QuickJS 后台运行时侧的同语义入口在 `plugin::host_env` 的 `data.*`
//! capability）。两侧共享 `plugindata` 模块语义，写入均经版本化句柄
//! （写库即同步）。
//!
//! `domain` 参数沿用桥层口径（`plugin:{pluginId}` 或裸 pluginId），本模块
//! 统一归一为裸 pluginId 再做前缀归属校验。

use serde_json::Value;

use super::{Kernel, Result};
use crate::plugindata::{self, DeclareInput};

/// 桥层 domain → 裸 pluginId（`plugin:ai-chat` → `ai-chat`；无前缀原样）。
fn plugin_id_of(domain: &str) -> &str {
    domain.strip_prefix("plugin:").unwrap_or(domain)
}

/// 归属校验 + 声明解析：插件只能触达自己前缀的集合（与 host_env 同口径）。
fn resolve_owned<S: crate::storage::StorageBackend>(
    storage: &S,
    domain: &str,
    name: &str,
    version: Option<&str>,
) -> Result<crate::plugindata::CollectionDeclaration> {
    let plugin_id = plugin_id_of(domain);
    if !name.starts_with(&format!("{plugin_id}:")) {
        return Err(crate::plugindata::PlugindataError::NamePrefixMismatch {
            name: name.to_string(),
            plugin_id: plugin_id.to_string(),
        }
        .into());
    }
    Ok(plugindata::resolve(storage, name, version)?)
}

impl Kernel {
    /// 声明集合（幂等；代际内策略冲突报错）。返回声明记录。
    pub fn data_declare_collection(
        &mut self,
        domain: &str,
        input: DeclareInput,
    ) -> Result<crate::plugindata::CollectionDeclaration> {
        let storage = self.require_storage_mut()?;
        Ok(plugindata::declare(
            storage,
            plugin_id_of(domain),
            input,
            crate::p2p::node::system_now_ms(),
        )?)
    }

    /// 写记录（version 缺省 = 最新代际）。
    pub fn data_save(
        &mut self,
        domain: &str,
        name: &str,
        key: &str,
        value: Value,
        version: Option<&str>,
    ) -> Result<()> {
        let decl = resolve_owned(self.require_storage()?, domain, name, version)?;
        let storage = self.require_storage_mut()?;
        Ok(plugindata::save(storage, &decl, key, &value.to_string())?)
    }

    /// 删记录（墓碑传播）。
    pub fn data_delete(
        &mut self,
        domain: &str,
        name: &str,
        key: &str,
        version: Option<&str>,
    ) -> Result<()> {
        let decl = resolve_owned(self.require_storage()?, domain, name, version)?;
        let storage = self.require_storage_mut()?;
        Ok(plugindata::del(storage, &decl, key)?)
    }

    /// 读单条（未命中 → None）。
    pub fn data_get(
        &self,
        domain: &str,
        name: &str,
        key: &str,
        version: Option<&str>,
    ) -> Result<Option<Value>> {
        let decl = resolve_owned(self.require_storage()?, domain, name, version)?;
        let raw = plugindata::get(self.require_storage()?, &decl, key)?;
        Ok(match raw {
            Some(text) => Some(serde_json::from_str(&text).unwrap_or(Value::String(text))),
            None => None,
        })
    }

    /// 前缀分页查询。
    pub fn data_query(
        &self,
        domain: &str,
        name: &str,
        prefix: Option<&str>,
        limit: Option<usize>,
        cursor: Option<&str>,
        version: Option<&str>,
    ) -> Result<crate::plugindata::QueryPage> {
        let decl = resolve_owned(self.require_storage()?, domain, name, version)?;
        Ok(plugindata::query(
            self.require_storage()?,
            &decl,
            prefix,
            limit,
            cursor,
        )?)
    }

    /// 清理一个代际（声明 + 全部数据键墓碑化传播）。
    pub fn data_drop_version(&mut self, domain: &str, name: &str, version: &str) -> Result<()> {
        let decl = resolve_owned(self.require_storage()?, domain, name, Some(version))?;
        let storage = self.require_storage_mut()?;
        Ok(plugindata::drop_version(storage, &decl)?)
    }

    /// blob 保存（base64 入、内容哈希出）。
    pub fn data_save_blob(&mut self, data_base64: &str) -> Result<crate::plugindata::blob::BlobInfo> {
        use base64::Engine as _;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(data_base64)
            .map_err(|e| crate::plugindata::PlugindataError::Blob(format!("invalid base64: {e}")))?;
        let storage = self.require_storage_mut()?;
        Ok(plugindata::blob::save_blob(storage, &bytes)?)
    }

    /// blob 读取（命中 → base64；未命中 → 置 want 标记后返回 None——lazy
    /// 拉取由 pdsync hello 调和完成，调用方稍后重读）。
    pub fn data_read_blob(&mut self, hash: &str) -> Result<Option<String>> {
        if let Some(data) = plugindata::blob::read_blob(self.require_storage()?, hash)? {
            return Ok(Some(data));
        }
        let storage = self.require_storage_mut()?;
        plugindata::blob::mark_want(storage, hash)?;
        Ok(None)
    }
}

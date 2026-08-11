//! O3/O4 成员侧读路径（orgq 缓存 + encrypted 解密）`Kernel` 辅助。
//!
//! 从 `data_ops.rs` 拆出（650 行硬线）：`data_get`/`data_query` 在非数据账号
//! 成员读 org data-accounts 集合时走成员侧缓存（`orgq:cache:`），encrypted
//! 集合的缓存值/本地值为密文，读路径统一解密为明文。本模块只放读辅助，写/
//! 路由编排仍在 `data_ops.rs`。
//!
//! - [`Self::decrypt_orgd_read_value`]：本地 `orgd:` 密文解密（O4 工作项 4）；
//! - [`Self::data_orgq_cached_get`] / [`Self::data_orgq_cached_query`]：成员侧
//!   缓存读取 + encrypted 解密（O3 读路径 + O4 加密）。

use serde_json::Value;

use super::{Kernel, Result};

impl Kernel {
    /// O4 工作项 4：encrypted 集合本地读解密——`plugindata::get` 返回 `orgd:`
    /// 密文 `{epoch,nonce,ct}`，按记录 epoch 取密钥解密为插件明文 JSON 值。
    /// filtered / personal 集合原样透传。无该 epoch 密钥（非 reader 或历史
    /// epoch 未达）→ `Err(KeyUnavailable)`（plugin-data-api §8）。
    pub(crate) fn decrypt_orgd_read_value(
        &self,
        decl: &crate::plugindata::CollectionDeclaration,
        key: &str,
        stored: &str,
    ) -> Result<Value> {
        if decl.confidentiality != crate::plugindata::Confidentiality::Encrypted {
            return Ok(serde_json::from_str(stored).unwrap_or(Value::String(stored.to_string())));
        }
        let Some(oid) = decl.org_id.as_deref() else {
            // encrypted 仅 org scope；无 org_id 视为无法解密（不应发生）
            return Ok(serde_json::from_str(stored).unwrap_or(Value::String(stored.to_string())));
        };
        let plain = crate::sync::orgsync::decrypt_orgd_value(
            self.require_storage()?,
            oid,
            &decl.name,
            &decl.version,
            key,
            stored,
        )
        .map_err(|e| super::KernelError::Internal(format!("decrypt {key}: {e}")))?;
        Ok(serde_json::from_str(&plain).unwrap_or(Value::String(plain)))
    }

    /// O3 读路径：非数据账号成员读 org data-accounts 集合时先查成员侧缓存
    /// （`orgq:cache:` 命名空间，可淘汰）。命中返回缓存值；未命中返回 `None`
    /// （由调用方决定 Offline 回缓存/UnavailableOffline 语义）。
    pub(crate) fn data_orgq_cached_get<S: crate::storage::StorageBackend>(
        &self,
        storage: &S,
        decl: &crate::plugindata::CollectionDeclaration,
        key: &str,
    ) -> Result<Option<Value>> {
        let col_full = format!("{}@v{}", decl.name, decl.version);
        let relative = key
            .strip_prefix(&crate::plugindata::org_data_prefix(
                decl.org_id.as_deref().unwrap_or(""),
                &decl.name,
                &decl.version,
            ))
            .unwrap_or(key);
        let cache_key = crate::sync::orgsync::orgq_cache_key(
            decl.org_id.as_deref().unwrap_or(""),
            &col_full,
            relative,
        );
        let raw = storage.get(&cache_key)?;
        Ok(raw.map(|r| {
            // 缓存值 = OrgqRespRecord JSON，取其中 value 字段
            let val = serde_json::from_str::<crate::sync::orgsync::OrgqRespRecord>(&r)
                .map(|rec| rec.value)
                .unwrap_or_else(|_| serde_json::from_str(&r).unwrap_or(Value::Null));
            // O4 工作项 4：encrypted 集合缓存值为密文，取记录 epoch 密钥解密
            // 为明文（成员本地解密，plugin-data-api §3）。解密失败（非 reader/
            // 密钥未达）→ 返回 Null（UnavailableOffline 语义）。
            if decl.confidentiality == crate::plugindata::Confidentiality::Encrypted {
                if let Some(oid) = decl.org_id.as_deref() {
                    // 密文序列化回串（rec.value 为 {epoch,nonce,ct} 对象）
                    let ct_str = if let Value::String(s) = &val {
                        s.clone()
                    } else {
                        serde_json::to_string(&val).unwrap_or_default()
                    };
                    if let Ok(plain) = crate::sync::orgsync::decrypt_orgd_value(
                        storage,
                        oid,
                        &decl.name,
                        &decl.version,
                        &relative,
                        &ct_str,
                    ) {
                        return serde_json::from_str(&plain).unwrap_or(Value::String(plain));
                    }
                    return Value::Null;
                }
            }
            val
        }))
    }

    /// O3 读路径：成员侧缓存前缀扫描（`orgq:cache:` 命名空间）。返回分页
    /// 页（相对键 → 缓存值），无缓存返回空页。
    pub(crate) fn data_orgq_cached_query<S: crate::storage::StorageBackend>(
        &self,
        storage: &S,
        decl: &crate::plugindata::CollectionDeclaration,
        prefix: Option<&str>,
        limit: Option<usize>,
        cursor: Option<&str>,
    ) -> Result<crate::plugindata::QueryPage> {
        let oid = decl.org_id.as_deref().unwrap_or("");
        let col_full = format!("{}@v{}", decl.name, decl.version);
        let cache_prefix = crate::sync::orgsync::orgq_cache_prefix(oid, &col_full);
        let match_prefix = match prefix {
            Some(p) => format!("{cache_prefix}{p}"),
            None => cache_prefix.clone(),
        };
        let scan_from = match cursor {
            Some(c) => format!("{cache_prefix}{c}\u{0}"),
            None => match_prefix.clone(),
        };
        let limit = limit
            .unwrap_or(crate::sync::orgsync::ORGQ_LIMIT_DEFAULT)
            .clamp(1, crate::sync::orgsync::ORGQ_LIMIT_MAX);
        let options = crate::storage::ScanOptions {
            prefix: match_prefix,
            start: Some(scan_from),
            end: None,
            limit: Some(limit),
            reverse: false,
        };
        let mut page = crate::plugindata::QueryPage::default();
        for (key, raw) in storage.scan(&options)? {
            let relative = key[cache_prefix.len()..].to_string();
            let mut value = serde_json::from_str::<crate::sync::orgsync::OrgqRespRecord>(&raw)
                .map(|rec| rec.value.to_string())
                .unwrap_or_else(|_| raw.clone());
            // O4 工作项 4：encrypted 集合缓存值为密文，取记录 epoch 密钥解密
            // 为明文。解密失败（非 reader/密钥未达）→ 该条置 null。
            if decl.confidentiality == crate::plugindata::Confidentiality::Encrypted {
                if let Ok(plain) = crate::sync::orgsync::decrypt_orgd_value(
                    storage,
                    oid,
                    &decl.name,
                    &decl.version,
                    &relative,
                    &value,
                ) {
                    value = plain;
                } else {
                    value = "null".to_string();
                }
            }
            page.items.push((relative.clone(), value));
            if page.items.len() >= limit {
                page.next_cursor = Some(relative);
                break;
            }
        }
        Ok(page)
    }
}

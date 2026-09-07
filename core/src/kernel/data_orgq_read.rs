//! O3 成员侧读路径（orgq 缓存）`Kernel` 辅助。
//!
//! 从 `data_ops.rs` 拆出（650 行硬线）：`data_get`/`data_query` 在非数据账号
//! 成员读 org data-accounts 集合时走成员侧缓存（`orgq:cache:`）。本模块只放
//! 读辅助，写/路由编排仍在 `data_ops.rs`。
//!
//! C7：O4 encrypted 集合解密面已随 `encrypted` 轴退役移除（orgd 值恒为明文）。

use serde_json::Value;

use super::{Kernel, Result};

impl Kernel {
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
            // 缓存值 = OrgqRespRecord JSON，取其中 value 字段（C7 后恒为明文）
            serde_json::from_str::<crate::sync::orgsync::OrgqRespRecord>(&r)
                .map(|rec| rec.value)
                .unwrap_or_else(|_| serde_json::from_str(&r).unwrap_or(Value::Null))
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
            let value = serde_json::from_str::<crate::sync::orgsync::OrgqRespRecord>(&raw)
                .map(|rec| rec.value.to_string())
                .unwrap_or_else(|_| raw.clone());
            page.items.push((relative.clone(), value));
            if page.items.len() >= limit {
                page.next_cursor = Some(relative);
                break;
            }
        }
        Ok(page)
    }
}

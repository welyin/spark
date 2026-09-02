//! 数据命令（P6 声明式数据 API 直通内核门面）：data-declare / data-save /
//! data-delete / data-get / data-query / data-grant-access / org-fold-vv。
//!
//! `domain` 为插件域（裸 pluginId，如 "e2e"），集合名须带其前缀
//! （`e2e:ledger`）。org 集合传 `orgId`；personal 集合省略。

use serde_json::{Value, json};
use spark_core::kernel::Kernel;
use spark_core::plugindata::{
    Accounts, Confidentiality, DeclareInput, MergeRule, Space,
};

use crate::dispatch::{Params, to_json};

/// `data-declare`：声明集合（幂等）。space 缺省按 orgId 有无推导。
pub fn declare(kernel: &mut Kernel, params: &Params) -> Result<Value, String> {
    let org_id = params.opt_str("orgId");
    let space = match params.opt_str("space") {
        Some("org") => Some(Space::Org),
        Some("personal") => Some(Space::Personal),
        Some(other) => return Err(format!("unknown space: {other}")),
        None => org_id.map(|_| Space::Org),
    };
    let accounts = match params.opt_str("accounts") {
        Some("data-accounts") => Some(Accounts::DataAccounts),
        Some("all-members") => Some(Accounts::AllMembers),
        Some(other) => return Err(format!("unknown accounts: {other}")),
        None => None,
    };
    let confidentiality = match params.opt_str("confidentiality") {
        Some("filtered") => Some(Confidentiality::Filtered),
        Some("encrypted") => Some(Confidentiality::Encrypted),
        Some(other) => return Err(format!("unknown confidentiality: {other}")),
        None => None,
    };
    let merge = match params.opt_str("merge") {
        Some("lww-record") => Some(MergeRule::LwwRecord),
        Some("append-only") => Some(MergeRule::AppendOnly),
        Some("whole") => Some(MergeRule::Whole),
        Some(other) => return Err(format!("unknown merge: {other}")),
        None => None,
    };
    let input = DeclareInput {
        name: params.need_str("name")?.to_string(),
        version: params.opt_str("version").map(ToString::to_string),
        space,
        accounts,
        confidentiality,
        merge,
        ..Default::default()
    };
    to_json(kernel.data_declare_collection(params.need_str("domain")?, input, org_id))
}

/// `data-save`：写记录（value 为任意 JSON）。
pub fn save(kernel: &mut Kernel, params: &Params) -> Result<Value, String> {
    let value = params
        .opt_value("value")
        .cloned()
        .ok_or_else(|| "missing param: value".to_string())?;
    kernel
        .data_save(
            params.need_str("domain")?,
            params.need_str("name")?,
            params.need_str("key")?,
            value,
            params.opt_str("version"),
            params.opt_str("orgId"),
        )
        .map_err(|e| e.to_string())?;
    Ok(json!({"saved": true}))
}

/// `data-delete`：删记录（墓碑传播）。
pub fn delete(kernel: &mut Kernel, params: &Params) -> Result<Value, String> {
    kernel
        .data_delete(
            params.need_str("domain")?,
            params.need_str("name")?,
            params.need_str("key")?,
            params.opt_str("version"),
            params.opt_str("orgId"),
        )
        .map_err(|e| e.to_string())?;
    Ok(json!({"deleted": true}))
}

/// `data-get`：读单条（未命中 → data 为 null）。
pub fn get(kernel: &Kernel, params: &Params) -> Result<Value, String> {
    let value = kernel
        .data_get(
            params.need_str("domain")?,
            params.need_str("name")?,
            params.need_str("key")?,
            params.opt_str("version"),
            params.opt_str("orgId"),
        )
        .map_err(|e| e.to_string())?;
    Ok(json!({"value": value}))
}

/// `data-query`：前缀分页查询 → {items: [{key, value}], nextCursor}。
pub fn query(kernel: &Kernel, params: &Params) -> Result<Value, String> {
    let page = kernel
        .data_query(
            params.need_str("domain")?,
            params.need_str("name")?,
            params.opt_str("prefix"),
            params.opt_value("limit").and_then(Value::as_u64).map(|n| n as usize),
            params.opt_str("cursor"),
            params.opt_str("version"),
            params.opt_str("orgId"),
        )
        .map_err(|e| e.to_string())?;
    let items: Vec<Value> = page
        .items
        .iter()
        .map(|(key, raw)| {
            json!({"key": key, "value": serde_json::from_str::<Value>(raw).unwrap_or(Value::Null)})
        })
        .collect();
    Ok(json!({"items": items, "nextCursor": page.next_cursor}))
}

/// `data-grant-access`：owner 将成员加入 encrypted 集合 readers
/// （创世首个 grant 自签 acl epoch=1，并向新读者投递密钥）。
pub fn grant_access(kernel: &mut Kernel, params: &Params) -> Result<Value, String> {
    let readers = params.opt_strings("readers").unwrap_or_default();
    to_json(kernel.data_grant_access(
        params.need_str("orgId")?,
        params.need_str("name")?,
        params.str_or("version", "1"),
        &readers,
    ))
}

/// `data-revoke-access`：owner 将成员移出 readers（epoch+1 轮换密钥）。
pub fn revoke_access(kernel: &mut Kernel, params: &Params) -> Result<Value, String> {
    let members = params.opt_strings("members").unwrap_or_default();
    to_json(kernel.data_revoke_access(
        params.need_str("orgId")?,
        params.need_str("name")?,
        params.str_or("version", "1"),
        &members,
    ))
}

/// `data-list-access`：读本机 acl 名单（{owners, readers, epoch}；无记录 → 空）。
pub fn list_access(kernel: &Kernel, params: &Params) -> Result<Value, String> {
    to_json(kernel.data_list_access(
        params.need_str("orgId")?,
        params.need_str("name")?,
        params.str_or("version", "1"),
    ))
}

/// `org-publish-access-key`：发布本人组织身份访问密钥到成员表（org:structure
/// 全员同步；orgkey-deliver 的前提——收件人无 accessKey 时投递跳过）。
pub fn publish_access_key(kernel: &mut Kernel, params: &Params) -> Result<Value, String> {
    kernel
        .org_publish_access_key(params.need_str("orgId")?)
        .map_err(|e| e.to_string())?;
    Ok(json!({"published": true}))
}

/// `org-fold-vv`：本机某 org 集合的折叠 vv（orgsync-hello 摘要同口径），
/// 供双端收敛断言。返回 {vv: {nodeId: counter}}。
pub fn fold_vv(kernel: &Kernel, params: &Params) -> Result<Value, String> {
    let storage = kernel
        .__test_storage()
        .ok_or_else(|| "storage not ready".to_string())?;
    let vv = spark_core::sync::orgsync::collect_org_collection_vv(
        &storage,
        params.need_str("orgId")?,
        params.need_str("name")?,
        params.str_or("version", "1"),
    )
    .map_err(|e| e.to_string())?;
    Ok(json!({"vv": vv}))
}

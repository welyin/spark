//! affair 日志加载（indexer 侧）：从本地副本读创世 + 已接受操作 + 存证链
//! 锚定时刻（§7.2 时间源），供公告验证复算（§4）与健康信号推导（§10 决策 4）。
//! 无日志副本是常态（纯 indexer 元数据面）——加载器以 Option 表达，
//! 调用方区分 Unverified 与健康信号缺席。

use std::collections::HashMap;

use serde_json::Value;

use crate::affair::{
    AFFAIR_OP_PREFIX, AffairGenesis, AffairOp, affair_record_key, parse_genesis, parse_op,
};
use crate::storage::{ScanOptions, StorageBackend};

use super::IndexError;

/// 存证锚定索引键：`{collection}:{id}`（§7.2 时间源；未锚定条目不参与时间推导）。
pub type AnchorKey = (String, String);

/// 已解析操作 + 链上锚定时刻。
pub struct LoadedOp {
    pub op_hash: String,
    pub parsed: AffairOp,
    pub anchored_ms: Option<i64>,
}

/// 本 affair 的本地日志视图。
pub struct LoadedLog {
    pub affair_id: String,
    pub genesis_value: Value,
    pub genesis: AffairGenesis,
    pub ops: Vec<LoadedOp>,
    pub anchors: HashMap<AnchorKey, i64>,
}

/// 加载本地 affair 日志（创世 + opHash 字典序操作 + 锚定表）；创世缺失
/// （无本地副本）返回 Ok(None)。
pub fn load_log<S: StorageBackend>(
    storage: &S,
    affair_id: &str,
) -> Result<Option<LoadedLog>, IndexError> {
    let Some(genesis_raw) = storage.get(&affair_record_key(affair_id))? else {
        return Ok(None);
    };
    let genesis_value: Value = serde_json::from_str(&genesis_raw)
        .map_err(|_| IndexError::CorruptLog(affair_id.to_string()))?;
    let genesis =
        parse_genesis(&genesis_value).map_err(|_| IndexError::CorruptLog(affair_id.to_string()))?;
    let anchors = anchor_map(storage, affair_id)?;
    let mut ops = Vec::new();
    for (key, raw) in storage.scan(&ScanOptions::prefix(&format!(
        "{AFFAIR_OP_PREFIX}{affair_id}:"
    )))? {
        let Some(op_hash) = key
            .strip_prefix(&format!("{AFFAIR_OP_PREFIX}{affair_id}:"))
            .map(ToString::to_string)
        else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(&raw) else {
            continue; // 损坏记录跳过（与 affair_ops::load_ops 同口径）
        };
        let Ok(parsed) = parse_op(&value) else {
            continue;
        };
        let anchored_ms = anchors.get(&("ops".to_string(), op_hash.clone())).copied();
        ops.push(LoadedOp {
            op_hash,
            parsed,
            anchored_ms,
        });
    }
    ops.sort_by(|a, b| a.op_hash.cmp(&b.op_hash));
    Ok(Some(LoadedLog {
        affair_id: affair_id.to_string(),
        genesis_value,
        genesis,
        ops,
        anchors,
    }))
}

/// 本副本存证链锚定时刻索引：`affair:{affairId}` 域内 (collection, id) →
/// 条目时间戳。O(链高) 顺序扫描（命令层最小实现；链高增长后的分页/索引
/// 属存证模块的独立优化项，与 kernel/affair_ops 同口径）。
fn anchor_map<S: StorageBackend>(
    storage: &S,
    affair_id: &str,
) -> Result<HashMap<AnchorKey, i64>, IndexError> {
    let domain = format!("affair:{affair_id}");
    let height = crate::evidence::get_evidence_height(storage)?;
    let mut map = HashMap::new();
    for seq in 1..=height {
        let Some(entry) = crate::evidence::get_evidence_entry(storage, seq)? else {
            continue;
        };
        if entry.domain == domain {
            map.insert((entry.collection, entry.id), entry.timestamp);
        }
    }
    Ok(map)
}

/// 有效异议计数：opType=objection 且 payload.target == 目标 opHash 的操作数
/// （§6.2 异议口径；已接受集 = 内容判定基础，与到达顺序无关）。
pub fn objection_counts(ops: &[LoadedOp]) -> HashMap<String, u64> {
    let mut counts: HashMap<String, u64> = HashMap::new();
    for op in ops {
        if op.parsed.op_type != crate::affair::OpType::Objection {
            continue;
        }
        if let Some(target) = op
            .parsed
            .payload
            .get("target")
            .and_then(Value::as_str)
            .map(ToString::to_string)
        {
            *counts.entry(target).or_default() += 1;
        }
    }
    counts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_genesis_yields_none() {
        let s = crate::storage::MemoryStorage::new();
        assert!(load_log(&s, &"a".repeat(64)).unwrap().is_none());
    }
}

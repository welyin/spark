//! 市场状态文件持久化（<app_data_dir>/plugin-market-state.json，
//! TS PersistedPluginState 同构；读写失败语义对齐 TS readJsonFile/writeJsonFile）。

use std::fs;
use std::path::Path;

use super::types::PersistedPluginState;

pub(crate) const PLUGIN_STATE_FILE: &str = "plugin-market-state.json";

/// TS `readJsonFile`：读取失败/解析失败一律回退默认值。
pub(crate) fn read_state_file(path: &Path) -> PersistedPluginState {
    fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

/// TS `writeJsonFile`：mkdir -p + JSON 两空格缩进。
pub(crate) fn write_state_file(path: &Path, state: &PersistedPluginState) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("{e}"))?;
    }
    let text = serde_json::to_string_pretty(state).map_err(|e| format!("{e}"))?;
    fs::write(path, text).map_err(|e| format!("{e}"))
}

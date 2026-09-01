//! 开发/自动化测试挂钩（**仅 debug 构建编译进壳层**，release 无本模块）。
//!
//! 用途：双端联调 / 真机回归时免去 UI 输密码——由测试驱动经
//! `adb shell run-as <pkg> sh -c 'echo ... > files/dev_instruction.json'`
//! （debug 包 run-as 可写；桌面端直接写 `$SPARK_DATA_DIR/dev_instruction.json`）
//! 投递一条登录指令，setup 阶段自动执行并写回 `dev_result.json` 供驱动轮询。
//!
//! 指令格式（JSON）：
//! - `{ "action": "init", "password": "...", "nickname": "..." }`
//!   新建账号；结果含 `mnemonic`（驱动据此可在第二端 recover）。
//! - `{ "action": "unlock", "password": "...", "rootId": "可选" }`
//! - `{ "action": "recover_mnemonic", "mnemonic": "24 词", "password": "...",
//!     "nickname": "..." }`
//!
//! 安全边界：debug_assertions 编译期裁剪 + 文件位于应用私有目录
//! （仅 run-as/root 可写），不进任何对外协议。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Deserialize;
use spark_core::kernel::Kernel;
use spark_core::storage::StorageBackend;

#[derive(Deserialize)]
struct DevInstruction {
    action: String,
    password: Option<String>,
    root_id: Option<String>,
    mnemonic: Option<String>,
    nickname: Option<String>,
    peer_id: Option<String>,
    addresses: Option<Vec<String>>,
    prefix: Option<String>,
}

const INSTRUCTION_FILE: &str = "dev_instruction.json";
const RESULT_FILE: &str = "dev_result.json";

/// setup 尾段调用：有指令则执行并写回结果，无指令零成本返回。
pub fn run(data_dir: &Path, kernel: &mut Kernel) {
    consume_and_execute(data_dir, kernel);
}

/// 轮询模式：后台线程周期检查数据目录下的 dev_instruction.json，存在即
/// 消费执行并写回 dev_result.json。用于免重启热查询连接级状态（如
/// relay_status 的预约/共享池）——setup 消费只发生在启动瞬间，彼时
/// p2p 连接尚未建立，查不到运行期状态。
pub fn spawn_polling(data_dir: PathBuf, kernel: Arc<Mutex<Kernel>>) {
    std::thread::Builder::new()
        .name("dev-harness-poll".into())
        .spawn(move || loop {
            std::thread::sleep(Duration::from_secs(3));
            if !data_dir.join(INSTRUCTION_FILE).exists() {
                continue;
            }
            let mut guard = kernel.lock().unwrap_or_else(|e| e.into_inner());
            consume_and_execute(&data_dir, &mut guard);
        })
        .expect("spawn dev-harness-poll thread");
}

fn consume_and_execute(data_dir: &Path, kernel: &mut Kernel) {
    let path = data_dir.join(INSTRUCTION_FILE);
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return;
    };
    // 无论成败都先摘除指令，防止重启后重复执行。
    let _ = std::fs::remove_file(&path);
    eprintln!("[dev-harness] instruction consumed ({} bytes)", raw.len());
    let result = match serde_json::from_str::<DevInstruction>(&raw) {
        Ok(ins) => execute(kernel, &ins),
        Err(e) => Err(format!("invalid {INSTRUCTION_FILE}: {e}")),
    };
    let body = result.unwrap_or_else(|e| serde_json::json!({ "ok": false, "error": e }).to_string());
    match std::fs::write(data_dir.join(RESULT_FILE), body.as_bytes()) {
        Ok(()) => eprintln!("[dev-harness] result written ({} bytes)", body.len()),
        Err(e) => eprintln!("[dev-harness] result WRITE FAILED: {e}"),
    }
}

fn execute(kernel: &mut Kernel, ins: &DevInstruction) -> Result<String, String> {
    // pair_peer 需要解锁态，而重启后必为锁定态——同一指令携带 password 时
    // 先解锁再配对（单指令完成「解锁 + 配对」，免两轮重启）。
    // 需解锁态的动作（pair_peer/relay_status/dump 等）：携带 password 时先
    // 解锁（重启后必为锁定态，单指令完成「解锁 + 动作」，幂等——已解锁时
    // unlock 报错忽略）。
    if ins.action != "init"
        && ins.action != "recover_mnemonic"
        && ins.action != "unlock"
    {
        if let Some(password) = ins.password.as_deref() {
            if let Err(e) = kernel.unlock(password, None) {
                eprintln!("[dev-harness] 预解锁失败（继续执行动作）: {e}");
            }
        }
    }
    match ins.action.as_str() {
        "init" => {
            let password = required(ins.password.as_deref(), "password")?;
            let nickname = ins.nickname.as_deref().unwrap_or("dev-test");
            let out = kernel
                .init_identity(password, nickname, None)
                .map_err(|e| e.to_string())?;
            Ok(serde_json::json!({
                "ok": true,
                "action": "init",
                "rootId": out.root_id,
                "mnemonic": out.mnemonic,
            })
            .to_string())
        }
        "unlock" => {
            let password = required(ins.password.as_deref(), "password")?;
            let root_id = kernel
                .unlock(password, ins.root_id.as_deref())
                .map_err(|e| e.to_string())?;
            Ok(serde_json::json!({
                "ok": true,
                "action": "unlock",
                "rootId": root_id,
            })
            .to_string())
        }
        "recover_mnemonic" => {
            let password = required(ins.password.as_deref(), "password")?;
            let mnemonic = required(ins.mnemonic.as_deref(), "mnemonic")?;
            let nickname = ins.nickname.as_deref().unwrap_or("dev-test");
            let root_id = kernel
                .recover_mnemonic(mnemonic, password, nickname, None)
                .map_err(|e| e.to_string())?;
            Ok(serde_json::json!({
                "ok": true,
                "action": "recover_mnemonic",
                "rootId": root_id,
            })
            .to_string())
        }
        "pair_peer" => execute_pair_peer(kernel, ins),
        "relay_status" => {
            // 调试：导出本机 relay 状态（AutoNAT/角色/预约/共享池，U1 facade）。
            match kernel.relay_status() {
                Ok(status) => Ok(serde_json::json!({
                    "ok": true,
                    "action": "relay_status",
                    "status": status,
                })
                .to_string()),
                Err(e) => Err(e.to_string()),
            }
        }
        "dump" => {
            // 调试：按前缀扫 sled 键（值截断 160 字符），用于真机排障
            // （如 D′ 链 pwack 记录状态核查）。仅 debug 构建。
            let prefix = required(ins.prefix.as_deref(), "prefix")?;
            let storage = kernel.__test_storage().ok_or("storage unavailable")?;
            let items = storage
                .scan(&spark_core::storage::ScanOptions {
                    prefix: prefix.to_string(),
                    start: None,
                    end: None,
                    reverse: false,
                    limit: Some(200),
                })
                .map_err(|e| e.to_string())?;
            let entries: Vec<serde_json::Value> = items
                .into_iter()
                .map(|(k, v)| {
                    serde_json::json!({
                        "k": k,
                        "v": v.chars().take(160).collect::<String>(),
                    })
                })
                .collect();
            Ok(serde_json::json!({
                "ok": true,
                "action": "dump",
                "n": entries.len(),
                "entries": entries,
            })
            .to_string())
        }
        other => Err(format!("unknown action: {other}")),
    }
}

/// pair_peer：免扫码设备配对（等价 QR 恢复的配对步骤，见
/// `Kernel::dev_pair_peer`）。要求已解锁（先投 unlock 再投本指令，或随
/// recover_mnemonic 后追加一轮重启）。
fn execute_pair_peer(kernel: &mut Kernel, ins: &DevInstruction) -> Result<String, String> {
    let peer_id = required(ins.peer_id.as_deref(), "peerId")?;
    let addresses = ins.addresses.clone().unwrap_or_default();
    kernel
        .dev_pair_peer(peer_id, &addresses)
        .map_err(|e| e.to_string())?;
    Ok(serde_json::json!({
        "ok": true,
        "action": "pair_peer",
        "peerId": peer_id,
    })
    .to_string())
}

fn required<'a>(value: Option<&'a str>, field: &str) -> Result<&'a str, String> {
    value
        .filter(|v| !v.is_empty())
        .ok_or_else(|| format!("missing field: {field}"))
}

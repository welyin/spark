//! 身份与网络命令：init-identity/unlock/root-id/update-profile、
//! start-p2p/stop-p2p/p2p-status/make-node-card/import-node-card。

use serde_json::{Value, json};
use spark_core::kernel::Kernel;

use crate::DEFAULT_PASSWORD;
use crate::dispatch::{Params, to_json};

/// `init-identity`：创建根身份（昵称必填，口令可省用默认值）；
/// 内核语义「登录即在线」——成功后 p2p 自动启动。
pub fn init_identity(kernel: &mut Kernel, params: &Params) -> Result<Value, String> {
    let nickname = params.need_str("nickname")?;
    let password = params.str_or("password", DEFAULT_PASSWORD);
    let result = kernel
        .init_identity(password, nickname, params.opt_str("avatar"))
        .map_err(|e| e.to_string())?;
    Ok(json!({"rootId": result.root_id, "mnemonic": result.mnemonic}))
}

/// `unlock`：解锁已有身份（rootId 可省 = 活动身份）。
pub fn unlock(kernel: &mut Kernel, params: &Params) -> Result<Value, String> {
    let password = params.str_or("password", DEFAULT_PASSWORD);
    let root_id = kernel
        .unlock(password, params.opt_str("rootId"))
        .map_err(|e| e.to_string())?;
    Ok(json!({"rootId": root_id}))
}

/// `recover-mnemonic`：助记词恢复身份（设备配对场景：第二节点同身份）。
pub fn recover_mnemonic(kernel: &mut Kernel, params: &Params) -> Result<Value, String> {
    let mnemonic = params.need_str("mnemonic")?;
    let nickname = params.need_str("nickname")?;
    let password = params.str_or("password", DEFAULT_PASSWORD);
    let root_id = kernel
        .recover_mnemonic(mnemonic, password, nickname, None)
        .map_err(|e| e.to_string())?;
    Ok(json!({"rootId": root_id}))
}

/// `root-id`：当前解锁身份 rootId（未解锁 → null）。
pub fn root_id(kernel: &Kernel) -> Result<Value, String> {
    let root_id = kernel.current_root_id().map_err(|e| e.to_string())?;
    Ok(json!({"rootId": root_id}))
}

/// `update-profile`：nickname/avatar/gender/region/signature 三态——
/// 键缺省不变；avatar "" 清除（Some(None)）、非空设置；其余 "" 清除。
pub fn update_profile(kernel: &mut Kernel, params: &Params) -> Result<Value, String> {
    let avatar = params
        .tri_str("avatar")
        .map(|value| (!value.is_empty()).then_some(value));
    to_json(kernel.update_profile_session(
        params.opt_str("nickname"),
        avatar,
        params.tri_str("gender"),
        params.tri_str("region"),
        params.tri_str("signature"),
    ))
}

/// `start-p2p`：返回本机 peerId。
pub fn start_p2p(kernel: &mut Kernel) -> Result<Value, String> {
    let peer_id = kernel.start_p2p().map_err(|e| e.to_string())?;
    Ok(json!({"started": true, "peerId": peer_id}))
}

/// `stop-p2p`（幂等）。
pub fn stop_p2p(kernel: &mut Kernel) -> Result<Value, String> {
    kernel.stop_p2p().map_err(|e| e.to_string())?;
    Ok(json!({"started": false}))
}

/// `p2p-status`：未启动返回 `{started:false}`；启动返回 peerId/addresses/connectedPeers。
pub fn p2p_status(kernel: &Kernel) -> Result<Value, String> {
    match kernel.p2p_status().map_err(|e| e.to_string())? {
        Some(info) => Ok(json!({
            "started": info.started,
            "peerId": info.peer_id,
            "addresses": info.addresses,
            "connectedPeers": info.connected_peers,
        })),
        None => Ok(json!({"started": false, "peerId": null, "addresses": [], "connectedPeers": []})),
    }
}

/// `make-node-card`：本机节点名片串（orgId 可省）。
pub fn make_node_card(kernel: &mut Kernel, params: &Params) -> Result<Value, String> {
    let card = kernel
        .make_node_card(params.opt_str("orgId"))
        .map_err(|e| e.to_string())?;
    Ok(json!({"card": card}))
}

/// `import-node-card`：验签 → 未验证入池 → 尽力连接。
pub fn import_node_card(kernel: &mut Kernel, params: &Params) -> Result<Value, String> {
    let card = params.need_str("card")?;
    let result = kernel.import_node_card(card).map_err(|e| e.to_string())?;
    Ok(json!({
        "peerId": result.peer_id,
        "hasRecoveryToken": result.has_recovery_token,
        "connectError": result.connect_error,
    }))
}

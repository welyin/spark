//! 命令分发：解析请求行 → 路由到各业务模块 → 打印统一响应行。

use serde_json::{Map, Value, json};
use spark_core::kernel::Kernel;

use crate::{contact, identity, message, org, print_line};

/// 参数提取助手：统一处理缺参错误与三态字段。
pub struct Params<'a> {
    map: &'a Map<String, Value>,
}

impl<'a> Params<'a> {
    pub fn new(map: &'a Map<String, Value>) -> Self {
        Self { map }
    }

    /// 必填字符串。
    pub fn need_str(&self, key: &str) -> Result<&'a str, String> {
        self.map
            .get(key)
            .and_then(Value::as_str)
            .ok_or_else(|| format!("missing param: {key}"))
    }

    /// 可选字符串（缺省/null → None）。
    pub fn opt_str(&self, key: &str) -> Option<&'a str> {
        self.map.get(key).and_then(Value::as_str)
    }

    /// 可选字符串，带默认值。
    pub fn str_or(&self, key: &str, default: &'a str) -> &'a str {
        self.opt_str(key).unwrap_or(default)
    }

    /// 可选布尔。
    pub fn opt_bool(&self, key: &str) -> Option<bool> {
        self.map.get(key).and_then(Value::as_bool)
    }

    /// 可选字符串数组。
    pub fn opt_strings(&self, key: &str) -> Option<Vec<String>> {
        self.map.get(key).and_then(Value::as_array).map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(ToString::to_string)
                .collect()
        })
    }

    /// 可选对象。
    pub fn opt_value(&self, key: &str) -> Option<&'a Value> {
        self.map.get(key).filter(|v| !v.is_null())
    }

    /// 三态字符串：缺省 → None（不变）；"" → Some("")（清除，由内核按字段口径解释）。
    pub fn tri_str(&self, key: &str) -> Option<&'a str> {
        self.opt_str(key)
    }
}

/// 处理一行请求；返回 true 表示收到 shutdown，主循环应退出。
pub fn handle_line(text: &str, kernel: &mut Kernel) -> bool {
    let parsed: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(e) => {
            print_line(&json!({"id": Value::Null, "ok": false, "error": format!("invalid json: {e}")}));
            return false;
        }
    };
    let id = parsed.get("id").cloned().unwrap_or(Value::Null);
    let cmd = parsed.get("cmd").and_then(Value::as_str).unwrap_or("");
    let empty = Map::new();
    let params = Params::new(parsed.as_object().unwrap_or(&empty));

    let outcome: Result<Value, String> = match cmd {
        // 身份
        "init-identity" => identity::init_identity(kernel, &params),
        "unlock" => identity::unlock(kernel, &params),
        "recover-mnemonic" => identity::recover_mnemonic(kernel, &params),
        "root-id" => identity::root_id(kernel),
        "update-profile" => identity::update_profile(kernel, &params),
        // 网络
        "start-p2p" => identity::start_p2p(kernel),
        "stop-p2p" => identity::stop_p2p(kernel),
        "p2p-status" => identity::p2p_status(kernel),
        "make-node-card" => identity::make_node_card(kernel, &params),
        "import-node-card" => identity::import_node_card(kernel, &params),
        // 联系人
        "contact-overview" => contact::overview(kernel, &params),
        "send-request" => contact::send_request(kernel, &params),
        "accept-request" => contact::accept_request(kernel, &params),
        "reply-request" => contact::reply_request(kernel, &params),
        "ask-request" => contact::ask_request(kernel, &params),
        "remove-friend" => contact::remove_friend(kernel, &params),
        "block-root" => contact::block_root(kernel, &params),
        // 消息
        "conversations" => message::conversations(kernel, &params),
        "messages" => message::messages(kernel, &params),
        "send-text" => message::send_text(kernel, &params),
        "mark-read" => message::mark_read(kernel, &params),
        "recall" => message::recall(kernel, &params),
        "resend" => message::resend(kernel, &params),
        // 组织
        "org-create" => org::create(kernel, &params),
        "org-add-member" => org::add_member(kernel, &params),
        "org-send-invite" => org::send_invite(kernel, &params),
        "org-respond-invite" => org::respond_invite(kernel, &params),
        "org-invite-records" => org::invite_records(kernel, &params),
        "org-list" => org::list(kernel),
        "org-view" => org::view(kernel, &params),
        "org-update-info" => org::update_info(kernel, &params),
        "org-update-my-identity" => org::update_my_identity(kernel, &params),
        // 杂项
        "shutdown" => {
            print_line(&json!({"id": id, "ok": true, "data": {"stopping": true}}));
            return true;
        }
        other => Err(format!("unknown cmd: {other}")),
    };

    match outcome {
        Ok(data) => print_line(&json!({"id": id, "ok": true, "data": data})),
        Err(error) => print_line(&json!({"id": id, "ok": false, "error": error})),
    }
    false
}

/// kernel Result<T: Serialize> → JSON 值。
pub fn to_json<T: serde::Serialize>(result: Result<T, spark_core::kernel::KernelError>) -> Result<Value, String> {
    result
        .map_err(|e| e.to_string())
        .and_then(|v| serde_json::to_value(v).map_err(|e| e.to_string()))
}

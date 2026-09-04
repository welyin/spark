//! Android 通知/前台服务/网络回调的 JNI 桥（阶段四C Android 存活链路，
//! wiki architecture/p2p/android-notifications §5/§6）。
//!
//! 镜像 `android_activity.rs`/`biometric_android.rs` 先例：MainActivity.onCreate
//! 经 `nativeSetNotificationHelper` 把 `NotificationHelper` 实例注册进来
//! （GlobalRef 防回收 + JavaVM 供任意命令线程附着）；`NetworkHelper` 的
//! 网络回调经 `nativeOnNetworkChanged` 直调内核 `p2p_network_changed`
//! （不经前端 WebView 转发——后台/锁屏时浏览器事件不可靠）。
//!
//! 内核句柄槽：Tauri 管理的 KernelState 无法从 Kotlin 侧到达，lib.rs setup
//! 在 `app.manage` 后经 `register_kernel` 把 Arc 克隆注册进静态格。
//!
//! 全部入口尽力而为：未注册/调用失败静默降级（通知是装饰性反馈，不冒泡）。

use jni::objects::{GlobalRef, JClass, JObject, JString, JValue};
use jni::{JavaVM, JNIEnv};
use std::sync::Mutex;

use crate::KernelState;

static HELPER: Mutex<Option<(JavaVM, GlobalRef)>> = Mutex::new(None);
static KERNEL: Mutex<Option<KernelState>> = Mutex::new(None);

/// MainActivity.onCreate 调用：注册 NotificationHelper 实例。
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_spark_desktop_MainActivity_nativeSetNotificationHelper(
    env: JNIEnv<'_>,
    _class: JClass,
    helper: JObject,
) {
    match (env.get_java_vm(), env.new_global_ref(helper)) {
        (Ok(vm), Ok(global)) => {
            *HELPER.lock().unwrap_or_else(|e| e.into_inner()) = Some((vm, global));
        }
        _ => eprintln!("[android-native] nativeSetNotificationHelper failed"),
    }
}

/// lib.rs setup 调用（`app.manage` 之后）：注册内核句柄供网络回调直调。
pub fn register_kernel(state: &KernelState) {
    *KERNEL.lock().unwrap_or_else(|e| e.into_inner()) = Some(state.clone());
}

/// NetworkHelper 网络回调（500ms 去抖后）：直调内核网络变更处理（内核防抖
/// + 地址快照比对天然幂等，与桌面浏览器事件双源上报无害）。P2P 未启动时
/// 内核静默成功。
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_spark_desktop_NetworkHelper_nativeOnNetworkChanged(
    _env: JNIEnv<'_>,
    _class: JClass,
) {
    let kernel = KERNEL.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if let Some(state) = kernel {
        let guard = state.lock().unwrap_or_else(|e| e.into_inner());
        if let Err(e) = guard.p2p_network_changed() {
            eprintln!("[android-native] p2p_network_changed failed: {e}");
        }
    }
}

/// 单锁作用域内完成「建参数字符串 + 调用」两步（Mutex 不可重入——拆两把锁
/// 会自死锁）。未注册 helper 返回 `unsupported`（调用方静默降级）。
fn with_helper<R>(
    f: impl FnOnce(&mut JNIEnv, &GlobalRef) -> Result<R, String>,
) -> Result<R, String> {
    let guard = HELPER.lock().unwrap_or_else(|e| e.into_inner());
    let Some((vm, helper)) = guard.as_ref() else {
        return Err("unsupported".to_string());
    };
    let mut env = vm
        .attach_current_thread_as_daemon()
        .map_err(|_| "unsupported".to_string())?;
    f(&mut env, helper)
}

/// 调用 helper 的 Bool 返回方法；Java 侧抛异常时清栈后报错（挂起异常不清除
/// 会污染当前线程后续全部 JNI 调用——biometric_android.rs 同口径）。
fn call_method_bool(
    env: &mut JNIEnv,
    helper: &GlobalRef,
    method: &str,
    sig: &str,
    args: &[JValue],
) -> Result<bool, String> {
    let value = match env.call_method(helper, method, sig, args) {
        Ok(value) => value,
        Err(e) => {
            if env.exception_check().unwrap_or(false) {
                let _ = env.exception_describe();
                let _ = env.exception_clear();
            }
            return Err(format!("jni call {method}: {e}"));
        }
    };
    value.z().map_err(|e| format!("jni: {e}"))
}

/// env 内建 JString（生命周期绑定 env；错误上抛为 String）。
fn new_strings<'a>(env: &mut JNIEnv<'a>, values: &[&str]) -> Result<Vec<JString<'a>>, String> {
    values
        .iter()
        .map(|v| env.new_string(v).map_err(|e| format!("jni new_string: {e}")))
        .collect()
}

/// `system_notify_chat`（Android 实现）：聊天消息系统通知（messages 渠道，
/// 稳定 id = convId hash 覆盖更新）。
pub fn notify_chat(space_key: &str, conv_id: &str, title: &str, body: &str, unread: i64) {
    let r = with_helper(|env, helper| {
        let js = new_strings(env, &[space_key, conv_id, title, body])?;
        call_method_bool(
            env,
            helper,
            "notifyChat",
            "(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;I)Z",
            &[
                JValue::Object(&js[0]),
                JValue::Object(&js[1]),
                JValue::Object(&js[2]),
                JValue::Object(&js[3]),
                JValue::Int(unread as i32),
            ],
        )
    });
    if let Err(e) = r {
        eprintln!("[android-native] notifyChat: {e}");
    }
}

/// `system_notify_generic`（Android 实现）：系统事件泛化提醒（system 渠道
/// 静默；只泛化提醒不含内容——内容在消息页 sys:notice 系统会话）。
pub fn notify_generic(title: &str, body: &str) {
    let r = with_helper(|env, helper| {
        let js = new_strings(env, &[title, body])?;
        call_method_bool(
            env,
            helper,
            "notifyGeneric",
            "(Ljava/lang/String;Ljava/lang/String;)Z",
            &[JValue::Object(&js[0]), JValue::Object(&js[1])],
        )
    });
    if let Err(e) = r {
        eprintln!("[android-native] notifyGeneric: {e}");
    }
}

/// P2P 启动成功（事件泵存活）→ 挂前台服务常驻通知（keepalive 渠道低打扰）。
pub fn keepalive_start() {
    if let Err(e) = with_helper(|env, helper| {
        call_method_bool(env, helper, "startKeepAlive", "()Z", &[])
    }) {
        eprintln!("[android-native] startKeepAlive: {e}");
    }
}

/// 登出/停止 P2P/应用退出 → 停前台服务（幂等）。
pub fn keepalive_stop() {
    if let Err(e) = with_helper(|env, helper| {
        call_method_bool(env, helper, "stopKeepAlive", "()Z", &[])
    }) {
        eprintln!("[android-native] stopKeepAlive: {e}");
    }
}

//! Android 生物识别保险柜的 JNI 桥（M4 生物识别解锁，方案 §3.3）。
//!
//! 镜像 `android_activity.rs` 先例：MainActivity.onCreate 经
//! `nativeSetBiometricHelper` 把 `BiometricKeystoreHelper` 实例注册进来
//! （GlobalRef 防回收 + JavaVM 供任意命令线程附着）。
//!
//! 线程模型：Tauri 命令跑在命令线程（非 Android 主线程），四个导出函数均为
//! 同步 JNI 调用——Kotlin 侧 `runOnUiThread` 弹 BiometricPrompt 并
//! `CountDownLatch.await`（2min 超时按 user-cancelled），不阻塞主线程、无死锁。
//!
//! 返回协议：Kotlin 侧方法返回 JSON 字符串（见 BiometricKeystoreHelper 头注），
//! 本模块解析后向命令层给出结构化结果；错误码七值原样透传（前端按码映射文案）。
//! 口令明文只在本模块与命令层内存中流转，不落存储、不进日志。

use jni::objects::{GlobalRef, JClass, JObject, JString, JValue};
use jni::{JavaVM, JNIEnv};
use std::sync::Mutex;

static HELPER: Mutex<Option<(JavaVM, GlobalRef)>> = Mutex::new(None);

/// `biometric_status` 结果（serde camelCase 与前端 DTO 对齐）。
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BiometricStatus {
    pub available: bool,
    pub enrolled: bool,
    pub has_secret: bool,
}

/// `biometric_unlock` 结果：芯片载荷的 per-identity 标签与口令明文。
pub struct BiometricUnlock {
    pub root_id: String,
    pub password: String,
}

/// MainActivity.onCreate 调用：注册 BiometricKeystoreHelper 实例。
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_spark_desktop_MainActivity_nativeSetBiometricHelper(
    env: JNIEnv<'_>,
    _class: JClass,
    helper: JObject,
) {
    match (env.get_java_vm(), env.new_global_ref(helper)) {
        (Ok(vm), Ok(global)) => {
            *HELPER.lock().unwrap_or_else(|e| e.into_inner()) = Some((vm, global));
        }
        _ => eprintln!("[biometric] nativeSetBiometricHelper failed"),
    }
}

/// 同步调用 helper 的无参方法（返回 JSON 字符串）。
fn call_helper0(method: &str) -> Result<String, String> {
    call_helper(method, "()Ljava/lang/String;", &[])
}

/// 同步调用 helper 的双字符串参方法（返回 JSON 字符串）。
fn call_helper2(method: &str, a: &str, b: &str) -> Result<String, String> {
    let guard = HELPER.lock().unwrap_or_else(|e| e.into_inner());
    let Some((vm, helper)) = guard.as_ref() else {
        return Err("unsupported".to_string());
    };
    let mut env = vm
        .attach_current_thread_as_daemon()
        .map_err(|_| "unsupported".to_string())?;
    let ja = env.new_string(a).map_err(|e| format!("jni: {e}"))?;
    let jb = env.new_string(b).map_err(|e| format!("jni: {e}"))?;
    call_method_string(
        &mut env,
        helper,
        method,
        "(Ljava/lang/String;Ljava/lang/String;)Ljava/lang/String;",
        &[JValue::Object(&ja), JValue::Object(&jb)],
    )
}

fn call_helper(method: &str, sig: &str, args: &[JValue]) -> Result<String, String> {
    let guard = HELPER.lock().unwrap_or_else(|e| e.into_inner());
    let Some((vm, helper)) = guard.as_ref() else {
        return Err("unsupported".to_string());
    };
    let mut env = vm
        .attach_current_thread_as_daemon()
        .map_err(|_| "unsupported".to_string())?;
    call_method_string(&mut env, helper, method, sig, args)
}

/// 发起调用并把返回的 JString 收为 Rust String；Java 侧抛异常时清栈后报错
/// （挂起异常不清除会污染当前线程后续全部 JNI 调用）。
fn call_method_string(
    env: &mut JNIEnv,
    helper: &GlobalRef,
    method: &str,
    sig: &str,
    args: &[JValue],
) -> Result<String, String> {
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
    let obj = value.l().map_err(|e| format!("jni: {e}"))?;
    let jstr = JString::from(obj);
    env.get_string(&jstr)
        .map(|s| s.into())
        .map_err(|e| format!("jni: {e}"))
}

fn parse_ok(json: &str) -> Result<serde_json::Value, String> {
    let value: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("helper json: {e}"))?;
    if value.get("ok").and_then(|v| v.as_bool()) == Some(true) {
        return Ok(value);
    }
    let code = value
        .get("error")
        .and_then(|v| v.as_str())
        .unwrap_or("auth-failed");
    Err(code.to_string())
}

/// `biometric_status`：可用性探测（Kotlin 侧尽力返回，不弹窗）。
pub fn status() -> Result<BiometricStatus, String> {
    let json = call_helper0("status")?;
    let value: serde_json::Value =
        serde_json::from_str(&json).map_err(|e| format!("helper json: {e}"))?;
    Ok(BiometricStatus {
        available: value
            .get("available")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        enrolled: value
            .get("enrolled")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        has_secret: value
            .get("hasSecret")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
    })
}

/// `biometric_store_password`：认证后加密落芯片。错误为七值错误码。
pub fn store_password(root_id: &str, password: &str) -> Result<(), String> {
    let json = call_helper2("storePassword", root_id, password)?;
    parse_ok(&json).map(|_| ())
}

/// `biometric_unlock`：认证后解密取回 {rootId, password}。错误为七值错误码。
pub fn unlock() -> Result<BiometricUnlock, String> {
    let json = call_helper0("unlock")?;
    let value = parse_ok(&json)?;
    Ok(BiometricUnlock {
        root_id: value
            .get("rootId")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        password: value
            .get("password")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
    })
}

/// `biometric_delete`：删密钥 + 清 blob（幂等）。
pub fn delete() -> Result<(), String> {
    let json = call_helper0("delete")?;
    parse_ok(&json).map(|_| ())
}

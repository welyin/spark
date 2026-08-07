//! Android 主 Activity 的 JNI 注册与「退到后台」能力（Android 前端改造）。
//!
//! 用途：系统返回键在一级页时，前端经 `system_exit_app` 命令要求退出——但我们是
//! P2P 应用，进程死了就掉线，正确语义是 `moveTaskToBack`（等同按 Home 键）。
//! Rust 侧无法直接拿到 Activity，故 MainActivity.onCreate 时经
//! `nativeSetActivity` 把实例注册进来（GlobalRef + JavaVM）。
//!
//! 注意：旧的 app.exit(0) 路径在 Android 上会在退出钩子里触发 WebView/Adreno GL
//! 析构崩溃（FORTIFY pthread_mutex_lock on destroyed mutex，已实测），不能用。

use jni::objects::{GlobalRef, JClass, JObject, JValue};
use jni::{JavaVM, JNIEnv};
use std::sync::Mutex;

static ACTIVITY: Mutex<Option<(JavaVM, GlobalRef)>> = Mutex::new(None);

/// MainActivity.onCreate 调用：注册主 Activity 实例（GlobalRef 防回收）。
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_spark_desktop_MainActivity_nativeSetActivity(
    mut env: JNIEnv,
    _class: JClass,
    activity: JObject,
) {
    match (env.get_java_vm(), env.new_global_ref(activity)) {
        (Ok(vm), Ok(global)) => {
            *ACTIVITY.lock().unwrap_or_else(|e| e.into_inner()) = Some((vm, global));
        }
        _ => eprintln!("[android-activity] nativeSetActivity failed"),
    }
}

/// 把主任务退到后台（等同按 Home 键）：进程保持存活，P2P 不断线。
/// 返回是否成功调用；未注册/调用失败返回 false（调用方可决定兜底）。
pub fn move_task_to_back() -> bool {
    let guard = ACTIVITY.lock().unwrap_or_else(|e| e.into_inner());
    let Some((vm, activity)) = guard.as_ref() else {
        return false;
    };
    let Ok(mut env) = vm.attach_current_thread_as_daemon() else {
        return false;
    };
    env.call_method(
        activity,
        "moveTaskToBack",
        "(Z)Z",
        &[JValue::Bool(1)],
    )
    .is_ok()
}

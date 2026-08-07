package com.spark.desktop

import android.content.Context
import android.net.wifi.WifiManager
import android.os.Bundle
import android.util.Log
import androidx.activity.enableEdgeToEdge

class MainActivity : TauriActivity() {
  // 禁用 WryActivity 自带的返回键处理（canGoBack→goBack 的 WebView 历史回退）：
  // 返回键统一交由 AppPlugin 回调 → JS onBackButtonPress 事件 → 前端导航栈处理
  // （本应用为 SPA 无 WebView 历史，且双回调并存时后注册的 WryActivity 回调会优先
  // 消费返回键，绕过前端导航栈）。JS 未加载/未注册监听时 AppPlugin 仍有原生兜底
  // （canGoBack→goBack，否则 finish），行为不受影响。
  override val handleBackNavigation: Boolean = false

  private var multicastLock: WifiManager.MulticastLock? = null

  companion object {
    // 向 Rust 注册主 Activity 实例（系统返回键在一级页时 moveTaskToBack 退后台保活，
    // 见 src-tauri/src/android_activity.rs；P2P 应用进程死了就掉线）
    @JvmStatic
    private external fun nativeSetActivity(activity: MainActivity)
  }

  override fun onCreate(savedInstanceState: Bundle?) {
    enableEdgeToEdge()
    super.onCreate(savedInstanceState)
    nativeSetActivity(this)
    // Android 默认不向应用投递组播包：mdns 局域网发现（P2P 节点互见/自设备配对）
    // 必须持 MulticastLock 才能正常收发组播，且 manifest 需 CHANGE_WIFI_MULTICAST_STATE。
    // 锁随 Activity 存活（onDestroy 释放；进程死亡系统亦会回收），桌面端无此概念。
    try {
      val wifi = applicationContext.getSystemService(Context.WIFI_SERVICE) as WifiManager
      multicastLock = wifi.createMulticastLock("spark-mdns").apply {
        setReferenceCounted(true)
        acquire()
      }
      Log.i("MainActivity", "MulticastLock acquired (mdns discovery enabled)")
    } catch (t: Throwable) {
      Log.w("MainActivity", "acquire MulticastLock failed", t)
    }
  }

  override fun onDestroy() {
    multicastLock?.let { if (it.isHeld) it.release() }
    multicastLock = null
    super.onDestroy()
  }
}

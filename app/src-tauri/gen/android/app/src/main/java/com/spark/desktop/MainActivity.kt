package com.spark.desktop

import android.content.Context
import android.net.wifi.WifiManager
import android.os.Bundle
import android.util.Log
import android.webkit.WebView
import androidx.activity.enableEdgeToEdge
import androidx.core.view.ViewCompat
import androidx.core.view.WindowInsetsCompat
import androidx.webkit.WebViewCompat

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

  /** Android 15+ 强制 edge-to-edge 后 WebView 铺满全屏，但 wry 不把系统栏 insets
      转发为 CSS env(safe-area-inset-*)，前端拿不到导航栏/状态栏高度（真机实测 env() 为空）。
      这里把真实 insets（转 CSS px）注入为 --spark-safe-top/--spark-safe-bottom 变量，
      app-shell.css 等以 var(--spark-safe-*, env()) 兜底使用（Android 前端修复）。
      注意：webview 创建时页面尚未加载，直接注入会随页面加载被清掉，因此：
      1) 延时重注入（页面加载完成后生效）；2) addDocumentStartJavaScript 持久注入
      （每次页面重新加载都在 document start 重放，vite 全量刷新/应用重载后不丢）。
      不要给 WebView 挂 OnApplyWindowInsetsListener：会拦截窗口 insets 派发
      （含键盘 IME），破坏 adjustResize + 100dvh 的键盘顶起登录页机制。 */
  override fun onWebViewCreate(webView: WebView) {
    super.onWebViewCreate(webView)
    val density = resources.displayMetrics.density
    var persistentRegistered = false
    val buildScript = { top: Int, bottom: Int ->
      "try{var r=document.documentElement;" +
        "r.style.setProperty('--spark-safe-top','${top}px');" +
        "r.style.setProperty('--spark-safe-bottom','${bottom}px');" +
        "}catch(e){}"
    }
    val inject = Runnable {
      try {
        val bars = ViewCompat.getRootWindowInsets(webView)
          ?.getInsets(WindowInsetsCompat.Type.systemBars())
        val top = ((bars?.top ?: 0) / density).toInt()
        val bottom = ((bars?.bottom ?: 0) / density).toInt()
        Log.i("MainActivity", "inject safe-area top=$top bottom=$bottom")
        webView.evaluateJavascript(buildScript(top, bottom), null)
        if (!persistentRegistered) {
          WebViewCompat.addDocumentStartJavaScript(
            webView, buildScript(top, bottom), emptySet()
          )
          persistentRegistered = true
        }
      } catch (t: Throwable) {
        Log.w("MainActivity", "inject safe-area failed", t)
      }
    }
    webView.postDelayed(inject, 1000)
    webView.postDelayed(inject, 2500)
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

package com.spark.desktop

import android.app.Application
import android.content.Context
import android.net.ConnectivityManager
import android.net.Network
import android.net.NetworkCapabilities
import android.os.Handler
import android.os.Looper
import android.util.Log

/**
 * ConnectivityManager 原生网络回调（阶段四C，android-notifications §6）：
 * 替代浏览器 online/offline 事件的主通路——浏览器事件在 Android WebView
 * 不反映 Wi-Fi↔蜂窝的默认网络切换，且后台/锁屏投递不可靠；原生回调随
 * 进程存活（Application 级 registerDefaultNetworkCallback），覆盖后台。
 *
 * 500ms 去抖（切换瞬间 onAvailable/onLost/onCapabilitiesChanged 三连发是
 * 常态）后经 JNI 直调内核 `p2p_network_changed`（不经前端 WebView 转发）。
 * 内核侧防抖 + 地址快照比对天然幂等，与桌面浏览器事件双源上报无害。
 */
object NetworkHelper {

  private const val TAG = "NetworkHelper"
  private const val DEBOUNCE_MS = 500L

  private val handler = Handler(Looper.getMainLooper())
  private var pending: Runnable? = null
  private var registered = false

  /** 向 Rust 注册入口见 src-tauri/src/android_native.rs（直调内核）。 */
  @JvmStatic
  private external fun nativeOnNetworkChanged()

  /** Application 级注册（MainActivity.onCreate 调用一次，幂等）。 */
  @JvmStatic
  fun register(app: Application) {
    if (registered) return
    registered = true
    try {
      val cm = app.getSystemService(Context.CONNECTIVITY_SERVICE) as ConnectivityManager
      cm.registerDefaultNetworkCallback(object : ConnectivityManager.NetworkCallback() {
        override fun onAvailable(network: Network) = schedule()
        override fun onLost(network: Network) = schedule()
        override fun onCapabilitiesChanged(network: Network, caps: NetworkCapabilities) = schedule()
      })
      Log.i(TAG, "default network callback registered")
    } catch (t: Throwable) {
      Log.w(TAG, "register network callback failed", t)
    }
  }

  /** 去抖：窗口内新事件替换未触发的上一次（合并连发为一次内核通知）。
      回调可达于 ConnectivityManager 线程——`pending` 的读改写全部转到
      主线程 Handler 队列内串行执行（消除回调线程 × 主线程的可见性/
      竞态窗口，重复调度会多弹一次内核通知，内核幂等但不为竞态留窗）。 */
  private fun schedule() {
    handler.post {
      pending?.let { handler.removeCallbacks(it) }
      val task = Runnable {
        pending = null
        try {
          nativeOnNetworkChanged()
        } catch (t: Throwable) {
          Log.w(TAG, "nativeOnNetworkChanged failed", t)
        }
      }
      pending = task
      handler.postDelayed(task, DEBOUNCE_MS)
    }
  }
}

package com.spark.desktop

import android.app.Service
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.IBinder
import android.util.Log

/**
 * 前台服务保活（阶段四C，android-notifications §5）：进程标为「正在为用户
 * 工作」，退后台后显著降低被杀概率。职责仅限本机自身收发（leaf mode 正交：
 * 不为他人提供数据/路由服务的口径不变）。
 *
 * 启停由 Rust 内核事件驱动（start_p2p 成功才挂 / 登出退出即停，经
 * NotificationHelper.startKeepAlive/stopKeepAlive）；通知本体由
 * NotificationHelper.buildKeepAliveNotification 构造（keepalive 低打扰渠道）。
 */
class KeepAliveService : Service() {

  companion object {
    private const val TAG = "KeepAliveService"
  }

  override fun onCreate() {
    super.onCreate()
    val notification = NotificationHelper.buildKeepAliveNotificationFor(this)
    if (Build.VERSION.SDK_INT >= 34) {
      startForeground(
        NotificationHelper.KEEPALIVE_NOTIFICATION_ID,
        notification,
        ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC,
      )
    } else {
      startForeground(NotificationHelper.KEEPALIVE_NOTIFICATION_ID, notification)
    }
    Log.i(TAG, "foreground service started (dataSync)")
  }

  override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
    // 被杀后由系统择机重启（START_STICKY）——重启时不带 intent，onCreate
    // 已挂前台通知，服务空转等内核事件重新接管
    return START_STICKY
  }

  override fun onBind(intent: Intent?): IBinder? = null

  override fun onDestroy() {
    Log.i(TAG, "foreground service stopped")
    super.onDestroy()
  }
}

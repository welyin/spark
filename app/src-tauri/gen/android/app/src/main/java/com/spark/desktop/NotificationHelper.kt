package com.spark.desktop

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat

/**
 * 通知构造与前台服务启停（阶段四C Android 存活链路，wiki
 * architecture/p2p/android-notifications §2/§5）。
 *
 * 三个渠道：messages（聊天，default importance 可响铃）/ system（系统事件，
 * low 静默）/ keepalive（前台服务常驻，min 无声音震动，可一键进设置关闭）。
 *
 * 由 Rust 命令层经 JNI 调用（src-tauri/src/android_native.rs）：
 * MainActivity.onCreate 经 `nativeSetNotificationHelper` 注册实例；
 * 全部方法从任意线程可达（NotificationManagerCompat 线程安全）。
 *
 * 返回协议：notify* / startKeepAlive / stopKeepAlive 返回 Bool
 * （true=已发出；false=权限缺失或内部失败，Rust 侧按静默降级处理）。
 */
class NotificationHelper(private val activity: MainActivity) {

  companion object {
    const val CHANNEL_MESSAGES = "messages"
    const val CHANNEL_SYSTEM = "system"
    const val CHANNEL_KEEPALIVE = "keepalive"
    const val KEEPALIVE_NOTIFICATION_ID = 1

    /** keepalive 常驻通知本体（KeepAliveService.onCreate 调用；服务侧无
        helper 实例，静态方法 + 显式 Context）。 */
    fun buildKeepAliveNotificationFor(context: Context): Notification {
      val intent = Intent(context, MainActivity::class.java)
      val pending = PendingIntent.getActivity(
        context, 0, intent,
        PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
      )
      return NotificationCompat.Builder(context, CHANNEL_KEEPALIVE)
        .setSmallIcon(android.R.drawable.stat_notify_sync)
        .setContentTitle("Spark 正在保持连接")
        .setContentText("关闭后可能收不到实时消息（系统设置可管理）")
        .setContentIntent(pending)
        .setOngoing(true)
        .build()
    }
  }

  private val appContext: Context get() = activity.applicationContext

  /** 建三个通知渠道（幂等；渠道属性创建后不可改，改文案需升渠道 id）。 */
  fun createChannels() {
    val manager = appContext.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
    manager.createNotificationChannel(
      NotificationChannel(CHANNEL_MESSAGES, "聊天消息", NotificationManager.IMPORTANCE_DEFAULT)
    )
    manager.createNotificationChannel(
      NotificationChannel(CHANNEL_SYSTEM, "系统通知", NotificationManager.IMPORTANCE_LOW)
    )
    manager.createNotificationChannel(
      NotificationChannel(CHANNEL_KEEPALIVE, "后台连接", NotificationManager.IMPORTANCE_MIN)
    )
  }

  /** Android 13+ 的通知运行时权限是否已授予。 */
  fun hasNotificationPermission(): Boolean {
    if (Build.VERSION.SDK_INT < 33) return true
    return activity.checkSelfPermission(android.Manifest.permission.POST_NOTIFICATIONS) ==
      PackageManager.PERMISSION_GRANTED
  }

  /** 聊天消息通知：稳定 id = convId 的 hash（同会话覆盖更新不堆叠）；
      点击经 PendingIntent → MainActivity（extra 携 spaceKey/convId）。 */
  fun notifyChat(
    spaceKey: String,
    convId: String,
    title: String,
    body: String,
    unread: Int,
  ): Boolean {
    if (!hasNotificationPermission()) return false
    val intent = Intent(appContext, MainActivity::class.java).apply {
      putExtra("spaceKey", spaceKey)
      putExtra("convId", convId)
    }
    val pending = PendingIntent.getActivity(
      appContext, 0, intent,
      PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
    )
    val notification = NotificationCompat.Builder(appContext, CHANNEL_MESSAGES)
      .setSmallIcon(android.R.drawable.ic_dialog_email)
      .setContentTitle(title)
      .setContentText(body)
      .setGroup("messages")
      .setNumber(unread)
      .setContentIntent(pending)
      .setAutoCancel(true)
      .build()
    return try {
      NotificationManagerCompat.from(appContext).notify(convId.hashCode(), notification)
      true
    } catch (t: SecurityException) {
      false // 权限被用户在系统设置里收回
    }
  }

  /** 系统事件泛化提醒（§2.2：不含具体内容——通知是铃铛，留痕在 sys:notice
      系统会话）；system 渠道静默。 */
  fun notifyGeneric(title: String, body: String): Boolean {
    if (!hasNotificationPermission()) return false
    val notification = NotificationCompat.Builder(appContext, CHANNEL_SYSTEM)
      .setSmallIcon(android.R.drawable.ic_dialog_info)
      .setContentTitle(title)
      .setContentText(body)
      .setAutoCancel(true)
      .build()
    return try {
      NotificationManagerCompat.from(appContext).notify(title.hashCode(), notification)
      true
    } catch (t: SecurityException) {
      false
    }
  }

  /** 挂前台服务（P2P 启动成功后由 Rust 调）：常驻 keepalive 渠道通知。 */
  fun startKeepAlive(): Boolean {
    return try {
      val intent = Intent(appContext, KeepAliveService::class.java)
      appContext.startForegroundService(intent)
      true
    } catch (t: Throwable) {
      false
    }
  }

  /** 停前台服务（登出/停 P2P/退出时由 Rust 调）。 */
  fun stopKeepAlive(): Boolean {
    return try {
      appContext.stopService(Intent(appContext, KeepAliveService::class.java))
      true
    } catch (t: Throwable) {
      false
    }
  }
}

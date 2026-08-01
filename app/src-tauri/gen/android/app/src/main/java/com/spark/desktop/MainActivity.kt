package com.spark.desktop

import android.content.Context
import android.net.wifi.WifiManager
import android.os.Bundle
import android.util.Log
import androidx.activity.enableEdgeToEdge

class MainActivity : TauriActivity() {
  private var multicastLock: WifiManager.MulticastLock? = null

  override fun onCreate(savedInstanceState: Bundle?) {
    enableEdgeToEdge()
    super.onCreate(savedInstanceState)
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

package com.spark.desktop

import android.content.Context
import android.os.Build
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyPermanentlyInvalidatedException
import android.security.keystore.KeyProperties
import android.util.Base64
import android.util.Log
import androidx.biometric.BiometricManager
import androidx.biometric.BiometricPrompt
import androidx.core.content.ContextCompat
import java.security.KeyStore
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicReference
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec
import org.json.JSONObject

/**
 * 生物识别口令保险柜（M4，见 wiki/architecture/identity/m4-m5-mobile-plan.md §3.2）。
 *
 * 安全语义：
 * - Keystore alias spark_bio_gate_v1，AES/GCM/NoPadding，每次使用必须当场生物识别
 *   认证（API 30+ setUserAuthenticationParameters(0, AUTH_BIOMETRIC_STRONG)，无时效窗）；
 * - setInvalidatedByBiometricEnrollment(true)：用户新增/删除指纹或人脸即密钥永久
 *   失效（KeyPermanentlyInvalidatedException → key-invalidated），强制回退密码；
 * - 口令明文只经生物识别认证后的 Cipher 加密落 SharedPreferences(private)，
 *   blob = base64(iv ‖ ciphertext)，载荷 JSON {rootId, password}（per-identity 标签）。
 *
 * 线程模型：四个方法均由 Rust 命令线程经 JNI 同步调用（biometric_android.rs），
 * BiometricPrompt 必须弹在 UI 线程——runOnUiThread 弹窗 + CountDownLatch.await
 * （2min 超时按 user-cancelled），不阻塞主线程、无死锁。
 *
 * 返回协议（供 Rust 解析）：store/unlock/delete 返回 JSON 字符串，
 * 成功 {"ok":true,...}，失败 {"ok":false,"error":"<七值错误码之一>"}；
 * status 恒返回 {"available":bool,"enrolled":bool,"hasSecret":bool}（尽力探测，不报错）。
 * 错误码：unsupported / not-enrolled / no-secret / user-cancelled /
 *         auth-failed / lockout / key-invalidated。
 */
class BiometricKeystoreHelper(private val activity: MainActivity) {

  companion object {
    private const val TAG = "BiometricKeystore"
    private const val KEY_ALIAS = "spark_bio_gate_v1"
    private const val PREFS_NAME = "spark_biometric"
    private const val PREF_BLOB = "gate_blob_v1"
    private const val GCM_TAG_BITS = 128
    private const val PROMPT_TIMEOUT_SECONDS = 120L
  }

  private val prefs
    get() = activity.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)

  // ------------------------------------------------------------------
  // status：可用性探测（不弹窗、不报错）
  // ------------------------------------------------------------------

  fun status(): String {
    val canAuth = BiometricManager.from(activity)
      .canAuthenticate(BiometricManager.Authenticators.BIOMETRIC_STRONG)
    val available = canAuth == BiometricManager.BIOMETRIC_SUCCESS ||
      canAuth == BiometricManager.BIOMETRIC_ERROR_NONE_ENROLLED
    val enrolled = canAuth == BiometricManager.BIOMETRIC_SUCCESS
    val hasSecret = prefs.contains(PREF_BLOB)
    return JSONObject()
      .put("available", available)
      .put("enrolled", enrolled)
      .put("hasSecret", hasSecret)
      .toString()
  }

  // ------------------------------------------------------------------
  // store：生物识别认证后把 {rootId, password} 加密落芯片
  // ------------------------------------------------------------------

  fun storePassword(rootId: String, password: String): String {
    if (!enrolled()) return err("not-enrolled")
    val cipher = try {
      getCipher(Cipher.ENCRYPT_MODE, null)
    } catch (e: KeyPermanentlyInvalidatedException) {
      // store 是重新登记语义：旧 blob 随失效密钥已无价值，重建密钥再试一次
      Log.i(TAG, "key invalidated during store, recreate key")
      deleteKey()
      try {
        createKey()
        getCipher(Cipher.ENCRYPT_MODE, null)
      } catch (t: Throwable) {
        Log.w(TAG, "recreate key failed", t)
        return err("key-invalidated")
      }
    } catch (t: Throwable) {
      Log.w(TAG, "getCipher(encrypt) failed", t)
      return err("unsupported")
    }
    val result = awaitPrompt(cipher) ?: return err("user-cancelled")
    if (result.errorCode != null) return err(mapErrorCode(result.errorCode))
    val authCipher = result.cryptoObject?.cipher ?: return err("auth-failed")
    return try {
      val payload = JSONObject()
        .put("rootId", rootId)
        .put("password", password)
        .toString()
        .toByteArray(Charsets.UTF_8)
      val ciphertext = authCipher.doFinal(payload)
      val blob = authCipher.iv + ciphertext
      prefs.edit().putString(
        PREF_BLOB,
        Base64.encodeToString(blob, Base64.NO_WRAP)
      ).apply()
      ok()
    } catch (t: Throwable) {
      Log.w(TAG, "encrypt/store failed", t)
      err("auth-failed")
    }
  }

  // ------------------------------------------------------------------
  // unlock：生物识别认证后解密返回 {rootId, password}
  // ------------------------------------------------------------------

  fun unlock(): String {
    val blobB64 = prefs.getString(PREF_BLOB, null) ?: return err("no-secret")
    val blob = try {
      Base64.decode(blobB64, Base64.NO_WRAP)
    } catch (t: Throwable) {
      return err("no-secret")
    }
    // GCM 标准 IV 12 字节；blob 过短视为损坏，按无口令处理
    if (blob.size <= 12) return err("no-secret")
    val iv = blob.copyOfRange(0, 12)
    val ciphertext = blob.copyOfRange(12, blob.size)
    val cipher = try {
      getCipher(Cipher.DECRYPT_MODE, GCMParameterSpec(GCM_TAG_BITS, iv))
    } catch (e: KeyPermanentlyInvalidatedException) {
      return err("key-invalidated")
    } catch (t: Throwable) {
      Log.w(TAG, "getCipher(decrypt) failed", t)
      return err("no-secret")
    }
    val result = awaitPrompt(cipher) ?: return err("user-cancelled")
    if (result.errorCode != null) return err(mapErrorCode(result.errorCode))
    val authCipher = result.cryptoObject?.cipher ?: return err("auth-failed")
    return try {
      val plain = authCipher.doFinal(ciphertext)
      val payload = JSONObject(String(plain, Charsets.UTF_8))
      JSONObject()
        .put("ok", true)
        .put("rootId", payload.getString("rootId"))
        .put("password", payload.getString("password"))
        .toString()
    } catch (t: Throwable) {
      Log.w(TAG, "decrypt failed", t)
      err("auth-failed")
    }
  }

  // ------------------------------------------------------------------
  // delete：删 Keystore 密钥 + 清 blob（幂等）
  // ------------------------------------------------------------------

  fun delete(): String {
    try {
      deleteKey()
    } catch (t: Throwable) {
      Log.w(TAG, "deleteKey failed", t)
    }
    prefs.edit().remove(PREF_BLOB).apply()
    return ok()
  }

  // ------------------------------------------------------------------
  // 内部：Keystore / Cipher / Prompt
  // ------------------------------------------------------------------

  private fun enrolled(): Boolean =
    BiometricManager.from(activity)
      .canAuthenticate(BiometricManager.Authenticators.BIOMETRIC_STRONG) ==
      BiometricManager.BIOMETRIC_SUCCESS

  private fun keyStore(): KeyStore =
    KeyStore.getInstance("AndroidKeyStore").apply { load(null) }

  private fun getSecretKey(): SecretKey? =
    (keyStore().getEntry(KEY_ALIAS, null) as? KeyStore.SecretKeyEntry)?.secretKey

  private fun createKey() {
    val builder = KeyGenParameterSpec.Builder(
      KEY_ALIAS,
      KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT
    )
      .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
      .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
      .setInvalidatedByBiometricEnrollment(true)
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
      // 每次使用必须当场认证（timeout=0），仅限 Class 3 生物识别
      builder.setUserAuthenticationParameters(
        0, KeyProperties.AUTH_BIOMETRIC_STRONG
      )
    } else {
      @Suppress("DEPRECATION")
      builder.setUserAuthenticationRequired(true)
    }
    val generator = KeyGenerator.getInstance(
      KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore"
    )
    generator.init(builder.build())
    generator.generateKey()
  }

  private fun deleteKey() {
    val ks = keyStore()
    if (ks.containsAlias(KEY_ALIAS)) ks.deleteEntry(KEY_ALIAS)
  }

  /** 构造指定模式的 Cipher；密钥不存在时先创建（store 首调场景）。 */
  private fun getCipher(mode: Int, spec: GCMParameterSpec?): Cipher {
    val key = getSecretKey() ?: run {
      createKey()
      getSecretKey() ?: throw IllegalStateException("keystore key create failed")
    }
    val cipher = Cipher.getInstance("AES/GCM/NoPadding")
    if (mode == Cipher.ENCRYPT_MODE) {
      cipher.init(mode, key)
    } else {
      cipher.init(mode, key, spec)
    }
    return cipher
  }

  /** BiometricPrompt 回调终态；errorCode 非空即失败终态。 */
  private class PromptResult(
    val cryptoObject: BiometricPrompt.CryptoObject?,
    val errorCode: Int?,
  )

  /**
   * 在 UI 线程弹生物识别认证，当前（Rust 命令）线程 latch 等待终态；
   * 超时按 user-cancelled（用户不操作时最长挂 2min，见方案 §3.3）。
   * 返回 null 仅表示无法弹窗（UI 线程不可用）。
   */
  private fun awaitPrompt(cipher: Cipher): PromptResult? {
    val latch = CountDownLatch(1)
    val resultRef = AtomicReference<PromptResult?>()
    val executor = ContextCompat.getMainExecutor(activity)
    val callback = object : BiometricPrompt.AuthenticationCallback() {
      override fun onAuthenticationSucceeded(result: BiometricPrompt.AuthenticationResult) {
        resultRef.set(PromptResult(result.cryptoObject, null))
        latch.countDown()
      }

      override fun onAuthenticationError(errorCode: Int, errString: CharSequence) {
        Log.i(TAG, "prompt error code=$errorCode msg=$errString")
        resultRef.set(PromptResult(null, errorCode))
        latch.countDown()
      }
      // onAuthenticationFailed 不终结流程（系统继续等下一次按压），不处理
    }
    activity.runOnUiThread {
      try {
        val promptInfo = BiometricPrompt.PromptInfo.Builder()
          .setTitle("验证身份")
          .setSubtitle("使用指纹或人脸以继续")
          .setNegativeButtonText("取消")
          .setAllowedAuthenticators(BiometricManager.Authenticators.BIOMETRIC_STRONG)
          .build()
        BiometricPrompt(activity, executor, callback)
          .authenticate(promptInfo, BiometricPrompt.CryptoObject(cipher))
      } catch (t: Throwable) {
        Log.w(TAG, "authenticate launch failed", t)
        resultRef.set(PromptResult(null, BiometricPrompt.ERROR_HW_UNAVAILABLE))
        latch.countDown()
      }
    }
    val completed = latch.await(PROMPT_TIMEOUT_SECONDS, TimeUnit.SECONDS)
    if (!completed) return PromptResult(null, BiometricPrompt.ERROR_USER_CANCELED)
    return resultRef.get()
  }

  // ------------------------------------------------------------------
  // 返回协议与错误映射
  // ------------------------------------------------------------------

  private fun ok(): String = JSONObject().put("ok", true).toString()

  private fun err(code: String): String =
    JSONObject().put("ok", false).put("error", code).toString()

  private fun mapErrorCode(code: Int): String = when (code) {
    BiometricPrompt.ERROR_USER_CANCELED,
    BiometricPrompt.ERROR_NEGATIVE_BUTTON,
    BiometricPrompt.ERROR_CANCELED -> "user-cancelled"
    BiometricPrompt.ERROR_LOCKOUT,
    BiometricPrompt.ERROR_LOCKOUT_PERMANENT -> "lockout"
    BiometricPrompt.ERROR_NO_BIOMETRICS -> "not-enrolled"
    BiometricPrompt.ERROR_HW_NOT_PRESENT -> "unsupported"
    else -> "auth-failed"
  }
}

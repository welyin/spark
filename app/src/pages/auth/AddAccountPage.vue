<template>
  <section class="auth-panel">
    <h2 class="auth-title">添加账号</h2>
    <p class="hint">把其它设备上的账号迁到本机。</p>

    <!-- 二维码迁移（device-trust-and-biometric 落地路线 §6 添加页）：
         本机「添加账号」= 扫码迁移，载荷用原密码加密，需记得原密码——
         与「忘记密码」天然区隔（迁移=账号在别处活着且记得密码；找回=只有助记词或手机延迟恢复）。
         两种方式：选图片解码（兜底）与摄像头 getUserMedia 取流逐帧 jsQR 解码 -->
    <template v-if="qrStep === 0">
      <div class="qr-source">
        <p class="qr-source-title">二维码在哪？</p>
        <p class="qr-source-desc">旧设备：我的 → 账号备份 →「二维码备份」（需先验证密码）</p>
      </div>
      <div class="qr-card-tip">
        <p class="qr-card-title">别扫成「名片二维码」</p>
        <p class="qr-card-desc">名片二维码只能加好友，不能迁移账号。</p>
      </div>

      <!-- 摄像头扫码：默认入口；取流失败/不支持时下方「选图片」兜底 -->
      <template v-if="!cameraActive">
        <el-button class="qr-camera-btn" type="primary" size="large" :disabled="busy" @click="startCamera">
          <el-icon><Camera /></el-icon>
          摄像头扫码
        </el-button>
        <el-button class="qr-select-btn" plain size="large" :disabled="busy" @click="triggerFileSelect">
          选择二维码图片
        </el-button>
      </template>

      <!-- 取景区：全屏覆盖式摄像头取景，识别成功即自动关闭进入下一步 -->
      <template v-else>
        <div class="camera-fullscreen">
          <video ref="cameraVideo" class="camera-video" muted playsinline autoplay />
          <canvas ref="cameraCanvas" class="camera-canvas" hidden />
          <!-- 取景框：引导用户把二维码对准中央 -->
          <div class="camera-frame" />
          <p class="camera-hint">{{ cameraHint }}</p>
          <el-button class="qr-camera-close" size="small" :disabled="busy" @click="stopCamera">关闭摄像头</el-button>
        </div>
      </template>
    </template>

    <template v-else>
      <p class="hint"><span class="ok-text">已识别备份二维码</span>，请输入原设备上的登录密码完成迁移。</p>
      <!-- 回车与点击统一走 submitMigrate（形态与登录页一致）：@keydown.enter.prevent 显式触发，
           按钮 native-type="button" + @click；@submit.prevent 纯兜底防刷新 -->
      <el-form label-position="top" class="auth-form" @submit.prevent>
        <el-form-item label="原登录密码">
          <el-input v-model="migratePassword" type="password" show-password placeholder="输入原设备上的登录密码" :disabled="busy" @keydown.enter.prevent="submitMigrate" />
        </el-form-item>
        <el-button
          class="submit-btn"
          type="primary"
          native-type="button"
          size="large"
          :loading="busy"
          :disabled="!migratePassword"
          @click="submitMigrate"
        >
          迁移到本机
        </el-button>
      </el-form>
      <div class="entry-link">
        <el-button link type="info" :disabled="busy" @click="qrStep = 0">上一步，重新选择图片</el-button>
      </div>
    </template>

    <input ref="fileInput" type="file" accept="image/*" class="hidden-input" @change="onFileChange" />

    <div class="entry-link">
      <el-button link type="primary" :disabled="busy" @click="emit('recover')">忘记密码，找回账号</el-button>
    </div>
    <div class="entry-link">
      <el-button link type="info" :disabled="busy" @click="emit('back')">返回</el-button>
    </div>

    <el-alert v-if="message" :title="message" type="error" :closable="false" show-icon class="block-gap" />
  </section>
</template>

<script lang="ts">
import { defineComponent, nextTick, onBeforeUnmount, ref } from 'vue';
import { Camera } from '@element-plus/icons-vue';
import { errorMessage } from '../../utils/ipc';
import { decodeQrTextFromCanvas, decodeQrTextFromFile } from '../../utils/qr-decode';

export default defineComponent({
  name: 'AddAccountPage',
  components: {
    Camera
  },
  // 不写 expose 选项：Options API 默认通过组件实例代理暴露 setup return 的全部字段，
  // 模板与父组件 ref 都能访问。RootGate 系统返回键通过 addAccountRef.isCameraActive()/stopCamera() 调用。
  emits: ['recovered', 'recover', 'back'],
  setup(_, { emit }) {
    const busy = ref(false);
    const message = ref('');

    // ---------------- 二维码迁移 ----------------
    const qrStep = ref(0);
    const fileInput = ref<HTMLInputElement | null>(null);
    const qrPayload = ref('');
    const migratePassword = ref('');

    const triggerFileSelect = () => {
      fileInput.value?.click();
    };

    /** 二维码防呆：识别出名片格式（spark-card JSON）而非备份格式时，给针对性提示，不报"看不懂"的解码失败 */
    const isCardPayload = (text: string): boolean => {
      try {
        const parsed = JSON.parse(text) as { type?: string };
        return parsed?.type === 'spark-card';
      } catch {
        return false;
      }
    };

    /**
     * 解码结果统一处理（选图/摄像头共用）：识别失败回第一步，名片码给防呆提示，
     * 合法备份码进入"输入原密码迁移"步。
     */
    const handleDecoded = (decoded: string) => {
      message.value = '';
      if (!decoded) {
        message.value = '无法识别二维码，请确认画面清晰完整';
        qrPayload.value = '';
        qrStep.value = 0;
        return;
      }
      if (isCardPayload(decoded)) {
        message.value = '这是名片二维码，不是账号备份码，请在旧设备「账号备份」里找备份二维码';
        qrPayload.value = '';
        qrStep.value = 0;
        return;
      }
      qrPayload.value = decoded;
      qrStep.value = 1;
    };

    // ---------------- 摄像头扫码（getUserMedia 逐帧 jsQR） ----------------
    const cameraActive = ref(false);
    const cameraHint = ref('请将备份二维码对准取景框');
    const cameraVideo = ref<HTMLVideoElement | null>(null);
    const cameraCanvas = ref<HTMLCanvasElement | null>(null);
    /** 取流重入守卫：双击按钮并发两次 getUserMedia 会让先到的一路 track 泄漏 */
    const cameraStarting = ref(false);
    let cameraStream: MediaStream | null = null;
    let cameraRaf = 0;
    /** 逐帧识别节流：视频帧率通常高于解码耗时，每帧都 getImageData+jsQR 会无谓占 CPU */
    let lastDecodeAt = 0;
    const CAMERA_DECODE_INTERVAL = 200;

    const startCamera = async () => {
      // 重入守卫：请求取流期间再点按钮直接忽略，避免并发 getUserMedia 泄漏 track
      if (cameraStarting.value || cameraActive.value) {
        return;
      }
      cameraStarting.value = true;
      cameraHint.value = '正在请求摄像头…';
      try {
        // 优先后置摄像头（环境），不可用则回落默认设备（桌面摄像头 / 前置）
        const constraints: MediaStreamConstraints = {
          video: { facingMode: { ideal: 'environment' } }
        };
        let stream: MediaStream;
        try {
          stream = await navigator.mediaDevices.getUserMedia(constraints);
        } catch {
          // 无摄像头 / 权限被拒：回退提示用图片解码
          message.value = '摄像头不可用或未授权，请改用「选择二维码图片」';
          return;
        }
        cameraStream = stream;
        cameraActive.value = true;
        message.value = '';
        // 置 cameraActive 后模板才渲染 <video>，ref 需等异步 patch 完成再取，
        // 否则同步读恒为 null，srcObject 永不挂载、解码循环永不启动（黑屏）。
        await nextTick();
        const video = cameraVideo.value;
        if (video) {
          video.srcObject = stream;
          // video 就绪后再开始逐帧；muted+playsinline+autoplay 已由模板保证自动播放
          video.onloadeddata = () => {
            cameraHint.value = '请将备份二维码对准取景框';
            loopDecodeFrame();
          };
        }
      } finally {
        cameraStarting.value = false;
      }
    };

    /** 逐帧解码循环：video 当前帧画到 canvas → jsQR 快路径解码 → 命中即停流进入解码头流程 */
    const loopDecodeFrame = () => {
      const video = cameraVideo.value;
      const canvas = cameraCanvas.value;
      if (!video || !canvas || video.readyState < 2) {
        cameraRaf = requestAnimationFrame(loopDecodeFrame);
        return;
      }
      const now = Date.now();
      if (now - lastDecodeAt >= CAMERA_DECODE_INTERVAL) {
        lastDecodeAt = now;
        // 画布按 video 实际画面尺寸建一次（避免每帧改宽高重排）
        if (canvas.width !== video.videoWidth || canvas.height !== video.videoHeight) {
          canvas.width = video.videoWidth;
          canvas.height = video.videoHeight;
        }
        const ctx = canvas.getContext('2d', { willReadFrequently: true });
        if (ctx) {
          ctx.drawImage(video, 0, 0, canvas.width, canvas.height);
          const decoded = decodeQrTextFromCanvas(canvas);
          if (decoded) {
            stopCamera();
            handleDecoded(decoded);
            return;
          }
        }
      }
      cameraRaf = requestAnimationFrame(loopDecodeFrame);
    };

    /** 停止取流：取消 rAF 循环 + 停掉全部 track + 释放 video 资源（卸载/取消时必须调用） */
    const stopCamera = () => {
      cancelAnimationFrame(cameraRaf);
      cameraStream?.getTracks().forEach((track) => track.stop());
      cameraStream = null;
      cameraActive.value = false;
      if (cameraVideo.value) {
        cameraVideo.value.srcObject = null;
        cameraVideo.value.onloadeddata = null;
      }
    };

    onBeforeUnmount(stopCamera);

    const onFileChange = async (event: Event) => {
      const input = event.target as HTMLInputElement;
      const file = input.files?.[0];
      input.value = '';
      if (!file) {
        return;
      }
      stopCamera();
      try {
        const decoded = await decodeQrTextFromFile(file);
        handleDecoded(decoded);
      } catch {
        message.value = '图片读取失败，请换一张图片重试';
        qrPayload.value = '';
        qrStep.value = 0;
      }
    };

    const submitMigrate = async () => {
      // 回车提交不走按钮 disabled，需自查
      if (busy.value || !migratePassword.value) {
        return;
      }
      busy.value = true;
      message.value = '';
      try {
        const result = await window.electronAPI.rootIdentity.recoverBackup(qrPayload.value, migratePassword.value);
        emit('recovered', result.rootId);
      } catch (error) {
        message.value = `迁移失败：${errorMessage(error)}`;
      } finally {
        busy.value = false;
      }
    };

    return {
      busy,
      message,
      fileInput,
      qrStep,
      migratePassword,
      cameraActive,
      cameraHint,
      cameraVideo,
      cameraCanvas,
      triggerFileSelect,
      startCamera,
      stopCamera,
      onFileChange,
      submitMigrate,
      // 供父组件 RootGate 系统返回键调用：箭头函数包装避免 Options API 的 this 绑定坑
      isCameraActive: () => cameraActive.value,
      emit
    };
  }
});
</script>

<style scoped src="../../styles/pages/auth/add-account.css"></style>

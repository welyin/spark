<template>
  <div class="card-input">
    <div class="card-input-actions">
      <div
        class="card-upload"
        :class="{ ok: status === 'ok' }"
        @click="openPicker"
      >
        <input
          ref="fileInput"
          type="file"
          accept="image/*"
          class="card-file-input"
          @change="onPick"
        />
        <template v-if="status === 'idle'">
          <div class="card-upload-icon">＋</div>
          <div class="card-upload-text">点击上传对方名片图片</div>
          <div class="card-upload-hint">支持 PNG / JPG，自动识别名片内容</div>
        </template>
        <template v-else-if="status === 'ok'">
          <div class="card-upload-icon ok">✓</div>
          <div class="card-upload-text">{{ fileName }}</div>
          <div class="card-upload-hint ok-text">已识别名片，点击可更换</div>
        </template>
        <template v-else>
          <div class="card-upload-icon">＋</div>
          <div class="card-upload-text">{{ fileName }}</div>
          <div class="card-upload-hint warn-text">未识别到名片二维码，请换一张，或在下方粘贴名片内容</div>
        </template>
      </div>
      <!-- 摄像头扫码入口：仅移动端（窄屏 ≤768px）展示（ui-contacts §4.1.1），桌面以拖拽/上传为主 -->
      <el-button
        v-if="isMobileLayout"
        class="card-scan-btn"
        :icon="Camera"
        @click="startScan"
      >
        扫码
      </el-button>
    </div>
    <label class="field-label">或粘贴对方名片内容</label>
    <textarea
      v-model="text"
      class="input card-textarea"
      placeholder="粘贴对方发来的名片内容，系统会自动识别其中的身份信息"
    ></textarea>

    <!-- 全屏取景：摄像头逐帧识别对方手机屏幕上的名片二维码（ui-contacts §4.1.1） -->
    <div v-if="cameraActive" class="camera-fullscreen">
      <video ref="cameraVideo" class="camera-video" muted playsinline autoplay />
      <canvas ref="cameraCanvas" class="camera-canvas" hidden />
      <div class="camera-frame" />
      <p class="camera-hint">{{ cameraHint }}</p>
      <el-button class="camera-close" size="small" @click="stopScan">关闭摄像头</el-button>
    </div>
  </div>
</template>

<script lang="ts">
import { defineComponent, ref } from 'vue';
import { Camera } from '@element-plus/icons-vue';
import { ElMessage } from 'element-plus';
import { decodeCardImage, parseCard } from '../../utils/card';
import { useQrCameraScan } from '../../composables/useQrCameraScan';
import { isMobileLayout } from '../../stores/ui-layout';

export default defineComponent({
  name: 'CardInput',
  components: { Camera },
  props: {
    modelValue: { type: String, default: '' },
  },
  emits: ['update:modelValue'],
  data() {
    return {
      text: '',
      decoded: '',
      fileName: '',
      status: 'idle' as 'idle' | 'ok' | 'fail',
    };
  },
  setup() {
    const cameraVideo = ref<HTMLVideoElement | null>(null);
    const cameraCanvas = ref<HTMLCanvasElement | null>(null);
    /** 扫码识别出的有效名片文本（桥接组合式函数回调 → Options API data.decoded，经 watch 消费） */
    const scanResult = ref('');
    const { isActive, hint, start, stop } = useQrCameraScan({
      videoRef: cameraVideo,
      canvasRef: cameraCanvas,
      // 解码命中：校验是有效名片才填入；否则保持取景继续扫
      onDecoded: (text) => {
        if (parseCard(text)) {
          scanResult.value = text;
          return true;
        }
        hint.value = '识别到内容，但不是有效名片，请对准名片二维码';
        return false;
      },
      // 无摄像头/权限拒绝：降级提示改用上传图片
      onError: (message) => {
        ElMessage.warning(message);
      }
    });
    return {
      cameraVideo,
      cameraCanvas,
      cameraActive: isActive,
      cameraHint: hint,
      startScan: start,
      stopScan: stop,
      scanResult,
      isMobileLayout,
      Camera,
    };
  },
  watch: {
    decoded() {
      this.emitValue();
    },
    text() {
      this.emitValue();
    },
    modelValue(val: string) {
      if (!val && (this.decoded || this.text)) {
        this.reset();
      }
    },
    // 摄像头扫码识别出有效名片 → 填入 decoded 并走既有 emit 链
    scanResult(val: string) {
      if (val) {
        this.decoded = val;
        this.fileName = '摄像头扫码';
        this.status = 'ok';
      }
    },
  },
  methods: {
    reset() {
      this.decoded = '';
      this.text = '';
      this.fileName = '';
      this.status = 'idle';
      const input = this.$refs.fileInput as HTMLInputElement | undefined;
      if (input) input.value = '';
    },
    openPicker() {
      (this.$refs.fileInput as HTMLInputElement | undefined)?.click();
    },
    emitValue() {
      this.$emit('update:modelValue', this.decoded || this.text.trim());
    },
    async onPick(e: Event) {
      const input = e.target as HTMLInputElement;
      const file = input.files?.[0];
      if (!file) return;
      this.fileName = file.name;
      const decoded = await decodeCardImage(file);
      this.decoded = decoded;
      this.status = decoded ? 'ok' : 'fail';
    },
  },
});
</script>

<style scoped>
.card-input-actions {
  display: flex;
  gap: 10px;
  align-items: stretch;
}

.card-upload {
  flex: 1;
  min-width: 0;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 6px;
  padding: 18px 14px;
  border: 1px dashed var(--spark-border);
  border-radius: var(--spark-radius-s);
  cursor: pointer;
  transition: border-color 0.15s, background 0.15s;
}

.card-upload:hover {
  border-color: var(--spark-primary);
  background: var(--spark-primary-light);
}

.card-upload.ok {
  border-style: solid;
  border-color: var(--spark-primary);
}

/* 摄像头扫码按钮：与上传区等高，移动端专属（ui-contacts §4.1.1） */
.card-scan-btn {
  flex-shrink: 0;
  align-self: stretch;
  height: auto;
  min-height: 100px;
  font-size: 13px;
}

.card-upload-icon {
  font-size: 22px;
  line-height: 1;
  color: var(--spark-text-2);
}

.card-upload-icon.ok {
  color: var(--spark-primary);
}

.card-upload-text {
  font-size: 13px;
  color: var(--spark-text-1);
}

.card-upload-hint {
  font-size: 12px;
  color: var(--spark-text-3);
}

.card-file-input {
  display: none;
}

.ok-text {
  color: var(--spark-primary);
}

.warn-text {
  color: var(--spark-warning);
}

.field-label {
  display: block;
  margin-top: 12px;
  margin-bottom: 6px;
  font-size: 13px;
  color: var(--spark-text-2);
}

.card-textarea {
  width: 100%;
  min-height: 96px;
  resize: vertical;
}

/* ---- 摄像头全屏取景（复用添加账号页取景样式，ui-contacts §4.1.1） ---- */
.camera-fullscreen {
  position: fixed;
  inset: 0;
  z-index: 2000;
  background: #000;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
}

.camera-fullscreen .camera-video {
  width: 100%;
  height: 100%;
  object-fit: cover;
}

.camera-frame {
  position: absolute;
  top: 50%;
  left: 50%;
  width: 60vmin;
  height: 60vmin;
  max-width: 280px;
  max-height: 280px;
  transform: translate(-50%, -50%);
  border: 2px solid rgba(255, 255, 255, 0.8);
  border-radius: 12px;
  box-shadow: 0 0 0 9999px rgba(0, 0, 0, 0.4);
  pointer-events: none;
}

.camera-hint {
  position: absolute;
  top: calc(50% + 32vmin);
  left: 0;
  right: 0;
  margin: 0;
  padding: 0 16px;
  font-size: 14px;
  text-align: center;
  color: rgba(255, 255, 255, 0.85);
}

.camera-close {
  position: absolute;
  top: calc(100vh - 56px - env(safe-area-inset-bottom, 0px));
  left: 50%;
  transform: translateX(-50%);
  width: auto;
  min-width: 120px;
}
</style>

/**
 * 摄像头二维码逐帧扫码组合式函数（名片识别 / 账号迁移共用）。
 *
 * 从 AddAccountPage 的摄像头实现中抽象出可复用的取流/解码/清理逻辑：
 * - getUserMedia 取流：优先后置摄像头（environment），不可用回退默认设备；
 * - 逐帧 rAF 循环 + 200ms 节流：video 当前帧画到隐藏 canvas → decodeQrTextFromCanvas
 *   （jsQR 快路径，为摄像头实时帧设计，见 utils/qr-decode.ts）；
 * - 解码命中经回调上抛，调用方自决（名片填 decoded / 备份码进迁移步）；
 * - 停止/清理收敛在此：取消 rAF + 停全部 track + 释放 video，组件卸载时自动调用，
 *   防摄像头指示灯常亮（track 泄漏）。
 *
 * 职责边界：本函数只管「取流 + 逐帧识别 + 清理」，不做业务判定（是否有效名片等）。
 * `onDecoded` 返回布尔值表示是否「消费成功」：返回 true 停止取景（识别完成）；
 * 返回 false 表示解码文本业务上无效（如非名片码），继续取景等待下一次命中。
 * 副作用全收敛于此，符合组件规范 §4.3。
 */
import { nextTick, onBeforeUnmount, ref, type Ref } from 'vue';
import { decodeQrTextFromCanvas } from './qr-decode';

/** 逐帧识别节流：视频帧率通常高于解码耗时，每帧 getImageData+jsQR 会无谓占 CPU */
const CAMERA_DECODE_INTERVAL = 200;

/**
 * 摄像头高级约束（Chromium 非标准扩展：focusMode/zoom 未进 lib.dom 的
 * MediaTrackConstraintSet 类型，但 Android WebView 运行时支持）。放宽类型仅为此
 * 扩展字段，其余仍受 MediaTrackConstraintSet 约束。
 */
type MediaAdvancedConstraint = MediaTrackConstraintSet & {
  focusMode?: { ideal: string };
  zoom?: { ideal: number };
};

export type UseQrCameraScanOptions = {
  /** 取景 <video> 元素 ref（模板渲染后由调用方绑定，start 后 nextTick 再取） */
  videoRef: Ref<HTMLVideoElement | null>;
  /** 隐藏 <canvas> 元素 ref（逐帧画帧解码用） */
  canvasRef: Ref<HTMLCanvasElement | null>;
  /** 解码命中回调：返回 true 表示消费成功（停止取景），false 表示业务无效（继续取景） */
  onDecoded: (text: string) => boolean;
  /** 取流失败 / 权限拒绝 / 无摄像头 回调：参数为降级提示文案 */
  onError: (message: string) => void;
};

export function useQrCameraScan({ videoRef, canvasRef, onDecoded, onError }: UseQrCameraScanOptions) {
  /** 是否取景中（模板据此切换按钮与取景区） */
  const isActive = ref(false);
  /** 取景提示文案（请求中 / 对准二维码 / 关闭等） */
  const hint = ref('');
  /** 取流重入守卫：双击按钮并发两次 getUserMedia 会让先到的一路 track 泄漏 */
  const isStarting = ref(false);

  let stream: MediaStream | null = null;
  let raf = 0;
  let lastDecodeAt = 0;

  /** 停止取流：取消 rAF + 停全部 track + 释放 video（取消/卸载/识别成功均须调用） */
  const stop = () => {
    cancelAnimationFrame(raf);
    stream?.getTracks().forEach((track) => track.stop());
    stream = null;
    isActive.value = false;
    const video = videoRef.value;
    if (video) {
      video.srcObject = null;
      video.onloadeddata = null;
    }
  };

  /** 逐帧解码循环：video 当前帧画到 canvas → jsQR 快路径解码 → 命中即停流并回调 */
  const loopDecodeFrame = () => {
    const video = videoRef.value;
    const canvas = canvasRef.value;
    if (!video || !canvas || video.readyState < 2) {
      raf = requestAnimationFrame(loopDecodeFrame);
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
          // onDecoded 返回 true=消费成功（停止取景）；false=业务无效（继续取景等下一次命中）
          if (onDecoded(decoded)) {
            stop();
            return;
          }
        }
      }
    }
    raf = requestAnimationFrame(loopDecodeFrame);
  };

  /** 开始取流：成功则进入取景与解码循环，失败则 onError 降级提示 */
  const start = async () => {
    // 重入守卫：请求取流期间再点按钮直接忽略，避免并发 getUserMedia 泄漏 track
    if (isStarting.value || isActive.value) {
      return;
    }
    isStarting.value = true;
    hint.value = '正在请求摄像头…';
    try {
      // 手机摄像头默认即连续自动对焦（CAF），此处显式提示连续对焦 + 适度放大。
      // 注意：focusMode / zoom 是 Chromium 非标准扩展，Android WebView 支持不一——
      // 必须用 ideal（软性要求，不支持会被忽略）而非 required（写死会导致取流失败），
      // 且整体做失败回退到基础约束，确保任何设备都能取到流。
      const baseConstraints: MediaStreamConstraints = {
        video: { facingMode: { ideal: 'environment' } }
      };
      const autofocusConstraints: MediaStreamConstraints = {
        video: {
          facingMode: { ideal: 'environment' },
          advanced: [
            { focusMode: { ideal: 'continuous' } },
            { zoom: { ideal: 2 } }
          ] as MediaAdvancedConstraint[]
        }
      };
      let gotStream: MediaStream;
      try {
        gotStream = await navigator.mediaDevices.getUserMedia(autofocusConstraints);
      } catch {
        // focusMode/zoom 不受支持（报错）时回退到基础约束，保证仍能取流
        try {
          gotStream = await navigator.mediaDevices.getUserMedia(baseConstraints);
        } catch {
          // 无摄像头 / 权限被拒：降级提示改用上传图片，调用方负责展示
          onError('摄像头不可用或未授权，请改用上传名片图片');
          return;
        }
      }
      stream = gotStream;
      isActive.value = true;
      hint.value = '请将对方名片的二维码对准取景框';
      // 置 isActive 后模板才渲染 <video>，ref 需等异步 patch 完成再取，
      // 否则同步读恒为 null，srcObject 永不挂载、解码循环永不启动（黑屏）。
      await nextTick();
      const video = videoRef.value;
      if (video) {
        video.srcObject = gotStream;
        // video 就绪后再开始逐帧；muted+playsinline+autoplay 由模板保证自动播放
        video.onloadeddata = () => {
          hint.value = '请将对方名片的二维码对准取景框';
          loopDecodeFrame();
        };
      }
    } finally {
      isStarting.value = false;
    }
  };

  // 组件卸载时必须停止取流，防 track 泄漏（摄像头指示灯常亮）
  onBeforeUnmount(stop);

  return { isActive, hint, start, stop };
}

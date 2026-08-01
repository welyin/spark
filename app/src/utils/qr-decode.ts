/**
 * 二维码解码共享工具（jsQR + 缩放阶梯）。
 *
 * 为什么需要缩放阶梯：jsQR 对部分 QR version 的原图存在定位失败（finder pattern
 * 定位依赖模块像素对齐，特定版本/宽度组合下非单调失败，无缩放变换时无解）。
 * 实测记录（qrcode 库生成，ECC M / margin 1）：
 * - 700B 载荷 340px：1x 失败，0.5x 成功
 * - 900B 载荷 320px：1x 失败，0.75x 成功
 * - 983B 载荷 320px：1x 失败，0.5x 成功
 * 按 [1, 0.75, 0.5, 0.35, 0.25] 阶梯最近邻缩小重试可全部解码成功。
 *
 * 三个调用点共用：名片二维码（utils/card.ts）、账号恢复（pages/auth/RecoverPage.vue）、
 * 连接名片（components/org/RecoverConnectionPanel.vue）。
 */
import jsQR from 'jsqr';

/** jsQR 定位失败的规避阶梯：从原图逐档缩小，首个成功即返回 */
const SCALE_LADDER = [1, 0.75, 0.5, 0.35, 0.25];

/** jsQR 输入所需的像素结构（与 canvas ImageData 同形） */
export type QrImageData = {
  data: Uint8ClampedArray;
  width: number;
  height: number;
};

/** 最近邻缩小采样（阶梯缩小不依赖 canvas，测试环境无 canvas 2D 也可运行） */
function downscale(source: QrImageData, scale: number): QrImageData {
  const width = Math.max(1, Math.round(source.width * scale));
  const height = Math.max(1, Math.round(source.height * scale));
  const data = new Uint8ClampedArray(width * height * 4);
  for (let y = 0; y < height; y++) {
    const sy = Math.min(source.height - 1, Math.floor(y / scale));
    for (let x = 0; x < width; x++) {
      const sx = Math.min(source.width - 1, Math.floor(x / scale));
      const from = (sy * source.width + sx) * 4;
      const to = (y * width + x) * 4;
      data[to] = source.data[from];
      data[to + 1] = source.data[from + 1];
      data[to + 2] = source.data[from + 2];
      data[to + 3] = source.data[from + 3];
    }
  }
  return { data, width, height };
}

/** 按缩放阶梯逐档 jsQR 解码，首个成功即返回文本；全部失败返回 '' */
export function decodeQrTextFromImageData(imageData: QrImageData): string {
  for (const scale of SCALE_LADDER) {
    const scaled = scale === 1 ? imageData : downscale(imageData, scale);
    const decoded = jsQR(scaled.data, scaled.width, scaled.height);
    if (decoded?.data) {
      return decoded.data;
    }
  }
  return '';
}

/** 读取图片文件并解码二维码文本（识别失败 / 图片读取失败均返回 ''） */
export function decodeQrTextFromFile(file: File): Promise<string> {
  return new Promise((resolve) => {
    const url = URL.createObjectURL(file);
    const image = new Image();
    image.onload = () => {
      URL.revokeObjectURL(url);
      const canvas = document.createElement('canvas');
      canvas.width = image.naturalWidth;
      canvas.height = image.naturalHeight;
      const ctx = canvas.getContext('2d');
      if (!ctx) {
        resolve('');
        return;
      }
      ctx.drawImage(image, 0, 0);
      const imageData = ctx.getImageData(0, 0, canvas.width, canvas.height);
      resolve(decodeQrTextFromImageData(imageData));
    };
    image.onerror = () => {
      URL.revokeObjectURL(url);
      resolve('');
    };
    image.src = url;
  });
}

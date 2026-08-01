/**
 * 二维码解码共享工具（jsQR + 两段式解码管线）。
 *
 * 第一段：快路径（干净图）——最近邻缩放阶梯。
 * 为什么需要缩放阶梯：jsQR 对部分 QR version 的原图存在定位失败（finder pattern
 * 定位依赖模块像素对齐，特定版本/宽度组合下非单调失败，无缩放变换时无解）。
 * 实测记录（qrcode 库生成，ECC M / margin 1）：
 * - 700B 载荷 340px：1x 失败，0.5x 成功
 * - 900B 载荷 320px：1x 失败，0.75x 成功
 * - 983B 载荷 320px：1x 失败，0.5x 成功
 * 按 [1, 0.75, 0.5, 0.35, 0.25] 阶梯最近邻缩小重试可全部解码成功。
 *
 * 第二段：抗摩尔纹路径（拍屏图）——面积平均缩放 + 灰度二值化。
 * 手机屏幕翻拍的照片带屏幕像素摩尔纹（高频明暗条纹），最近邻采样会保留甚至
 * 混叠摩尔纹，阶梯 × 裁剪 × 阈值组合均失败（实测：3072x4096 拍屏原图，
 * 最近邻 × 缩放阶梯 × 阈值 [110,128,150] 全部失败）；面积平均（box-average）
 * 是低通滤波，整周期平均可消除高频纹理。实测：面积平均 f=0.15 + 阈值 150
 * 一次解码成功（882 字符紧凑备份 JSON）。管线：面积平均缩放档
 * [0.5, 0.35, 0.25, 0.15, 0.1] × 阈值档 [无, 128, 150, 170]，首个成功即返回。
 *
 * 三个调用点共用：名片二维码（utils/card.ts）、账号恢复（pages/auth/RecoverPage.vue）、
 * 连接名片（components/org/RecoverConnectionPanel.vue）。
 */
import jsQR from 'jsqr';

/** 快路径缩放阶梯：jsQR 定位失败的规避档位，从原图逐档缩小 */
const SCALE_LADDER = [1, 0.75, 0.5, 0.35, 0.25];

/** 抗摩尔纹路径：面积平均缩放档（低通消高频纹理） */
const ANTI_MOIRE_SCALES = [0.5, 0.35, 0.25, 0.15, 0.1];

/** 抗摩尔纹路径：灰度二值化阈值档（0 表示不二值化，直接用平均后的灰度图） */
const ANTI_MOIRE_THRESHOLDS = [0, 128, 150, 170];

/** 抗摩尔纹路径的尺寸上限：拍屏大图先最近邻缩到该边长内，控制多级平均耗时 */
const ANTI_MOIRE_MAX_SIDE = 1600;

/** jsQR 输入所需的像素结构（与 canvas ImageData 同形） */
export type QrImageData = {
  data: Uint8ClampedArray;
  width: number;
  height: number;
};

/** 最近邻缩小采样（快路径用；不依赖 canvas，测试环境无 canvas 2D 也可运行） */
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

/** 面积平均缩小（box-average 低通）：每个输出像素取对应源矩形内像素的均值，可消除摩尔纹等高频纹理 */
function boxAverage(source: QrImageData, scale: number): QrImageData {
  const width = Math.max(1, Math.round(source.width * scale));
  const height = Math.max(1, Math.round(source.height * scale));
  const data = new Uint8ClampedArray(width * height * 4);
  for (let y = 0; y < height; y++) {
    const y0 = Math.floor(y / scale);
    const y1 = Math.min(source.height, Math.max(y0 + 1, Math.floor((y + 1) / scale)));
    for (let x = 0; x < width; x++) {
      const x0 = Math.floor(x / scale);
      const x1 = Math.min(source.width, Math.max(x0 + 1, Math.floor((x + 1) / scale)));
      let r = 0;
      let g = 0;
      let b = 0;
      for (let sy = y0; sy < y1; sy++) {
        for (let sx = x0; sx < x1; sx++) {
          const from = (sy * source.width + sx) * 4;
          r += source.data[from];
          g += source.data[from + 1];
          b += source.data[from + 2];
        }
      }
      const count = (x1 - x0) * (y1 - y0);
      const to = (y * width + x) * 4;
      data[to] = r / count;
      data[to + 1] = g / count;
      data[to + 2] = b / count;
      data[to + 3] = 255;
    }
  }
  return { data, width, height };
}

/** 灰度二值化：大于等于阈值置白，否则置黑（抑制平均后残留的灰阶波纹） */
function binarize(source: QrImageData, threshold: number): QrImageData {
  const data = new Uint8ClampedArray(source.data.length);
  for (let i = 0; i < source.width * source.height; i++) {
    const from = i * 4;
    const gray = (source.data[from] * 299 + source.data[from + 1] * 587 + source.data[from + 2] * 114) / 1000;
    const value = gray >= threshold ? 255 : 0;
    data[from] = value;
    data[from + 1] = value;
    data[from + 2] = value;
    data[from + 3] = 255;
  }
  return { data, width: source.width, height: source.height };
}

function decodeJsQr(imageData: QrImageData): string {
  return jsQR(imageData.data, imageData.width, imageData.height)?.data ?? '';
}

/** 快路径：最近邻缩放阶梯逐档 jsQR（干净图在此返回；拍屏摩尔纹图会全部失败） */
export function decodeQrFastFromImageData(imageData: QrImageData): string {
  for (const scale of SCALE_LADDER) {
    const decoded = decodeJsQr(scale === 1 ? imageData : downscale(imageData, scale));
    if (decoded) {
      return decoded;
    }
  }
  return '';
}

/** 抗摩尔纹路径：面积平均缩放档 × 阈值档，首个成功即返回；全部失败返回 '' */
function decodeQrAntiMoireFromImageData(imageData: QrImageData): string {
  // 拍屏大图先最近邻缩到上限尺寸，控制多级面积平均的总耗时
  const maxSide = Math.max(imageData.width, imageData.height);
  const base = maxSide > ANTI_MOIRE_MAX_SIDE ? downscale(imageData, ANTI_MOIRE_MAX_SIDE / maxSide) : imageData;
  for (const scale of ANTI_MOIRE_SCALES) {
    const averaged = boxAverage(base, scale);
    for (const threshold of ANTI_MOIRE_THRESHOLDS) {
      const decoded = decodeJsQr(threshold === 0 ? averaged : binarize(averaged, threshold));
      if (decoded) {
        return decoded;
      }
    }
  }
  return '';
}

/** 解码二维码文本：先快路径（干净图），失败后进抗摩尔纹路径（拍屏图）；全部失败返回 '' */
export function decodeQrTextFromImageData(imageData: QrImageData): string {
  return decodeQrFastFromImageData(imageData) || decodeQrAntiMoireFromImageData(imageData);
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

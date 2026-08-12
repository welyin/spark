/**
 * 朋友圈插件（spark-moments）· 图片处理工具（composer.md §3 / 产品 §6.4）。
 *
 * 发布管线第一步：逐张生成缩略图（canvas ~256×256，JPEG 质量 70%）并 saveBlob
 * 原图 + 缩略图（内容哈希寻址，返回 {hash, thumbHash, name, size, mime}）。
 * 解码失败（HEIC 等 WebView 不可解码格式）抛错，调用方按「去除后继续」处理。
 */

import type { PluginSDK } from '../../../packages/plugin-sdk/src';
import { MOMENTS_THUMB_QUALITY, MOMENTS_THUMB_SIZE, type MomentsImage } from '../model';

/** File → base64（去 data URL 前缀） */
export function fileToBase64(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => {
      const result = String(reader.result ?? '');
      const comma = result.indexOf(',');
      resolve(comma >= 0 ? result.slice(comma + 1) : result);
    };
    reader.onerror = () => reject(new Error('读取文件失败'));
    reader.readAsDataURL(file);
  });
}

/** 生成缩略图 base64（canvas 缩放至 ~256×256，JPEG 质量 70%） */
export async function makeThumbnail(dataBase64: string, mime: string): Promise<string> {
  const img = await loadImage(`data:${mime};base64,${dataBase64}`);
  const canvas = document.createElement('canvas');
  const scale = Math.min(1, MOMENTS_THUMB_SIZE / Math.max(img.width, img.height));
  canvas.width = Math.max(1, Math.round(img.width * scale));
  canvas.height = Math.max(1, Math.round(img.height * scale));
  const ctx = canvas.getContext('2d');
  if (!ctx) throw new Error('canvas 不可用');
  ctx.drawImage(img, 0, 0, canvas.width, canvas.height);
  return canvas.toDataURL('image/jpeg', MOMENTS_THUMB_QUALITY).split(',')[1];
}

/** 处理单张图：saveBlob 原图 + 缩略图，返回 MomentsImage */
export async function processImage(file: File, sdk: PluginSDK): Promise<MomentsImage> {
  const dataBase64 = await fileToBase64(file);
  const thumbBase64 = await makeThumbnail(dataBase64, file.type || 'image/jpeg');

  const [full, thumb] = await Promise.all([
    sdk.data.saveBlob(dataBase64),
    sdk.data.saveBlob(thumbBase64)
  ]);

  return {
    hash: full.hash,
    thumbHash: thumb.hash,
    name: file.name,
    size: file.size,
    mime: file.type || 'image/jpeg'
  };
}

function loadImage(src: string): Promise<HTMLImageElement> {
  return new Promise((resolve, reject) => {
    const img = new Image();
    img.onload = () => resolve(img);
    img.onerror = () => reject(new Error('图片解码失败'));
    img.src = src;
  });
}

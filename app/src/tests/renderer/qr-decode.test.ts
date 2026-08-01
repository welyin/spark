// 缩放阶梯解码单测：jsdom 无 canvas 2D，用 qrcode 库生成 dataUrl 后自带极简 PNG 解码成像素，
// 直接测阶梯函数 decodeQrTextFromImageData（文件选择链路不测）。
// 失败样本来自实测记录（qrcode 库 ECC M / margin 1）：
// 700B@340px / 900B@320px / 983B@320px 原图 jsQR 定位失败，阶梯缩小后可解码。
import { describe, expect, it } from 'vitest';
import { inflateSync } from 'node:zlib';
import QRCode from 'qrcode';
import jsQR from 'jsqr';
import { decodeQrTextFromImageData, type QrImageData } from '../../utils/qr-decode';

/** 极简 PNG 解码器：仅支持 8bit RGB/RGBA、无隔行（qrcode 库 node 端输出即此格式） */
function decodePng(dataUrl: string): QrImageData {
  const buf = Buffer.from(dataUrl.split(',')[1], 'base64');
  let width = 0;
  let height = 0;
  let bitDepth = 0;
  let colorType = 0;
  const idat: Buffer[] = [];
  let offset = 8; // 跳过 8 字节签名
  while (offset < buf.length) {
    const length = buf.readUInt32BE(offset);
    const type = buf.toString('ascii', offset + 4, offset + 8);
    const chunk = buf.subarray(offset + 8, offset + 8 + length);
    if (type === 'IHDR') {
      width = chunk.readUInt32BE(0);
      height = chunk.readUInt32BE(4);
      bitDepth = chunk[8];
      colorType = chunk[9];
    } else if (type === 'IDAT') {
      idat.push(chunk);
    } else if (type === 'IEND') {
      break;
    }
    offset += 12 + length;
  }
  if (bitDepth !== 8 || (colorType !== 6 && colorType !== 2)) {
    throw new Error(`不支持的 PNG 格式：bitDepth=${bitDepth} colorType=${colorType}`);
  }
  const bpp = colorType === 6 ? 4 : 3;
  const raw = inflateSync(Buffer.concat(idat));
  const stride = width * bpp;
  const out = new Uint8ClampedArray(width * height * 4);
  const paeth = (a: number, b: number, c: number) => {
    const p = a + b - c;
    const pa = Math.abs(p - a);
    const pb = Math.abs(p - b);
    const pc = Math.abs(p - c);
    return pa <= pb && pa <= pc ? a : pb <= pc ? b : c;
  };
  let prev = new Uint8Array(stride);
  for (let y = 0; y < height; y++) {
    const filter = raw[y * (stride + 1)];
    const row = raw.subarray(y * (stride + 1) + 1, (y + 1) * (stride + 1));
    const cur = new Uint8Array(stride);
    for (let x = 0; x < stride; x++) {
      const a = x >= bpp ? cur[x - bpp] : 0;
      const b = prev[x];
      const c = x >= bpp ? prev[x - bpp] : 0;
      let v = row[x];
      if (filter === 1) v += a;
      else if (filter === 2) v += b;
      else if (filter === 3) v += (a + b) >> 1;
      else if (filter === 4) v += paeth(a, b, c);
      cur[x] = v & 0xff;
    }
    for (let x = 0; x < width; x++) {
      const from = x * bpp;
      const to = (y * width + x) * 4;
      out[to] = cur[from];
      out[to + 1] = cur[from + 1];
      out[to + 2] = cur[from + 2];
      out[to + 3] = bpp === 4 ? cur[from + 3] : 255;
    }
    prev = cur;
  }
  return { data: out, width, height };
}

/** 与生成端一致：qrcode 库 ECC M / margin 1，输出像素图 */
async function makeQrImage(text: string, width: number): Promise<QrImageData> {
  const dataUrl = await QRCode.toDataURL(text, { errorCorrectionLevel: 'M', margin: 1, width });
  return decodePng(dataUrl);
}

describe('decodeQrTextFromImageData（缩放阶梯）', () => {
  it('100B 小载荷 1x 直接成功', async () => {
    const payload = 'A'.repeat(100);
    const image = await makeQrImage(payload, 320);
    // 前提：原图（1x）jsQR 即可解码
    expect(jsQR(image.data, image.width, image.height)?.data).toBe(payload);
    expect(decodeQrTextFromImageData(image)).toBe(payload);
  });

  // 实测 jsQR 原图定位失败的样本（载荷长度 + 生成宽度）
  it.each([
    { length: 700, width: 340 },
    { length: 900, width: 320 },
    { length: 983, width: 320 }
  ])('$lengthB 载荷（${width}px）原图失败、经阶梯解码成功且内容一致', async ({ length, width }) => {
    const payload = 'A'.repeat(length);
    const image = await makeQrImage(payload, width);
    // 前提：原图（1x）jsQR 确实定位失败，否则样本失效
    expect(jsQR(image.data, image.width, image.height)).toBeNull();
    expect(decodeQrTextFromImageData(image)).toBe(payload);
  });

  it('非二维码图片返回空串', () => {
    const size = 200;
    const data = new Uint8ClampedArray(size * size * 4);
    for (let i = 0; i < data.length; i++) {
      data[i] = Math.floor(Math.random() * 256);
    }
    expect(decodeQrTextFromImageData({ data, width: size, height: size })).toBe('');
  });
});

/**
 * 头像图片处理：本地图片 → 中心裁剪正方形 → 缩放到固定尺寸 → dataURL。
 * 输出体積小（128px PNG 通常 < 30KB），可直接存入身份文件。
 */
export async function fileToAvatarDataUrl(file: File, size = 128): Promise<string> {
  if (!file.type.startsWith('image/')) {
    throw new Error('请选择图片文件');
  }
  const bitmap = await createImageBitmap(file);
  try {
    const side = Math.min(bitmap.width, bitmap.height);
    const sx = Math.floor((bitmap.width - side) / 2);
    const sy = Math.floor((bitmap.height - side) / 2);

    const canvas = document.createElement('canvas');
    canvas.width = size;
    canvas.height = size;
    const context = canvas.getContext('2d');
    if (!context) {
      throw new Error('当前环境不支持图片处理');
    }
    context.drawImage(bitmap, sx, sy, side, side, 0, 0, size, size);
    return canvas.toDataURL('image/png');
  } finally {
    bitmap.close();
  }
}

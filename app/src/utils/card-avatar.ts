/**
 * 名片码中央头像合成（device-trust-and-biometric 落地路线 §6「名片码嵌头像」）。
 *
 * 职责：把用户头像（或自动头像回退）合成进已生成的二维码 dataURL，输出新 dataURL。
 * 与 UserAvatar 自动头像同一套占位逻辑（昵称首字 + hashGradient 哈希色块），
 * 保证无头像用户的名片码视觉不中断。白色圆角衬底略大于头像，确保二维码
 * 边缘定位模块完整不被头像遮挡。
 */
import { hashGradient } from './palette';

/** 中央头像占码边长比例（设计定稿 §6：约 22%） */
const AVATAR_RATIO = 0.22;
/** 白色圆角衬底比头像大出的像素（四向各扩），保证边缘定位模块完整 */
const BACKDROP_PAD = 3;
/** 自动头像：昵称首字（取与 UserAvatar 相同的规范化规则） */
function autoInitial(nickname: string): string {
  const displayName = nickname.trim() || '未命名用户';
  const first = [...displayName][0] ?? '用';
  return /^[a-z]$/i.test(first) ? first.toUpperCase() : first;
}

/**
 * 在二维码 dataURL 上合成中央头像，返回新 PNG dataURL。
 * @param qrDataUrl  二维码 PNG dataURL（须为 errorCorrectionLevel: 'H' 生成，见调用方）
 * @param avatar     用户头像 dataURL；为空时回退自动头像（昵称首字 + 哈希渐变）
 * @param nickname   昵称（自动头像取首字用）
 * @param seed       自动头像配色 seed（用 rootId，同一身份恒同色）
 */
export function composeCardAvatar(
  qrDataUrl: string,
  avatar: string,
  nickname: string,
  seed: string
): Promise<string> {
  return new Promise((resolve, reject) => {
    const qr = new Image();
    qr.onload = () => {
      try {
        const size = qr.naturalWidth;
        const canvas = document.createElement('canvas');
        canvas.width = size;
        canvas.height = size;
        const ctx = canvas.getContext('2d');
        if (!ctx) {
          reject(new Error('当前环境不支持图片合成'));
          return;
        }
        ctx.drawImage(qr, 0, 0);

        const avatarSide = Math.round(size * AVATAR_RATIO);
        const backSide = avatarSide + BACKDROP_PAD * 2;
        const backX = (size - backSide) / 2;
        const backY = (size - backSide) / 2;
        const avatarX = (size - avatarSide) / 2;
        const avatarY = (size - avatarSide) / 2;

        // 白底圆角衬底（略大于头像），先画衬底再叠头像
        roundRectPath(ctx, backX, backY, backSide, backSide, Math.round(avatarSide * 0.2));
        ctx.fillStyle = '#ffffff';
        ctx.fill();

        if (avatar) {
          const img = new Image();
          img.onload = () => {
            try {
              clipRoundAvatar(ctx, avatarX, avatarY, avatarSide, Math.round(avatarSide * 0.2));
              ctx.drawImage(img, avatarX, avatarY, avatarSide, avatarSide);
              resolve(canvas.toDataURL('image/png'));
            } catch (e) {
              reject(e);
            }
          };
          img.onerror = () => reject(new Error('头像图片加载失败'));
          img.src = avatar;
          return;
        }

        // 无头像：自动头像（昵称首字 + 哈希渐变圆角块），与 UserAvatar 同占位逻辑
        clipRoundAvatar(ctx, avatarX, avatarY, avatarSide, Math.round(avatarSide * 0.2));
        ctx.fillStyle = hashGradient(seed || 'spark');
        ctx.fill();
        ctx.fillStyle = '#ffffff';
        ctx.font = `600 ${Math.round(avatarSide * 0.44)}px sans-serif`;
        ctx.textAlign = 'center';
        ctx.textBaseline = 'middle';
        ctx.fillText(autoInitial(nickname), size / 2, size / 2 + avatarSide * 0.02);
        resolve(canvas.toDataURL('image/png'));
      } catch (e) {
        reject(e);
      }
    };
    qr.onerror = () => reject(new Error('二维码图片加载失败'));
    qr.src = qrDataUrl;
  });
}

/** 画圆角矩形路径 */
function roundRectPath(
  ctx: CanvasRenderingContext2D,
  x: number,
  y: number,
  width: number,
  height: number,
  radius: number
): void {
  ctx.beginPath();
  ctx.moveTo(x + radius, y);
  ctx.arcTo(x + width, y, x + width, y + height, radius);
  ctx.arcTo(x + width, y + height, x, y + height, radius);
  ctx.arcTo(x, y + height, x, y, radius);
  ctx.arcTo(x, y, x + width, y, radius);
  ctx.closePath();
}

/** 圆角裁剪 + 建立圆角路径（供随后填充/画图，且保证头像裁成圆角） */
function clipRoundAvatar(
  ctx: CanvasRenderingContext2D,
  x: number,
  y: number,
  side: number,
  radius: number
): void {
  roundRectPath(ctx, x, y, side, side, radius);
  ctx.clip();
  ctx.beginPath();
  roundRectPath(ctx, x, y, side, side, radius);
}

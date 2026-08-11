// composeCardAvatar 合成单测（device-trust-and-biometric §6 名片码嵌头像）。
// jsdom 无 canvas 2D 与真实 Image 解码，故桩掉：
// - HTMLCanvasElement.getContext → 返回记录几何/绘制的 fake 2D context
// - 全局 Image → 设置 src 即触发 onload（qr 带 naturalWidth，avatar 无需尺寸）
// 断言聚焦：H 级容错参数透传、头像占比/衬底几何计算、无头像回退自动头像路径、
// 以及各加载失败/环境不支持的错误路径。
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { composeCardAvatar } from '../../utils/card-avatar';

type CallRecord = { kind: string; args: unknown[]; fillStyle?: string };
/** fake 2D context：记录绘制调用与属性，供断言几何计算 */
function makeFakeCtx() {
  const calls: CallRecord[] = [];
  const ctx: Record<string, unknown> = {
    calls,
    fillStyle: '',
    font: '',
    textAlign: '',
    textBaseline: '',
    beginPath: () => calls.push({ kind: 'beginPath', args: [] }),
    moveTo: (...a: unknown[]) => calls.push({ kind: 'moveTo', args: a }),
    arcTo: (...a: unknown[]) => calls.push({ kind: 'arcTo', args: a }),
    closePath: () => calls.push({ kind: 'closePath', args: [] }),
    clip: () => calls.push({ kind: 'clip', args: [] }),
    fill: () => calls.push({ kind: 'fill', args: [], fillStyle: String(ctx.fillStyle) }),
    fillText: (...a: unknown[]) => calls.push({ kind: 'fillText', args: a }),
    drawImage: (...a: unknown[]) => calls.push({ kind: 'drawImage', args: a })
  };
  return { ctx, calls };
}

/** 装配全局桩：返回可注入的 fake ctx 与断言用记录 */
function installStubs() {
  // 记录 Image 实例，触发 onload/onerror 用
  const images: Array<Record<string, unknown>> = [];
  const ImageStub = class {
    naturalWidth = 0;
    naturalHeight = 0;
    onload: (() => void) | null = null;
    onerror: (() => void) | null = null;
    src = '';
    constructor() {
      // class 实例无 index signature，需断言成 Record<string, unknown> 入列（实例属性已在其上）
      images.push(this as unknown as Record<string, unknown>);
    }
  } as unknown as typeof Image;
  const prevImage = global.Image;
  global.Image = ImageStub;

  const { ctx, calls } = makeFakeCtx();
  const toDataURL = vi.fn(() => 'data:image/png;base64,OUTPUT');
  const prevGetContext = HTMLCanvasElement.prototype.getContext;
  HTMLCanvasElement.prototype.getContext = vi.fn(() => ctx) as unknown as typeof HTMLCanvasElement.prototype.getContext;
  HTMLCanvasElement.prototype.toDataURL = toDataURL as unknown as typeof HTMLCanvasElement.prototype.toDataURL;

  const cleanup = () => {
    global.Image = prevImage;
    HTMLCanvasElement.prototype.getContext = prevGetContext;
    // @ts-expect-error toDataURL 非标准桩，清理时复原为未定义
    delete HTMLCanvasElement.prototype.toDataURL;
  };
  return { images, ctx, calls, toDataURL, cleanup };
}

/** 触发指定 Image 实例的 onload（src 赋值后由被测逻辑设置 onload） */
function fireImageLoad(image: Record<string, unknown>) {
  (image as { onload?: () => void }).onload?.();
}

function fireImageError(image: Record<string, unknown>) {
  (image as { onerror?: () => void }).onerror?.();
}

/** 让某个 Image 实例表现出"已解码"：赋值 naturalWidth 后触发 onload */
function loadImageAs(image: Record<string, unknown>, naturalWidth: number) {
  image.naturalWidth = naturalWidth;
  image.naturalHeight = naturalWidth;
  fireImageLoad(image);
}

describe('composeCardAvatar（名片码嵌头像合成）', () => {
  let stubs: ReturnType<typeof installStubs>;

  beforeEach(() => {
    stubs = installStubs();
  });

  afterEach(() => {
    stubs.cleanup();
  });

  const QR = 'data:image/png;base64,QR';
  const AVATAR = 'data:image/png;base64,AVATAR';

  it('合成头像：占比 22%、衬底四向各扩 3px，几何居中对齐', async () => {
    const size = 320;
    const p = composeCardAvatar(QR, AVATAR, 'alice', 'root-1');

    // 首个 Image 是二维码
    loadImageAs(stubs.images[0], size);
    // 第二个 Image 是头像（触发 onload 即进入 drawImage 分支）
    fireImageLoad(stubs.images[1]);

    const out = await p;
    expect(out).toBe('data:image/png;base64,OUTPUT');

    const avatarSide = Math.round(size * 0.22);
    const backSide = avatarSide + 3 * 2;
    const backX = (size - backSide) / 2;
    const backY = (size - backSide) / 2;
    const avatarX = (size - avatarSide) / 2;
    const avatarY = (size - avatarSide) / 2;
    const radius = Math.round(avatarSide * 0.2);

    // 二维码整幅铺底（drawImage 首参 = 二维码 Image 实例）
    const qrImage = stubs.images[0];
    const bgDraw = stubs.calls.find((c) => c.kind === 'drawImage' && c.args[0] === qrImage);
    expect(bgDraw).toBeTruthy();

    // 衬底：白底圆角路径 start = (backX, backY) + 边长 backSide
    const moveTos = stubs.calls.filter((c) => c.kind === 'moveTo');
    // 衬底 1 次 + 头像裁剪区 2 次（clipRoundAvatar 内 roundRectPath 调两遍：先 clip 后重建路径）
    expect(moveTos).toHaveLength(3);
    // roundRectPath 起笔在 (x+radius, y)：衬底左上角即 backX+radius
    expect(moveTos[0].args[0]).toBeCloseTo(backX + radius);
    expect(moveTos[0].args[1]).toBeCloseTo(backY);
    // 白底衬底 fill：fillStyle=#ffffff
    expect(stubs.calls.find((c) => c.kind === 'fill')!.fillStyle).toBe('#ffffff');

    // 头像 drawImage：以 (avatarX, avatarY, avatarSide, avatarSide) 贴图（首参非二维码 Image）
    const avatarDraw = stubs.calls.find((c) => c.kind === 'drawImage' && c.args[0] !== qrImage)!;
    expect(avatarDraw.args).toEqual([expect.anything(), avatarX, avatarY, avatarSide, avatarSide]);

    // 圆角半径=头像边长的 20%（arcTo 第 5 参=radius，衬底与头像圆角一致）
    expect(stubs.calls.some((c) => c.kind === 'arcTo' && c.args[4] === radius)).toBe(true);
  });

  it('无头像回退自动头像：昵称首字 + 哈希渐变衬底（英文首字大写）', async () => {
    const size = 200;
    stubs.cleanup();
    stubs = installStubs();
    const p = composeCardAvatar(QR, '', 'alice', 'root-1');

    loadImageAs(stubs.images[0], size);
    // 无头像：不创建第二个 Image，直接走 fillText 分支
    const out = await p;
    expect(out).toBe('data:image/png;base64,OUTPUT');

    const avatarSide = Math.round(size * 0.22);
    // 自动头像 fill = hashGradient(seed)；白底衬底在先
    const gradientFill = stubs.calls.find((c) => c.kind === 'fill' && c.fillStyle?.includes('linear-gradient'));
    expect(gradientFill).toBeTruthy();
    // 昵称首字大写 'A'，水平垂直居中（textAlign=center / textBaseline=middle）
    const fillText = stubs.calls.find((c) => c.kind === 'fillText')!;
    expect(fillText.args[0]).toBe('A');
    expect(stubs.ctx.textAlign).toBe('center');
    expect(stubs.ctx.textBaseline).toBe('middle');
    expect(String(stubs.ctx.font)).toContain(String(Math.round(avatarSide * 0.44)));
    // 只创建了二维码这一个 Image
    expect(stubs.images).toHaveLength(1);
  });

  it('无头像回退：中文昵称取首字，空昵称回退「未」', async () => {
    const size = 200;
    stubs.cleanup();
    stubs = installStubs();
    const p = composeCardAvatar(QR, '', '测试用户', 'root-1');
    loadImageAs(stubs.images[0], size);
    await p;
    const zhFill = stubs.calls.find((c) => c.kind === 'fillText')!;
    expect(zhFill.args[0]).toBe('测');

    stubs.cleanup();
    stubs = installStubs();
    const p2 = composeCardAvatar(QR, '', '   ', 'root-1');
    loadImageAs(stubs.images[0], size);
    await p2;
    const emptyFill = stubs.calls.find((c) => c.kind === 'fillText')!;
    expect(emptyFill.args[0]).toBe('未');
  });

  it('环境不支持 2D 上下文 → reject 当前环境不支持图片合成', async () => {
    const prevGetContext = HTMLCanvasElement.prototype.getContext;
    HTMLCanvasElement.prototype.getContext = vi.fn(() => null) as unknown as typeof HTMLCanvasElement.prototype.getContext;
    try {
      const p = composeCardAvatar(QR, AVATAR, 'alice', 'root-1');
      loadImageAs(stubs.images[0], 320);
      await expect(p).rejects.toThrow('当前环境不支持图片合成');
    } finally {
      HTMLCanvasElement.prototype.getContext = prevGetContext;
    }
  });

  it('二维码图片加载失败 → reject 二维码图片加载失败', async () => {
    const p = composeCardAvatar(QR, AVATAR, 'alice', 'root-1');
    fireImageError(stubs.images[0]);
    await expect(p).rejects.toThrow('二维码图片加载失败');
  });

  it('头像图片加载失败 → reject 头像图片加载失败', async () => {
    const p = composeCardAvatar(QR, AVATAR, 'alice', 'root-1');
    loadImageAs(stubs.images[0], 320);
    fireImageError(stubs.images[1]);
    await expect(p).rejects.toThrow('头像图片加载失败');
  });

  it('H 级容错：seed 为空时自动头像退用默认渐变种子', async () => {
    const size = 200;
    stubs.cleanup();
    stubs = installStubs();
    const p = composeCardAvatar(QR, '', 'alice', '');
    loadImageAs(stubs.images[0], size);
    await p;
    // seed='' → hashGradient('spark') 默认值，仍是合法渐变
    const gradientFill = stubs.calls.find((c) => c.kind === 'fill' && c.fillStyle?.includes('linear-gradient'));
    expect(gradientFill).toBeTruthy();
  });
});

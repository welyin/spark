// CardInput 摄像头扫码（ui-contacts §4.1.1 名片识别）取流/识别/停流回归：
// 覆盖四条路径：
// 1. 成功识别有效名片 → parseCard 通过 → 填入 decoded → emit update:modelValue → 关闭取景
// 2. 识别出内容但非有效名片（parseCard 失败）→ 保持取景继续扫
// 3. 权限拒绝/无摄像头（getUserMedia 抛错）→ 降级提示改用上传图片
// 4. 组件卸载（onBeforeUnmount）→ 停全部 track（防 track 泄漏）
// 扫码入口仅移动端（isMobileLayout）展示，故 mock ui-layout 令其为 true。
// jsdom 无真实摄像头与 canvas 2D，故 mock：getUserMedia / decodeQrTextFromCanvas /
// canvas.getContext（同 add-account.render.test.ts 的做法）。
import { afterEach, beforeEach, describe, expect, it, vi, type Mock } from 'vitest';
import { createApp, h, nextTick } from 'vue';
import ElementPlus from 'element-plus';
import CardInput from '../../components/common/CardInput.vue';
import { decodeQrTextFromCanvas } from '../../utils/qr-decode';
import { decodeCardImage } from '../../utils/card';

// 移动端布局：令扫码入口可见（ui-contacts §4.1.1 仅移动端提供）
vi.mock('../../stores/ui-layout', () => ({
  isMobileLayout: { value: true }
}));

// 摄像头逐帧解码与图片解码均 mock（真实 jsQR 需 canvas 2D，jsdom 无）
vi.mock('../../utils/qr-decode', () => ({
  decodeQrTextFromCanvas: vi.fn(() => ''),
  decodeQrTextFromFile: vi.fn().mockResolvedValue('')
}));

// parseCard 保留真实实现做名片校验；仅 mock 图片解码入口
vi.mock('../../utils/card', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../../utils/card')>();
  return { ...actual, decodeCardImage: vi.fn().mockResolvedValue('') };
});

/** 有效名片载荷（spark-card JSON，parseCard 可识别） */
const VALID_CARD = '{"type":"spark-card","rootId":"' + 'a'.repeat(64) + '"}';

type Track = { stop: ReturnType<typeof vi.fn> };
type FakeStream = { getTracks: () => Track[] };

function makeStream(trackCount: number): { stream: FakeStream; tracks: Track[] } {
  const tracks: Track[] = Array.from({ length: trackCount }, () => ({ stop: vi.fn() }));
  const stream = { getTracks: () => tracks };
  return { stream, tracks };
}

let rafCallbacks: FrameRequestCallback[] = [];
let rafId = 0;

function installRaf(): void {
  rafCallbacks = [];
  rafId = 0;
  vi.spyOn(window, 'requestAnimationFrame').mockImplementation((cb) => {
    rafCallbacks.push(cb);
    return ++rafId;
  });
  vi.spyOn(window, 'cancelAnimationFrame').mockImplementation(() => {
    rafCallbacks = [];
  });
}

let getUserMedia: Mock;
let restoreMedia: (() => void) | null = null;

function installMedia(getUserMediaImpl: Mock): void {
  getUserMedia = getUserMediaImpl;
  const desc = Object.getOwnPropertyDescriptor(navigator, 'mediaDevices');
  Object.defineProperty(navigator, 'mediaDevices', {
    configurable: true,
    value: { getUserMedia }
  });
  restoreMedia = () => {
    if (desc?.get) {
      Object.defineProperty(navigator, 'mediaDevices', desc);
    } else {
      // @ts-expect-error jsdom 默认无 mediaDevices，清理时删掉
      delete navigator.mediaDevices;
    }
  };
}

async function flush(): Promise<void> {
  await nextTick();
  await new Promise((resolve) => setTimeout(resolve, 0));
  await nextTick();
}

function mount(): { host: HTMLElement; app: ReturnType<typeof createApp> } {
  const host = document.createElement('div');
  document.body.appendChild(host);
  const app = createApp({ render: () => h(CardInput) });
  app.use(ElementPlus);
  app.mount(host);
  return { host, app };
}

beforeEach(() => {
  (decodeQrTextFromCanvas as Mock).mockReturnValue('');
  (decodeCardImage as Mock).mockResolvedValue('');
  HTMLCanvasElement.prototype.getContext = vi.fn(() => ({ drawImage: () => {} })) as unknown as typeof HTMLCanvasElement.prototype.getContext;
});

afterEach(() => {
  restoreMedia?.();
  restoreMedia = null;
  vi.restoreAllMocks();
  document.body.innerHTML = '';
});

describe('CardInput 摄像头扫码（移动端入口）', () => {
  it('移动端展示扫码按钮，桌面端不展示', () => {
    const { host } = mount();
    expect(host.querySelector('.card-scan-btn')).toBeTruthy();
    host.remove();
  });

  it('成功识别有效名片：parseCard 通过 → 填入 decoded → 关闭取景', async () => {
    installRaf();
    const { stream, tracks } = makeStream(2);
    installMedia(vi.fn().mockResolvedValue(stream));

    const { host } = mount();
    (host.querySelector('.card-scan-btn') as HTMLButtonElement).click();
    await flush();

    const video = host.querySelector('.camera-video') as HTMLVideoElement;
    expect(video).toBeTruthy();
    expect(video.srcObject).toBe(stream as unknown as MediaStream);

    // 模拟视频数据就绪 → 启动解码循环；命中有效名片
    Object.defineProperty(video, 'readyState', { configurable: true, value: 4 });
    Object.defineProperty(video, 'videoWidth', { configurable: true, value: 320 });
    Object.defineProperty(video, 'videoHeight', { configurable: true, value: 320 });
    (decodeQrTextFromCanvas as Mock).mockReturnValue(VALID_CARD);
    (video.onloadeddata as unknown as (() => void) | null)?.();
    await flush();

    // 取景关闭（全部 track stop）、video 移除
    expect(tracks[0].stop).toHaveBeenCalledTimes(1);
    expect(tracks[1].stop).toHaveBeenCalledTimes(1);
    expect(host.querySelector('.camera-video')).toBeNull();
    // 有效名片已填入上传区（ok 态）
    expect(host.querySelector('.card-upload.ok')).toBeTruthy();

    host.remove();
  });

  it('识别出内容但非有效名片：parseCard 失败 → 保持取景继续扫', async () => {
    installRaf();
    const { stream, tracks } = makeStream(2);
    installMedia(vi.fn().mockResolvedValue(stream));

    const { host } = mount();
    (host.querySelector('.card-scan-btn') as HTMLButtonElement).click();
    await flush();

    const video = host.querySelector('.camera-video') as HTMLVideoElement;
    Object.defineProperty(video, 'readyState', { configurable: true, value: 4 });
    Object.defineProperty(video, 'videoWidth', { configurable: true, value: 320 });
    Object.defineProperty(video, 'videoHeight', { configurable: true, value: 320 });
    // 解码命中但内容不是有效名片
    (decodeQrTextFromCanvas as Mock).mockReturnValue('not-a-card-text');
    (video.onloadeddata as unknown as (() => void) | null)?.();
    await flush();

    // 未停流（track 未被 stop），取景保持
    expect(tracks[0].stop).not.toHaveBeenCalled();
    expect(host.querySelector('.camera-video')).toBeTruthy();
    // 上传区未进入 ok 态
    expect(host.querySelector('.card-upload.ok')).toBeNull();

    host.remove();
  });

  it('权限拒绝/无摄像头：getUserMedia 抛错 → 降级提示，不进入取景', async () => {
    installRaf();
    installMedia(vi.fn().mockRejectedValue(new Error('Permission denied')));

    const { host } = mount();
    (host.querySelector('.card-scan-btn') as HTMLButtonElement).click();
    await flush();

    // 未进入取景
    expect(host.querySelector('.camera-video')).toBeNull();

    host.remove();
  });

  it('组件卸载：停全部 track（防摄像头指示灯常亮）', async () => {
    installRaf();
    const { stream, tracks } = makeStream(2);
    installMedia(vi.fn().mockResolvedValue(stream));

    const { host, app } = mount();
    (host.querySelector('.card-scan-btn') as HTMLButtonElement).click();
    await flush();

    const video = host.querySelector('.camera-video') as HTMLVideoElement;
    expect(video.srcObject).toBe(stream as unknown as MediaStream);

    // 卸载 → useQrCameraScan onBeforeUnmount(stop) 停全部 track
    app.unmount();
    expect(tracks[0].stop).toHaveBeenCalledTimes(1);
    expect(tracks[1].stop).toHaveBeenCalledTimes(1);

    host.remove();
  });
});

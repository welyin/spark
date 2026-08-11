// AddAccountPage 摄像头扫码（device-trust-and-biometric §6 添加页）取流/停流回归：
// 覆盖三条路径：
// 1. 成功取流 → video 挂载 srcObject → 解码命中 → 停流进入「输入原密码」步
// 2. 手动点击「关闭摄像头」→ 停全部 track
// 3. 组件卸载（onBeforeUnmount）→ 停全部 track
// jsdom 无真实摄像头与 canvas 2D，故 mock：
// - navigator.mediaDevices.getUserMedia → 返回带可断言 track 的 fake stream
// - decodeQrTextFromCanvas → 可控返回值（命中时给备份码，否则给 ''）
// - canvas.getContext → 空壳对象（decode 已被 mock，仅需非 null 以进入解码分支）
// 断言聚焦：video 确实挂载了 srcObject（修复「模板 ref 异步 patch」bug）、解码命中后停流、
// 手动/卸载都停全部 track。
import { afterEach, beforeEach, describe, expect, it, vi, type Mock } from 'vitest';
import { createApp, h, nextTick } from 'vue';
import ElementPlus from 'element-plus';
import { decodeQrTextFromCanvas, decodeQrTextFromFile } from '../../utils/qr-decode';
import AddAccountPage from '../../pages/auth/AddAccountPage.vue';

vi.mock('../../utils/qr-decode', () => ({
  decodeQrTextFromCanvas: vi.fn(() => ''),
  decodeQrTextFromFile: vi.fn().mockResolvedValue('')
}));

type Track = { stop: ReturnType<typeof vi.fn> };
type FakeStream = { getTracks: () => Track[] };

function makeStream(trackCount: number): { stream: FakeStream; tracks: Track[] } {
  const tracks: Track[] = Array.from({ length: trackCount }, () => ({ stop: vi.fn() }));
  const stream = { getTracks: () => tracks };
  return { stream, tracks };
}

let getUserMedia: Mock;
let rafCallbacks: FrameRequestCallback[] = [];
let rafId = 0;

function installMedia(getUserMediaImpl: Mock): () => void {
  getUserMedia = getUserMediaImpl;
  const desc = Object.getOwnPropertyDescriptor(navigator, 'mediaDevices');
  Object.defineProperty(navigator, 'mediaDevices', {
    configurable: true,
    value: { getUserMedia }
  });
  return () => {
    if (desc?.get) {
      Object.defineProperty(navigator, 'mediaDevices', desc);
    } else {
      // @ts-expect-error jsdom 默认无 mediaDevices，清理时删掉
      delete navigator.mediaDevices;
    }
  };
}

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

async function flush(): Promise<void> {
  await nextTick();
  await new Promise((resolve) => setTimeout(resolve, 0));
  await nextTick();
}

function mount(): { host: HTMLElement; app: ReturnType<typeof createApp> } {
  const host = document.createElement('div');
  document.body.appendChild(host);
  const app = createApp({ render: () => h(AddAccountPage) });
  app.use(ElementPlus);
  app.mount(host);
  return { host, app };
}

let restoreMedia: (() => void) | null = null;

beforeEach(() => {
  (decodeQrTextFromCanvas as Mock).mockReturnValue('');
  (decodeQrTextFromFile as Mock).mockResolvedValue('');
  HTMLCanvasElement.prototype.getContext = vi.fn(() => ({ drawImage: () => {} })) as unknown as typeof HTMLCanvasElement.prototype.getContext;
});

afterEach(() => {
  restoreMedia?.();
  restoreMedia = null;
  vi.restoreAllMocks();
  document.body.innerHTML = '';
});

describe('AddAccountPage 摄像头扫码', () => {
  it('成功取流：video 挂载 srcObject → 解码命中 → 停流进入输入密码步', async () => {
    installRaf();
    const { stream, tracks } = makeStream(2);
    restoreMedia = installMedia(vi.fn().mockResolvedValue(stream));

    const { host } = mount();
    (host.querySelector('.qr-camera-btn') as HTMLButtonElement).click();
    await flush();

    // 取流成功 → cameraActive=true 渲染出 <video>，srcObject 已挂载（修复 ref 异步 bug）
    const video = host.querySelector('.camera-video') as HTMLVideoElement;
    expect(video).toBeTruthy();
    expect(video.srcObject).toBe(stream as unknown as MediaStream);

    // 模拟视频数据就绪（触发 onloadeddata → 启动解码循环）
    Object.defineProperty(video, 'readyState', { configurable: true, value: 4 });
    Object.defineProperty(video, 'videoWidth', { configurable: true, value: 320 });
    Object.defineProperty(video, 'videoHeight', { configurable: true, value: 320 });
    (decodeQrTextFromCanvas as Mock).mockReturnValue('backup-payload');
    (video.onloadeddata as unknown as (() => void) | null)?.();
    await flush();

    // 解码命中 → 停流（全部 track stop）
    expect(tracks[0].stop).toHaveBeenCalledTimes(1);
    expect(tracks[1].stop).toHaveBeenCalledTimes(1);
    // 进入「输入原密码」步
    expect(host.textContent).toContain('已识别备份二维码');

    host.remove();
  });

  it('手动点击「关闭摄像头」：停全部 track 并释放 video', async () => {
    installRaf();
    const { stream, tracks } = makeStream(2);
    restoreMedia = installMedia(vi.fn().mockResolvedValue(stream));

    const { host } = mount();
    (host.querySelector('.qr-camera-btn') as HTMLButtonElement).click();
    await flush();

    const video = host.querySelector('.camera-video') as HTMLVideoElement;
    expect(video.srcObject).toBe(stream as unknown as MediaStream);

    // 点击关闭
    (host.querySelector('.qr-camera-close') as HTMLButtonElement).click();
    await flush();

    expect(tracks[0].stop).toHaveBeenCalledTimes(1);
    expect(tracks[1].stop).toHaveBeenCalledTimes(1);
    // 回到未取流态：video 移除、srcObject 释放
    expect(host.querySelector('.camera-video')).toBeNull();
    expect(video.srcObject).toBeNull();

    host.remove();
  });

  it('组件卸载（onBeforeUnmount）：停全部 track', async () => {
    installRaf();
    const { stream, tracks } = makeStream(2);
    restoreMedia = installMedia(vi.fn().mockResolvedValue(stream));

    const { host, app } = mount();
    (host.querySelector('.qr-camera-btn') as HTMLButtonElement).click();
    await flush();

    const video = host.querySelector('.camera-video') as HTMLVideoElement;
    expect(video.srcObject).toBe(stream as unknown as MediaStream);

    // 卸载组件 → onBeforeUnmount(stopCamera) 停全部 track
    app.unmount();

    expect(tracks[0].stop).toHaveBeenCalledTimes(1);
    expect(tracks[1].stop).toHaveBeenCalledTimes(1);

    host.remove();
  });
});

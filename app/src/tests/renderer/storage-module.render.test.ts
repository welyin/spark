// StorageModule（存储与副本，A3）渲染回归：
// - 健康度展示：设备数/K/「你当前只有 N 份副本」头部（有 blob 取最差副本水位，
//   无 blob 按核心数据口径=设备数）；
// - 副本不足 K 时挂 warning（只提醒不处置文案），达标时不挂；
// - 配额水位展示；保存（GB→字节）与恢复默认（null）调用正确；
// - 设备 ≤3 台退化全量口径在 K 行如实表达。
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createApp, h } from 'vue';
import ElementPlus, { ElMessage } from 'element-plus';
import StorageModule from '../../components/mine/StorageModule.vue';
import type { BlobHealthDto } from '../../api/types';

const GIB = 1024 * 1024 * 1024;

function healthOf(patch: Partial<BlobHealthDto> = {}): BlobHealthDto {
  return {
    deviceCount: 3,
    kTarget: 3,
    totalBlobs: 12,
    totalBytes: 2 * GIB,
    underKBlobs: 0,
    minFullReplicas: 3,
    quota: { quotaBytes: 10 * GIB, usedBytes: 2 * GIB, overBytes: 0 },
    ...patch
  };
}

function mount(): HTMLElement {
  const host = document.createElement('div');
  document.body.appendChild(host);
  const app = createApp({ render: () => h(StorageModule) });
  app.use(ElementPlus);
  app.mount(host);
  return host;
}

async function flush(): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, 0));
  await new Promise((resolve) => setTimeout(resolve, 0));
}

describe('StorageModule 渲染', () => {
  let health: ReturnType<typeof vi.fn>;
  let getQuota: ReturnType<typeof vi.fn>;
  let setQuota: ReturnType<typeof vi.fn>;
  let host: HTMLElement;

  function setupApi(h: BlobHealthDto) {
    health = vi.fn().mockResolvedValue(h);
    getQuota = vi.fn().mockResolvedValue(h.quota.quotaBytes);
    setQuota = vi.fn().mockResolvedValue(undefined);
    (window as any).electronAPI = { blob: { health, getQuota, setQuota } };
  }

  beforeEach(() => {
    vi.spyOn(ElMessage, 'success').mockImplementation(() => ({}) as never);
  });

  afterEach(() => {
    host?.remove();
    vi.restoreAllMocks();
  });

  it('健康度摘要：头部副本数取最差副本水位，K 行含退化全量口径', async () => {
    setupApi(healthOf({ minFullReplicas: 2, underKBlobs: 1 }));
    host = mount();
    await flush();
    expect(host.textContent).toContain('你当前只有 2 份副本');
    expect(host.textContent).toContain('设备：3 台（核心数据全量 3 份副本）');
    expect(host.textContent).toContain('K = 3（设备不超过 3 台时退化为全量）');
    expect(host.textContent).toContain('1 个副本不足 K');
    // 副本不足挂 warning（只提醒不处置）
    expect(host.querySelector('.el-alert--warning')).toBeTruthy();
    expect(host.textContent).toContain('只提醒不处置');
  });

  it('无 blob 时头部按核心数据口径（= 设备数），副本达标不挂警告', async () => {
    setupApi(healthOf({ deviceCount: 1, kTarget: 1, totalBlobs: 0, totalBytes: 0, minFullReplicas: null }));
    host = mount();
    await flush();
    expect(host.textContent).toContain('你当前只有 1 份副本');
    expect(host.textContent).toContain('K = 1');
    expect(host.textContent).toContain('副本均达标');
    expect(host.querySelector('.el-alert--warning')).toBeNull();
  });

  it('配额：保存按 GB 换算字节调用，恢复默认传 null', async () => {
    setupApi(healthOf());
    host = mount();
    await flush();
    expect(host.textContent).toContain('2.0 GB / 10.0 GB');
    // 保存（输入框初值 = 配额 GB 取整 = 10）
    const buttons = Array.from(host.querySelectorAll('.storage-quota-form button')) as HTMLButtonElement[];
    const saveBtn = buttons.find((b) => b.textContent?.includes('保存'))!;
    saveBtn.click();
    await flush();
    expect(setQuota).toHaveBeenCalledWith(10 * GIB);
    // 恢复默认
    const resetBtn = buttons.find((b) => b.textContent?.includes('恢复默认'))!;
    resetBtn.click();
    await flush();
    expect(setQuota).toHaveBeenCalledWith(null);
  });

  it('超配额水位如实展示超出量', async () => {
    setupApi(healthOf({ quota: { quotaBytes: GIB, usedBytes: 2 * GIB, overBytes: GIB } }));
    host = mount();
    await flush();
    expect(host.textContent).toContain('已超出 1.0 GB');
    expect(host.textContent).toContain('绝不删除最后副本');
  });
});

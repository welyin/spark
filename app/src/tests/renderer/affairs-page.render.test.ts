// AffairsPage 渲染回归（A6/A7/A8/G8）：
// - A6：可组合筛选收成顶部 chips（状态/关系/标签），选中生效并收敛列表；「等我操作」分组标题始终在高亮位；
// - A7：进行中且公示中的事务卡片带「公示中 · 链上锚定」chip（链上锚定口径）；
// - G8：已关闭执行型事务带「链上决议已生效 · 执行回报 x/y」chip；
// - A8：无承接插件时点卡片弹「未安装承接应用」引导（去应用市场）。
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createApp, h } from 'vue';
import ElementPlus, { ElMessageBox } from 'element-plus';
import AffairsPage from '../../pages/AffairsPage.vue';
import { affairFeed } from '../../stores/affairs/affair-feed';
import { affairTypeRegistry } from '../../stores/affairs/affair-types';
import { currentUser } from '../../stores/current-user';

function genesis(title: string, tags: string[], originator: string) {
  return { title, summary: '', tags, initiator: { identity: originator }, createdAt: 1700000000000 };
}

function setupApi() {
  (window as any).electronAPI = {
    affairs: {
      listFollowed: vi.fn().mockResolvedValue(['open1', 'closed1']),
      readLog: vi.fn().mockImplementation(async (affairId: string) => ({
        affairId,
        genesis: affairId === 'open1' ? genesis('预算议题', ['预算'], 'root-me') : genesis('选举议题', ['选举'], 'root-other'),
        ops: [],
        heads: [],
        followedAt: 1700000000000
      })),
      readResolution: vi.fn().mockImplementation(async (affairId: string) => ({
        affairId,
        resolutions:
          affairId === 'open1'
            ? [{ state: 'pending', anchoredMs: 1700000100000, pubPeriodMs: 86_400_000 }]
            : [{ state: 'effective' }]
      })),
      readExec: vi.fn().mockResolvedValue({
        affairId: 'closed1',
        nowMs: 0,
        exec: { kind: 'exec' },
        states: [
          { state: 'returned', reportOpHash: 'r1', anchoredMs: 1, effectiveMs: 2 },
          { state: 'awaiting-execution', reportOpHash: null, anchoredMs: 1, effectiveMs: 2 }
        ]
      })
    }
  };
}

function mount(): HTMLElement {
  const host = document.createElement('div');
  document.body.appendChild(host);
  const app = createApp({ render: () => h(AffairsPage) });
  app.use(ElementPlus);
  app.mount(host);
  return host;
}

async function flush(): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, 0));
  await new Promise((resolve) => setTimeout(resolve, 0));
  await new Promise((resolve) => setTimeout(resolve, 0));
}

const chipByText = (host: HTMLElement, text: string) =>
  Array.from(host.querySelectorAll('.affair-filter-chip')).find((c) => c.textContent?.includes(text)) as HTMLElement | undefined;

beforeEach(() => {
  affairFeed.value = [];
  affairTypeRegistry.value = new Map();
  currentUser.rootId = 'root-me';
  setupApi();
});

afterEach(() => {
  document.body.innerHTML = '';
  vi.restoreAllMocks();
});

describe('AffairsPage（A6/A7/A8/G8）', () => {
  it('A7/G8：公示中 chip（链上锚定口径）与执行回报 chip 如实展示；等我操作置顶高亮', async () => {
    const host = mount();
    await flush();
    expect(host.textContent).toContain('等我操作（1）');
    expect(host.querySelector('.affair-group-title--action')).not.toBeNull();
    expect(host.textContent).toContain('公示中 · 链上锚定');
    expect(host.textContent).toContain('链上决议已生效 · 执行回报 1/2（链下执行中）');
  });

  it('A6：chips 组合筛选（标签 chip 选中后只剩匹配项；清除恢复）', async () => {
    const host = mount();
    await flush();
    expect(host.textContent).toContain('预算议题');
    expect(host.textContent).toContain('选举议题');

    chipByText(host, '# 选举')!.click();
    await flush();
    expect(host.textContent).not.toContain('预算议题');
    expect(host.textContent).toContain('选举议题');

    chipByText(host, '清除筛选')!.click();
    await flush();
    expect(host.textContent).toContain('预算议题');
  });

  it('A6：我发起 chip 只留 originator=自己的事务', async () => {
    const host = mount();
    await flush();
    chipByText(host, '我发起')!.click();
    await flush();
    expect(host.textContent).toContain('预算议题');
    expect(host.textContent).not.toContain('选举议题');
  });

  it('A8：本机无承接插件时点卡片弹市场引导（确认后打开应用管理市场视图）', async () => {
    const confirmMock = vi.spyOn(ElMessageBox, 'confirm').mockResolvedValue('confirm' as never);
    const host = mount();
    await flush();
    const card = Array.from(host.querySelectorAll('.affair-card')).find((c) =>
      c.textContent?.includes('预算议题')
    ) as HTMLElement;
    card.click();
    await flush();
    expect(confirmMock).toHaveBeenCalledTimes(1);
    const [message, title] = confirmMock.mock.calls[0] as unknown as [string, string];
    expect(title).toBe('未安装承接应用');
    expect(message).toContain('应用市场');
  });
});

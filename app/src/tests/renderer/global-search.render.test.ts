// GlobalSearch（⌘K 全局搜索，G1）渲染回归：
// - 加载态：索引未就绪时显示「索引加载中…」而非误报无结果；
// - 分组标注索引源（本机索引 · …）；事务类目走 affair-feed 元数据（真实数据源）；
// - 空态如实列出已检索索引面与「文件内容索引尚未建立」的未覆盖面。
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createApp, h } from 'vue';
import ElementPlus from 'element-plus';
import GlobalSearch from '../../components/GlobalSearch.vue';
import { affairFeed } from '../../stores/affairs/affair-feed';

function setupApi() {
  (window as any).electronAPI = {
    pluginMarket: { list: vi.fn().mockResolvedValue([]) },
    organization: { listMine: vi.fn().mockResolvedValue([]) },
    affairs: {
      listFollowed: vi.fn().mockResolvedValue(['a1']),
      readLog: vi.fn().mockResolvedValue({
        affairId: 'a1',
        genesis: { title: '预算审议', summary: '', tags: [], initiator: { identity: 'root-x' }, createdAt: 1 },
        ops: [],
        heads: [],
        followedAt: 1
      }),
      readResolution: vi.fn().mockResolvedValue({ affairId: 'a1', resolutions: [] }),
      readExec: vi.fn().mockResolvedValue({ affairId: 'a1', nowMs: 0, exec: null, states: [] })
    }
  };
}

function mount(): HTMLElement {
  const host = document.createElement('div');
  document.body.appendChild(host);
  const app = createApp({ render: () => h(GlobalSearch) });
  app.use(ElementPlus);
  app.mount(host);
  return host;
}

async function flush(): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, 0));
  await new Promise((resolve) => setTimeout(resolve, 0));
}

async function type(host: HTMLElement, text: string): Promise<void> {
  const input = host.querySelector('input')!;
  input.value = text;
  input.dispatchEvent(new Event('input'));
  await flush();
}

beforeEach(() => {
  localStorage.clear();
  affairFeed.value = [];
  setupApi();
});

afterEach(() => {
  document.body.innerHTML = '';
  vi.restoreAllMocks();
});

describe('GlobalSearch（G1）', () => {
  it('事务类目命中 affair-feed 元数据，组标题标注索引源', async () => {
    const host = mount();
    await flush();
    await type(host, '预算');
    const groupTitles = Array.from(host.querySelectorAll('.gs-group-title')).map((el) => el.textContent ?? '');
    const affairTitle = groupTitles.find((t) => t.includes('事务'));
    expect(affairTitle).toBeTruthy();
    expect(affairTitle).toContain('本机索引');
    expect(host.textContent).toContain('预算审议');
  });

  it('空态：如实列出已检索索引面与文件索引缺口（不虚构文件结果）', async () => {
    const host = mount();
    await flush();
    await type(host, '绝不存在的词xyz');
    expect(host.textContent).toContain('无匹配结果');
    expect(host.textContent).toContain('已检索本机索引');
    expect(host.textContent).toContain('文件内容索引尚未建立');
  });

  it('加载态：索引未就绪时提示加载中', async () => {
    // 组织列表挂起 → loading 保持
    (window as any).electronAPI.organization.listMine = vi.fn().mockReturnValue(new Promise(() => {}));
    const host = mount();
    await type(host, '任意');
    expect(host.textContent).toContain('索引加载中…');
  });
});

/**
 * OS 文件拖入（problem X3/X5）回归：
 * - classifyOsDropPoint 三类落点分类（空间导航项 / 桌面空白 / 应用窗口 / 不可承接）；
 * - 松手可承接 → 打开导入预检；确认只派发 spark:file-import-request（实际写入待内核能力）；
 * - 不可承接 / 无文件 → 不开预检。
 */
import { beforeEach, describe, expect, it } from 'vitest';
import {
  FILE_IMPORT_REQUEST_EVENT,
  classifyOsDropPoint,
  closeImportPreflight,
  confirmImportPreflight,
  endOsDrop,
  importPreflight,
  openImportPreflight,
  spaceRefFromAttr,
  updateOsDrop
} from '../../stores/os-file-drop';

function dom(html: string): Element {
  const wrap = document.createElement('div');
  wrap.innerHTML = html;
  document.body.appendChild(wrap);
  return wrap;
}

beforeEach(() => {
  endOsDrop();
  closeImportPreflight();
  document.body.innerHTML = '';
});

describe('os-file-drop · X3 落点分类', () => {
  it('空间导航项（含子元素命中）：落点即该空间', () => {
    const wrap = dom('<button data-drop-space="org-1"><span class="inner">空间A</span></button>');
    const hit = wrap.querySelector('.inner');
    expect(classifyOsDropPoint(hit)).toEqual({ zone: 'space-item', spaceAttr: 'org-1' });
    expect(spaceRefFromAttr('org-1')).toEqual({ type: 'org', orgId: 'org-1' });
    expect(spaceRefFromAttr('personal')).toEqual({ type: 'personal' });
  });

  it('桌面空白 / 应用窗口：落点为当前停留空间（spaceAttr=null 由调用方回退）', () => {
    const desktop = dom('<div data-drop-zone="desktop"><div class="pc-icon"></div></div>');
    expect(classifyOsDropPoint(desktop.querySelector('.pc-icon'))?.zone).toBe('desktop');
    const win = dom('<div class="window-frame"><div class="window-body"></div></div>');
    expect(classifyOsDropPoint(win.querySelector('.window-body'))?.zone).toBe('window');
  });

  it('不可承接区域 / 空命中：null（禁止光标依据）', () => {
    const plain = dom('<div class="rail-item">搜索</div>');
    expect(classifyOsDropPoint(plain.querySelector('.rail-item'))).toBeNull();
    expect(classifyOsDropPoint(null)).toBeNull();
  });
});

describe('os-file-drop · X5 导入预检状态机', () => {
  it('可承接且有文件：打开预检 → 确认派发导入请求事件 → confirmed', () => {
    const seen: Array<{ paths: string[]; target: unknown }> = [];
    const handler = (event: Event) => seen.push((event as CustomEvent).detail);
    window.addEventListener(FILE_IMPORT_REQUEST_EVENT, handler);

    updateOsDrop({ x: 1, y: 1, paths: ['C:/a.txt', 'C:/b.png'], zone: 'desktop', target: { type: 'personal' }, allowed: true });
    expect(openImportPreflight()).toBe(true);
    expect(importPreflight.value?.stage).toBe('precheck');
    expect(importPreflight.value?.paths).toHaveLength(2);

    confirmImportPreflight();
    window.removeEventListener(FILE_IMPORT_REQUEST_EVENT, handler);
    expect(seen).toHaveLength(1);
    expect(seen[0].paths).toEqual(['C:/a.txt', 'C:/b.png']);
    expect(seen[0].target).toEqual({ type: 'personal' });
    expect(importPreflight.value?.stage).toBe('confirmed');

    closeImportPreflight();
    expect(importPreflight.value).toBeNull();
  });

  it('不可承接 / 无文件：不开预检', () => {
    updateOsDrop({ x: 1, y: 1, paths: ['C:/a.txt'], zone: null, target: null, allowed: false });
    expect(openImportPreflight()).toBe(false);
    expect(importPreflight.value).toBeNull();

    updateOsDrop({ x: 1, y: 1, paths: [], zone: 'desktop', target: { type: 'personal' }, allowed: true });
    expect(openImportPreflight()).toBe(false);
  });

  it('confirmed 后重复确认不重复派发', () => {
    let count = 0;
    const handler = () => { count += 1; };
    window.addEventListener(FILE_IMPORT_REQUEST_EVENT, handler);
    updateOsDrop({ x: 1, y: 1, paths: ['C:/a.txt'], zone: 'space-item', target: { type: 'org', orgId: 'o1' }, allowed: true });
    openImportPreflight();
    confirmImportPreflight();
    confirmImportPreflight();
    window.removeEventListener(FILE_IMPORT_REQUEST_EVENT, handler);
    expect(count).toBe(1);
    closeImportPreflight();
  });
});

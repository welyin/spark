/**
 * 跨域拖放管线（problem X1/X2）回归：
 * - 同空间松手不弹确认、直接投递（派发 spark:cross-domain-drop）；
 * - 跨域松手弹确认（pendingCrossDomainDrop 打开，可取消）；
 * - 确认时勾选「不再提示」→ 本次会话同一 源→目标 直接投递；不勾则每次都弹；
 * - 豁免仅在内存（resetCrossDomainDropForTest 即失效 = 关会话语义），无永久豁免。
 */
import { beforeEach, describe, expect, it } from 'vitest';
import {
  CROSS_DOMAIN_DROP_EVENT,
  beginObjectDrag,
  cancelCrossDomainDrop,
  confirmCrossDomainDrop,
  endObjectDrag,
  isDropHover,
  pendingCrossDomainDrop,
  requestDrop,
  resetCrossDomainDropForTest,
  scopeLabelOf,
  spaceKeyOfRef,
  updateObjectDrag,
  type DropObject,
  type SpaceRef
} from '../../stores/cross-domain-drop';

const personal: SpaceRef = { type: 'personal' };
const orgA: SpaceRef = { type: 'org', orgId: 'org-a' };

function icon(source: SpaceRef): DropObject {
  return { kind: 'app-icon', id: 'spark-affairs', label: '事务', source };
}

function listenDrops() {
  const seen: Array<{ from: SpaceRef; to: SpaceRef; id: string }> = [];
  const handler = (event: Event) => {
    const detail = (event as CustomEvent).detail;
    seen.push({ from: detail.from, to: detail.to, id: detail.object.id });
  };
  window.addEventListener(CROSS_DOMAIN_DROP_EVENT, handler);
  return { seen, off: () => window.removeEventListener(CROSS_DOMAIN_DROP_EVENT, handler) };
}

/** 模拟一次完整拖拽：begin → 悬停到 target → 松手 requestDrop */
function dragOnto(source: SpaceRef, target: SpaceRef) {
  beginObjectDrag(icon(source), 10, 10);
  updateObjectDrag(20, 20, target, true);
  return requestDrop(target);
}

beforeEach(() => {
  localStorage.clear();
  resetCrossDomainDropForTest();
});

describe('cross-domain-drop · X1 同空间不弹', () => {
  it('源与目标同域：直接投递、不打开确认框', () => {
    const { seen, off } = listenDrops();
    expect(dragOnto(orgA, orgA)).toBe('committed');
    off();
    expect(pendingCrossDomainDrop.value).toBeNull();
    expect(seen).toHaveLength(1);
    expect(seen[0]).toEqual({ from: orgA, to: orgA, id: 'spark-affairs' });
  });

  it('松手不在任何落点：无动作', () => {
    beginObjectDrag(icon(personal), 10, 10);
    updateObjectDrag(20, 20, null, false);
    expect(requestDrop(personal)).toBe('none');
  });

  it('落点标记不可承接：无动作', () => {
    beginObjectDrag(icon(personal), 10, 10);
    updateObjectDrag(20, 20, orgA, false);
    expect(requestDrop(orgA)).toBe('none');
  });
});

describe('cross-domain-drop · X1/X2 跨域确认', () => {
  it('跨域松手：打开确认框（明示源/目标域），确认后投递', () => {
    const { seen, off } = listenDrops();
    expect(dragOnto(personal, orgA)).toBe('confirming');
    const pending = pendingCrossDomainDrop.value;
    expect(pending?.object.source).toEqual(personal);
    expect(pending?.target).toEqual(orgA);
    expect(seen).toHaveLength(0);
    confirmCrossDomainDrop(false);
    off();
    expect(seen).toHaveLength(1);
    expect(seen[0].to).toEqual(orgA);
    expect(pendingCrossDomainDrop.value).toBeNull();
  });

  it('取消：不投递、不写豁免（下一次仍弹）', () => {
    const { seen, off } = listenDrops();
    dragOnto(personal, orgA);
    cancelCrossDomainDrop();
    expect(seen).toHaveLength(0);
    expect(dragOnto(personal, orgA)).toBe('confirming');
    cancelCrossDomainDrop();
    off();
    expect(seen).toHaveLength(0);
  });

  it('勾「本次会话不再提示」：同一 源→目标 第二次直接投递；反向不受豁免', () => {
    const { seen, off } = listenDrops();
    dragOnto(personal, orgA);
    confirmCrossDomainDrop(true);
    expect(dragOnto(personal, orgA)).toBe('committed');
    expect(dragOnto(orgA, personal)).toBe('confirming');
    cancelCrossDomainDrop();
    off();
    expect(seen).toHaveLength(2);
  });

  it('豁免仅本次会话：重置后（= 关会话）同对仍需确认', () => {
    dragOnto(personal, orgA);
    confirmCrossDomainDrop(true);
    resetCrossDomainDropForTest();
    expect(dragOnto(personal, orgA)).toBe('confirming');
    cancelCrossDomainDrop();
  });
});

describe('cross-domain-drop · X4 悬停态与辅助', () => {
  it('悬停落点可读（rail 高亮数据源）；endObjectDrag 清空会话', () => {
    beginObjectDrag(icon(personal), 10, 10);
    updateObjectDrag(20, 20, orgA, true);
    expect(isDropHover(orgA)).toBe(true);
    expect(isDropHover(personal)).toBe(false);
    endObjectDrag();
    expect(isDropHover(orgA)).toBe(false);
  });

  it('spaceKeyOfRef / scopeLabelOf 口径稳定', () => {
    expect(spaceKeyOfRef(personal)).toBe('personal');
    expect(spaceKeyOfRef(orgA)).toBe('org:org-a');
    expect(scopeLabelOf(personal)).toBe('仅自己可见');
    expect(scopeLabelOf(orgA)).toContain('成员');
  });
});

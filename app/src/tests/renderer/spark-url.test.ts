/**
 * X8 spark:// 深链生成（空间 / 应用 / 对象三粒度）回归：
 * 语法对齐 services/deep-link.ts 头注（spark://space/<域>/app/<插件>[/object/<对象>]）；
 * X9 安全口径：URL 只含寻址字段（orgId / 插件 id / 对象 id），不携带密钥或内容数据。
 */
import { describe, expect, it } from 'vitest';
import { buildSparkUrl } from '../../services/deep-link';

describe('buildSparkUrl · X8 三粒度', () => {
  it('空间粒度：spark://space/<域>', () => {
    expect(buildSparkUrl({ type: 'personal' })).toBe('spark://space/personal');
    expect(buildSparkUrl({ type: 'org', orgId: 'org-1' })).toBe('spark://space/org-1');
  });

  it('应用粒度：…/app/<插件>', () => {
    expect(buildSparkUrl({ type: 'personal' }, 'spark-affairs')).toBe('spark://space/personal/app/spark-affairs');
    expect(buildSparkUrl({ type: 'org', orgId: 'org-1' }, 'spark-contacts')).toBe('spark://space/org-1/app/spark-contacts');
  });

  it('对象粒度：…/object/<对象>', () => {
    expect(buildSparkUrl({ type: 'org', orgId: 'org-1' }, 'spark-affairs', 'affair-9')).toBe(
      'spark://space/org-1/app/spark-affairs/object/affair-9'
    );
  });

  it('寻址段 URL 编码（含斜杠/空格不破坏结构）', () => {
    expect(buildSparkUrl({ type: 'org', orgId: 'org 1/x' }, 'p', 'o b')).toBe(
      'spark://space/org%201%2Fx/app/p/object/o%20b'
    );
  });
});

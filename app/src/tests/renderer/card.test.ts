// 名片内容解析单测：spark-card JSON / 带标签多行文本 / 裸 RootID
import { describe, expect, it } from 'vitest';
import { parseCard, trimCardAddresses } from '../../utils/card';

const ROOT = 'a'.repeat(64);

describe('trimCardAddresses', () => {
  const PEER = '12D3KooW'.padEnd(52, 'x');
  it('剔除中继电路、通配与回环地址', () => {
    const relay = `/ip4/203.0.113.9/tcp/4001/p2p/${PEER}/p2p-circuit`;
    const input = [
      relay,
      '/ip4/0.0.0.0/tcp/15002',
      '/ip6/::/tcp/15002',
      '/ip4/127.0.0.1/tcp/15002/ws',
      '/ip6/::1/tcp/15002',
      '/ip4/192.168.1.5/tcp/15002/ws'
    ];
    const trimmed = trimCardAddresses(input);
    expect(trimmed).toHaveLength(1);
    expect(trimmed).toEqual(['/ip4/192.168.1.5/tcp/15002/ws']);
    expect(trimmed.every((a) => !a.includes('/p2p-circuit') && !a.includes('/0.0.0.0') && !a.includes('/::'))).toBe(true);
  });

  it('公网地址优先于私网地址，封顶 3 条', () => {
    const input = [
      '/ip4/192.168.1.5/tcp/15002/ws',
      '/ip6/2408:8352:443:6471::e62/tcp/15002',
      '/ip4/203.0.113.9/tcp/15002',
      '/ip4/10.0.0.8/tcp/15002/ws'
    ];
    const trimmed = trimCardAddresses(input);
    expect(trimmed).toHaveLength(3);
    // 公网 IPv6(0) > 公网 IPv4(2) > 私网 IPv4(3)；私网内短地址优先（10.0.0.8 短于 192.168.1.5）
    expect(trimmed).toEqual([
      '/ip6/2408:8352:443:6471::e62/tcp/15002',
      '/ip4/203.0.113.9/tcp/15002',
      '/ip4/10.0.0.8/tcp/15002/ws'
    ]);
  });

  it('公网 IPv6 优先于公网 IPv4', () => {
    const input = ['/ip4/203.0.113.9/tcp/15002', '/ip6/2408:8352:443:6471::e62/tcp/15002'];
    expect(trimCardAddresses(input)).toEqual([
      '/ip6/2408:8352:443:6471::e62/tcp/15002',
      '/ip4/203.0.113.9/tcp/15002'
    ]);
  });

  it('同档短地址优先（同 rank 内按长度）', () => {
    const input = ['/ip4/192.168.1.5/tcp/15002/ws/extra', '/ip4/10.0.0.8/tcp/15002'];
    const trimmed = trimCardAddresses(input);
    expect(trimmed[0]).toBe('/ip4/10.0.0.8/tcp/15002');
  });

  it('真实地址列表：公网 IPv6 优先、剔除回环/中继，封顶 3 条', () => {
    const real = [
      '/ip6/2408:8352:443:6471::e62/tcp/15002',
      '/ip6/2408:8352:443:6471:d0ac:5357:70dd:6350/tcp/15002',
      '/ip6/2408:8352:443:cb81:6515:a07f:8444:78e4/tcp/15002',
      '/ip6/2408:8352:443:6471:242f:3499:c31a:75e/tcp/15002',
      '/ip6/2408:8352:443:cb81:242f:3499:c31a:75e/tcp/15002',
      '/ip6/::1/tcp/15002',
      '/ip4/192.168.240.1/tcp/15002/ws',
      '/ip4/192.168.31.134/tcp/15002/ws',
      '/ip4/127.0.0.1/tcp/15002/ws',
      '/ip6/2408:8352:443:6471::e62/tcp/15002/ws',
      '/ip6/2408:8352:443:6471:d0ac:5357:70dd:6350/tcp/15002/ws',
      '/ip6/2408:8352:443:cb81:6515:a07f:8444:78e4/tcp/15002/ws',
      '/ip6/2408:8352:443:6471:242f:3499:c31a:75e/tcp/15002/ws',
      '/ip6/2408:8352:443:cb81:242f:3499:c31a:75e/tcp/15002/ws',
      '/ip6/::1/tcp/15002/ws',
      '/ip4/192.168.240.1/tcp/15002',
      '/ip4/192.168.31.134/tcp/15002',
      '/ip4/127.0.0.1/tcp/15002'
    ];
    const trimmed = trimCardAddresses(real);
    // 全为回环的 ::1 与 127.0.0.1 应剔除；公网 IPv6 优先；同一 IP 的 tcp/ws 去重只留 tcp；封顶 3 条
    expect(trimmed).toHaveLength(3);
    expect(trimmed.every((a) => !a.includes('127.0.0.1') && !a.includes('::1'))).toBe(true);
    // 去重：同一 (IP,端口) 只保留一种（tcp），不出现 ws 变体
    expect(trimmed.every((a) => !a.endsWith('/ws'))).toBe(true);
    // 都是公网 IPv6 的 tcp 直连地址，且保留 3 个不同 IP（最短的 ::e62 在最前）
    expect(trimmed.every((a) => a.startsWith('/ip6/2408:8352:443:') && a.endsWith('/tcp/15002'))).toBe(true);
    expect(trimmed[0]).toBe('/ip6/2408:8352:443:6471::e62/tcp/15002');
    expect(new Set(trimmed.map((a) => a.replace(/\/tcp\/15002$/, ''))).size).toBe(3);
  });

  it('tcp/ws 同地址去重：只保留 tcp，不占名额', () => {
    const input = [
      '/ip6/2408:8352:443:6471::e62/tcp/15002/ws',
      '/ip6/2408:8352:443:6471::e62/tcp/15002',
      '/ip6/2408:8352:443:6471:abcd::1/tcp/15002/ws',
      '/ip6/2408:8352:443:6471:abcd::1/tcp/15002'
    ];
    const trimmed = trimCardAddresses(input);
    expect(trimmed).toEqual([
      '/ip6/2408:8352:443:6471::e62/tcp/15002',
      '/ip6/2408:8352:443:6471:abcd::1/tcp/15002'
    ]);
  });

  it('仅 ws 变体时保留 ws（不丢地址）', () => {
    const input = ['/ip6/2408:8352:443:6471::e62/tcp/15002/ws'];
    expect(trimCardAddresses(input)).toEqual(['/ip6/2408:8352:443:6471::e62/tcp/15002/ws']);
  });

  it('无可用地址时返回空数组', () => {
    expect(trimCardAddresses([])).toEqual([]);
    expect(trimCardAddresses(['/ip4/0.0.0.0/tcp/1'])).toEqual([]);
    expect(trimCardAddresses(['/ip4/127.0.0.1/tcp/1', '/ip6/::1/tcp/1'])).toEqual([]);
  });
});

describe('parseCard', () => {
  it('解析 spark-card JSON（含节点信息）', () => {
    const json = JSON.stringify({
      type: 'spark-card',
      rootId: ROOT,
      peerId: '12D3KooWTest',
      addresses: ['/ip4/127.0.0.1/tcp/15002/ws', '/dns4/example.com/tcp/443/wss']
    });
    expect(parseCard(json)).toEqual({
      rootId: ROOT,
      peerId: '12D3KooWTest',
      addresses: ['/ip4/127.0.0.1/tcp/15002/ws', '/dns4/example.com/tcp/443/wss']
    });
  });

  it('解析不带节点信息的 spark-card JSON', () => {
    const json = JSON.stringify({ type: 'spark-card', rootId: ROOT });
    expect(parseCard(json)).toEqual({ rootId: ROOT, peerId: undefined, addresses: undefined });
  });

  it('解析「RootID / PeerId / P2P Addresses」多行文本', () => {
    const text = `RootID: ${ROOT}\nPeerId: 12D3KooWTest\nP2P Addresses:\n/ip4/127.0.0.1/tcp/15002/ws\n/dns4/example.com/tcp/443/wss`;
    expect(parseCard(text)).toEqual({
      rootId: ROOT,
      peerId: '12D3KooWTest',
      addresses: ['/ip4/127.0.0.1/tcp/15002/ws', '/dns4/example.com/tcp/443/wss']
    });
  });

  it('多行文本中 PeerId 为「未获取」时视为无节点信息', () => {
    const text = `RootID: ${ROOT}\nPeerId: 未获取\nP2P Addresses:\n未获取`;
    expect(parseCard(text)).toEqual({ rootId: ROOT, peerId: undefined, addresses: undefined });
  });

  it('解析极简标记格式 R:/P:/A:（地址共用 A: 标签 + 裸列表）', () => {
    const text = `R:${ROOT}\nP:12D3KooWTest\nA:\n/ip6/2408:8352:443:6471::e62/tcp/15002\n/ip4/192.168.1.5/tcp/15002/ws`;
    expect(parseCard(text)).toEqual({
      rootId: ROOT,
      peerId: '12D3KooWTest',
      addresses: ['/ip6/2408:8352:443:6471::e62/tcp/15002', '/ip4/192.168.1.5/tcp/15002/ws']
    });
  });

  it('解析极简标记格式：兼容每行 A:<地址> 的旧极简格式', () => {
    const text = `R:${ROOT}\nP:12D3KooWTest\nA:/ip4/192.168.1.5/tcp/15002`;
    expect(parseCard(text)).toEqual({
      rootId: ROOT,
      peerId: '12D3KooWTest',
      addresses: ['/ip4/192.168.1.5/tcp/15002']
    });
  });

  it('极简标记格式：无 PeerId / 无地址时对应字段为空', () => {
    const text = `R:${ROOT}\nP:未获取\nA:未获取`;
    expect(parseCard(text)).toEqual({ rootId: ROOT, peerId: undefined, addresses: undefined });
  });

  it('解析裸 64 位十六进制 RootID', () => {
    expect(parseCard(`  ${ROOT}  `)).toEqual({ rootId: ROOT });
  });

  it('空文本 / 无 RootID 返回 null', () => {
    expect(parseCard('')).toBeNull();
    expect(parseCard('   ')).toBeNull();
    expect(parseCard('随便一段没有身份的内容')).toBeNull();
  });
});

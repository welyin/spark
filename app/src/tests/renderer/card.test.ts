// 名片内容解析单测：spark-card JSON / 带标签多行文本 / 裸 RootID
import { describe, expect, it } from 'vitest';
import { parseCard } from '../../utils/card';

const ROOT = 'a'.repeat(64);

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

  it('解析裸 64 位十六进制 RootID', () => {
    expect(parseCard(`  ${ROOT}  `)).toEqual({ rootId: ROOT });
  });

  it('空文本 / 无 RootID 返回 null', () => {
    expect(parseCard('')).toBeNull();
    expect(parseCard('   ')).toBeNull();
    expect(parseCard('随便一段没有身份的内容')).toBeNull();
  });
});

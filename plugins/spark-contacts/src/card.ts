/**
 * 名片内容解析（添加朋友 / 组织添加成员共用）。
 *
 * 名片两种载体内容一致（我的名片模块生成）：
 * - 二维码名片：jsQR 解码出 spark-card JSON（节点在线）或裸 RootID（节点离线）
 * - 名片内容文本：「RootID / PeerId / P2P Addresses」多行格式（一键复制）
 */
import { decodeQrTextFromFile } from './qr-decode';

export type CardInfo = {
  rootId: string;
  peerId?: string;
  addresses?: string[];
};

/** 名片/二维码地址裁剪上限：最多保留若干条最小可拨地址（与内核 trim_qr_addresses 对齐） */
export const CARD_MAX_ADDRESSES = 3;

/**
 * 名片地址静态可达性优先级（对齐内核 `p2p::peer_targets::sort_addresses`，
 * 数字越小越优先）：
 * - 0 公网 IPv6（跨网/异地最可靠，移动网络 IPv6 是移动端唯一直连可能）
 * - 1 私网 IPv6（ULA fc00::/7 与链路本地）
 * - 2 公网 IPv4
 * - 3 私网 IPv4（10/8、172.16/12、192.168/16，同局域网场景关键）
 * - 回环（127.0.0.1/::1）与中继电路、通配地址一律剔除（对端不可拨 / 不可路由）
 * 同档内短地址在前（体积敏感）。
 */
export function cardAddressRank(addr: string): number {
  const ip4 = addr.match(/\/ip4\/(\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3})/)?.[1] ?? '';
  if (ip4) {
    const isPrivate = /^10\./.test(ip4) || /^192\.168\./.test(ip4) || /^172\.(1[6-9]|2\d|3[01])\./.test(ip4);
    return isPrivate ? 3 : 2;
  }
  // IPv6：含 `/ip6/` 段且非回环。私网（ULA fc00::/7 或链路本地 fe80::/10）→ 1，否则公网 → 0
  if (addr.includes('/ip6/')) {
    return /\/ip6\/(fc|fd|fe[89ab])/i.test(addr) ? 1 : 0;
  }
  return 4; // 其他（dns 等）垫底
}

/** 是否应剔除该地址（对端不可拨 / 不可路由 / 中继） */
function shouldDropAddress(addr: string): boolean {
  return (
    addr.includes('/p2p-circuit') ||
    addr.includes('/ip4/0.0.0.0') ||
    addr.includes('/ip6/::') ||
    addr.includes('/ip4/127.') ||
    addr.includes('/ip6/::1')
  );
}

/**
 * 去重键：去掉末尾 `/ws` / `/wss` 段后的"基础地址"。
 *
 * 同一台设备同一端口往往同时监听 tcp 与 ws（如 `/ip6/…/tcp/15002` 与
 * `/ip6/…/tcp/15002/ws` 只是传输层不同）。若两者都占裁剪名额，会浪费——
 * tcp 与 ws 是同一 IP+端口的两条候选，拨通任意一条即连上。去重时以该键归并，
 * 保留可达性更高的一种（tcp，Android 端还会过滤 `/ws`）。
 */
function addressKey(addr: string): string {
  return addr.replace(/\/wss$/, '').replace(/\/ws$/, '');
}

/**
 * 名片二维码地址裁剪：把完整监听地址列表收敛成 QR 可承载的最小可拨子集。
 *
 * 完整 `p2p.info().addresses`（`listen_addr_strings`）可能含：多条本机网卡
 * IPv4/IPv6、external 地址、以及中继电路地址（`/ip4/<relayIP>/tcp/<port>/p2p/
 * <52 字符 peerId>/p2p-circuit`，每条近百字符）。全量携带会显著拉高 QR 载荷
 * 体积 → QR 版本升高、模块密度过大，手机摄像头难以对焦识别。
 *
 * 裁剪/排序对齐内核可达性优先级（`sort_addresses`）——按"最可能拨通"而非
 * "最短"来选：
 * - 剔除中继电路、通配（`/ip4/0.0.0.0`、`/ip6/::`）与**回环**（`127.0.0.1`、`::1`，
 *   对端扫到指的是它自己，必然不可拨）；
 * - **去重**：同一 (IP, 端口) 的 tcp/ws 变体只保留一种（优先 tcp），不白占名额；
 * - 排序：公网 IPv6 > 私网 IPv6 > 公网 IPv4 > 私网 IPv4（跨网/同网尽可能兼容），
 *   同档短地址在前；
 * - 总量封顶 [`CARD_MAX_ADDRESSES`]，超限取最靠前的若干条。
 */
export function trimCardAddresses(addresses: string[]): string[] {
  const kept = addresses.filter((a) => !shouldDropAddress(a));
  // 先按可达性 rank + 长度排序，使同 (IP,端口) 中 rank 更高（tcp）者在前
  kept.sort((a, b) => cardAddressRank(a) - cardAddressRank(b) || a.length - b.length);
  const seen = new Set<string>();
  const deduped: string[] = [];
  for (const addr of kept) {
    const key = addressKey(addr);
    if (seen.has(key)) continue;
    seen.add(key);
    deduped.push(addr);
    if (deduped.length >= CARD_MAX_ADDRESSES) break;
  }
  return deduped;
}

/** 解析名片内容：spark-card JSON / 极简标记(R:/P:/A:) / 带标签多行文本 / 裸 64 位十六进制 */
export function parseCard(text: string): CardInfo | null {
  const trimmed = text.trim();
  if (!trimmed) {
    return null;
  }
  // 1) spark-card JSON（二维码名片编码格式）
  try {
    const parsed = JSON.parse(trimmed) as { type?: string; rootId?: unknown; peerId?: unknown; addresses?: unknown };
    if (parsed?.type === 'spark-card' && typeof parsed.rootId === 'string') {
      return {
        rootId: parsed.rootId,
        peerId: typeof parsed.peerId === 'string' ? parsed.peerId : undefined,
        addresses: Array.isArray(parsed.addresses) ? parsed.addresses.filter((a): a is string => typeof a === 'string') : undefined
      };
    }
  } catch {
    // 非 JSON，继续按文本匹配
  }
  // 2) 极简标记格式（名片内容一键复制格式，R:/P:/A: 前缀，防错且更简洁）
  const rLine = trimmed.match(/^R:\s*([0-9a-fA-F]{64})\s*$/m);
  if (rLine) {
    const pLine = trimmed.match(/^P:\s*(\S+)\s*$/m);
    // 地址支持两种形态（统一切换成共用 A: 标签 + 裸地址列表，更省字符）：
    //   a) A: 独占一行 + 后续裸地址行（/ 开头，推荐格式）
    //   b) 每行 A:<地址>（兼容旧极简格式）
    // 定位 A: 段：找到第一个 A: 行，其后直至 R:/P: 或文本末尾的所有 / 开头行视为地址
    const aIdx = trimmed.indexOf('\nA:');
    let addresses: string[] = [];
    if (aIdx >= 0) {
      const afterA = trimmed.slice(aIdx + 3).trim();
      for (const line of afterA.split(/\r?\n/)) {
        const t = line.trim();
        if (t.startsWith('/')) addresses.push(t);
        else if (t.startsWith('R:') || t.startsWith('P:') || t === '未获取') break;
      }
    }
    return {
      rootId: rLine[1],
      peerId: pLine && pLine[1] !== '未获取' ? pLine[1] : undefined,
      addresses: addresses.length > 0 ? addresses : undefined
    };
  }
  // 3) 带标签多行文本（旧格式，兼容旧设备/旧名片）
  const rootMatch = trimmed.match(/RootID[:：]\s*([0-9a-fA-F]{64})/);
  if (rootMatch) {
    const peerMatch = trimmed.match(/PeerId[:：]\s*(\S+)/);
    const addrSection = trimmed.match(/P2P Addresses[:：]\s*\n([\s\S]+)/);
    const addresses = addrSection
      ? addrSection[1].split(/\r?\n/).map((line) => line.trim()).filter((line) => line.startsWith('/'))
      : undefined;
    return {
      rootId: rootMatch[1],
      peerId: peerMatch && peerMatch[1] !== '未获取' ? peerMatch[1] : undefined,
      addresses: addresses && addresses.length > 0 ? addresses : undefined
    };
  }
  // 4) 裸 64 位十六进制
  const bare = trimmed.match(/\b([0-9a-fA-F]{64})\b/);
  return bare ? { rootId: bare[1] } : null;
}

/** 上传名片图片：jsQR 本地识别二维码（缩放阶梯），返回解码文本（识别失败返回 ''） */
export function decodeCardImage(file: File): Promise<string> {
  return decodeQrTextFromFile(file);
}

/**
 * 名片内容解析（添加朋友 / 组织添加成员共用）。
 *
 * 名片两种载体内容一致（我的名片模块生成）：
 * - 二维码名片：jsQR 解码出 spark-card JSON（节点在线）或裸 RootID（节点离线）
 * - 名片内容文本：「RootID / PeerId / P2P Addresses」多行格式（一键复制）
 *
 * 注意（评审 U3 登记）：名片线形是协议面资产，此处四种格式解析为插件层
 * 复刻（内核只认签名节点名片，见 requests.ts 注释），终态应内核化
 * （sdk contacts.parseCard，见 wiki/product/todo.md 登记），演进时防止双份漂移。
 */
import { decodeQrTextFromFile } from './qr-decode';

export type CardInfo = {
  rootId: string;
  peerId?: string;
  addresses?: string[];
};

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

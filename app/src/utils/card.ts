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

/** 解析名片内容：spark-card JSON / 带标签多行文本 / 裸 64 位十六进制 */
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
  // 2) 带标签多行文本（名片内容一键复制格式）
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
  // 3) 裸 64 位十六进制
  const bare = trimmed.match(/\b([0-9a-fA-F]{64})\b/);
  return bare ? { rootId: bare[1] } : null;
}

/** 上传名片图片：jsQR 本地识别二维码（缩放阶梯），返回解码文本（识别失败返回 ''） */
export function decodeCardImage(file: File): Promise<string> {
  return decodeQrTextFromFile(file);
}

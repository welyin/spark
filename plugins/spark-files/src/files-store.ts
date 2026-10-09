/**
 * spark-files 共享逻辑（纯函数，可单测）：文件元数据记录线形、集合声明、
 * 照片判定、排序/过滤、展示助手。
 *
 * 存储模型（A42）：
 * - 文件本体 → sdk.content 内容面（内容寻址 cid；保存即声明 provider，
 *   「持有即做种」；pinRoot('user-pin') 持有，unpinRoot 后进 GC 宽限期）；
 * - 元数据 → sdk.data 声明式集合 spark-files:files（写库即同步；org 空间
 *   由桥注入 orgId 落组织集合，personal 空间落个人集合——各走各的域边界）。
 */
import type { PluginContentAPI, PluginDataDeclaration, PluginSysAPI } from '../../../packages/plugin-sdk/src';

/** 元数据集合名（前缀必须 == 插件 id，P6 声明式数据 API 约定） */
export const FILES_COLLECTION = 'spark-files:files';

/** 集合声明（幂等；sync 缺省 = 自设备/组织内同步，lww-record 缺省合并） */
export const FILES_COLLECTION_DECLARATION: PluginDataDeclaration = {
  name: FILES_COLLECTION
};

/** GC 根标记：用户显式持有（与 sdk.content pinRoot/unpinRoot 配对） */
export const FILE_PIN_ROOT = 'user-pin';

/** 文件元数据记录（sdk.data 集合值；cid 即集合键） */
export type SparkFileRecord = {
  cid: string;
  name: string;
  size: number;
  /** MIME 类型（浏览器 File.type；未知为空串） */
  mime: string;
  createdAt: number;
};

/** 照片判定（照片视图过滤口径：image/* 即照片） */
export function isPhoto(mime: string): boolean {
  return mime.startsWith('image/');
}

/** 列表排序：上传时间降序（返回新数组） */
export function sortFiles(records: SparkFileRecord[]): SparkFileRecord[] {
  return [...records].sort((a, b) => b.createdAt - a.createdAt || a.name.localeCompare(b.name, 'zh'));
}

/** 照片视图过滤（返回新数组） */
export function filterPhotos(records: SparkFileRecord[]): SparkFileRecord[] {
  return records.filter((record) => isPhoto(record.mime));
}

/** 搜索过滤：按文件名（大小写不敏感子串；空串全量） */
export function filterByKeyword(records: SparkFileRecord[], keyword: string): SparkFileRecord[] {
  const kw = keyword.trim().toLowerCase();
  if (!kw) {
    return records;
  }
  return records.filter((record) => record.name.toLowerCase().includes(kw));
}

/** data.query 结果 → 记录列表（剔除畸形条目，诚实呈现可读部分） */
export function normalizeRecords(items: Array<{ key: string; value: unknown }>): SparkFileRecord[] {
  const out: SparkFileRecord[] = [];
  for (const item of items) {
    const value = item.value as Partial<SparkFileRecord> | null;
    if (
      value &&
      typeof value === 'object' &&
      typeof value.cid === 'string' &&
      typeof value.name === 'string' &&
      typeof value.size === 'number' &&
      typeof value.createdAt === 'number'
    ) {
      out.push({
        cid: value.cid,
        name: value.name,
        size: value.size,
        mime: typeof value.mime === 'string' ? value.mime : '',
        createdAt: value.createdAt
      });
    }
  }
  return out;
}

/** 类型展示名（列表「类型」列；photo/其余按 MIME 主类型） */
export function fileTypeLabel(mime: string): string {
  if (!mime) {
    return '文件';
  }
  if (isPhoto(mime)) {
    return '照片';
  }
  if (mime.startsWith('video/')) {
    return '视频';
  }
  if (mime.startsWith('audio/')) {
    return '音频';
  }
  if (mime === 'application/pdf') {
    return 'PDF';
  }
  if (mime.startsWith('text/')) {
    return '文本';
  }
  return mime.split('/')[1]?.toUpperCase().slice(0, 8) || '文件';
}

export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 0) {
    return '0 B';
  }
  if (bytes < 1024) {
    return `${bytes} B`;
  }
  const units = ['KB', 'MB', 'GB', 'TB'];
  let value = bytes;
  let unit = 'B';
  for (const next of units) {
    if (value < 1024) {
      break;
    }
    value /= 1024;
    unit = next;
  }
  return `${value.toFixed(value >= 100 ? 0 : 1)} ${unit}`;
}

export function formatDate(timestamp: number): string {
  return new Intl.DateTimeFormat('zh-CN', {
    year: 'numeric',
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit'
  }).format(new Date(timestamp));
}

/** dataURL → base64 主体（FileReader readAsDataURL 出参剥前缀） */
export function dataUrlToBase64(dataUrl: string): string {
  const comma = dataUrl.indexOf(',');
  return comma >= 0 ? dataUrl.slice(comma + 1) : dataUrl;
}

/** 下载结果（壳层代存通路）：saved=已写入用户所选路径；cancelled=用户取消对话框；pending=本体未取回 */
export type DownloadOutcome = 'saved' | 'cancelled' | 'pending';

/**
 * 下载文件本体并代存到用户所选路径（A42 评审修复：插件沙箱 iframe 无
 * allow-downloads，Blob 锚点下载各 WebView 口径不一——改走 sdk.sys.saveFile
 * 壳层代开保存对话框，同 market.pickSpkg / org.exportData 先例）。
 * 本体取回顺序：本地命中直接读；未命中经 Kad provider 拉取；都未命中返回 pending。
 */
export async function downloadRecord(
  content: PluginContentAPI,
  sys: PluginSysAPI,
  record: SparkFileRecord
): Promise<DownloadOutcome> {
  const local = await content.readBlob(record.cid);
  const base64 = local ?? (await content.fetchBlob(record.cid));
  if (base64 === null) {
    return 'pending';
  }
  const result = await sys.saveFile({ name: record.name, dataBase64: base64 });
  return result.cancelled ? 'cancelled' : 'saved';
}

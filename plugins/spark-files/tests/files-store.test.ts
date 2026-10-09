/**
 * spark-files files-store 纯逻辑测试（A42）：元数据归一化、照片判定、
 * 排序/过滤、类型展示名、dataURL 剥前缀。
 */
import { describe, expect, it, vi } from 'vitest';
import {
  dataUrlToBase64,
  downloadRecord,
  FILE_PIN_ROOT,
  FILES_COLLECTION,
  FILES_COLLECTION_DECLARATION,
  fileTypeLabel,
  filterByKeyword,
  filterPhotos,
  formatBytes,
  isPhoto,
  normalizeRecords,
  sortFiles,
  type SparkFileRecord
} from '../src/files-store';

const recordOf = (patch: Partial<SparkFileRecord>): SparkFileRecord => ({
  cid: 'c'.repeat(64),
  name: 'file.txt',
  size: 100,
  mime: 'text/plain',
  createdAt: 1000,
  ...patch
});

describe('集合声明与线形', () => {
  it('集合名前缀 == 插件 id（P6 约定）；声明幂等可重放', () => {
    expect(FILES_COLLECTION).toBe('spark-files:files');
    expect(FILES_COLLECTION.startsWith('spark-files:')).toBe(true);
    expect(FILES_COLLECTION_DECLARATION.name).toBe(FILES_COLLECTION);
    expect(FILE_PIN_ROOT).toBe('user-pin');
  });

  it('normalizeRecords：合法条目放行，畸形条目剔除', () => {
    const items = [
      { key: 'a', value: recordOf({ name: 'ok.png', mime: 'image/png' }) },
      { key: 'b', value: { cid: 1, name: 'bad' } },
      { key: 'c', value: null },
      { key: 'd', value: { cid: 'x', name: 'no-size' } },
      // mime 缺省归一为空串
      { key: 'e', value: { cid: 'e', name: 'nomime.bin', size: 1, createdAt: 2 } }
    ];
    const records = normalizeRecords(items);
    expect(records.map((record) => record.name)).toEqual(['ok.png', 'nomime.bin']);
    expect(records[1].mime).toBe('');
  });
});

describe('照片判定与过滤', () => {
  it('isPhoto：image/* 即照片', () => {
    expect(isPhoto('image/png')).toBe(true);
    expect(isPhoto('image/jpeg')).toBe(true);
    expect(isPhoto('text/plain')).toBe(false);
    expect(isPhoto('')).toBe(false);
  });

  it('filterPhotos：只留照片（不改入参）', () => {
    const records = [recordOf({ name: 'a.png', mime: 'image/png' }), recordOf({ name: 'b.txt' })];
    const photos = filterPhotos(records);
    expect(photos.map((record) => record.name)).toEqual(['a.png']);
    expect(records).toHaveLength(2);
  });
});

describe('排序与搜索', () => {
  it('sortFiles：上传时间降序，同时刻按文件名 locale 序', () => {
    const records = [
      recordOf({ name: 'b.txt', createdAt: 100 }),
      recordOf({ name: 'a.txt', createdAt: 100 }),
      recordOf({ name: 'c.txt', createdAt: 300 })
    ];
    expect(sortFiles(records).map((record) => record.name)).toEqual(['c.txt', 'a.txt', 'b.txt']);
    expect(records[0].name).toBe('b.txt');
  });

  it('filterByKeyword：大小写不敏感子串；空串全量', () => {
    const records = [recordOf({ name: 'Report.PDF' }), recordOf({ name: 'photo.png' })];
    expect(filterByKeyword(records, 'report').map((record) => record.name)).toEqual(['Report.PDF']);
    expect(filterByKeyword(records, '  ')).toHaveLength(2);
  });
});

describe('展示助手', () => {
  it('fileTypeLabel：照片/视频/音频/PDF/文本/未知', () => {
    expect(fileTypeLabel('image/png')).toBe('照片');
    expect(fileTypeLabel('video/mp4')).toBe('视频');
    expect(fileTypeLabel('audio/mpeg')).toBe('音频');
    expect(fileTypeLabel('application/pdf')).toBe('PDF');
    expect(fileTypeLabel('text/markdown')).toBe('文本');
    expect(fileTypeLabel('')).toBe('文件');
    expect(fileTypeLabel('application/zip')).toBe('ZIP');
  });

  it('formatBytes：分级进位', () => {
    expect(formatBytes(0)).toBe('0 B');
    expect(formatBytes(1023)).toBe('1023 B');
    expect(formatBytes(1024)).toBe('1.0 KB');
    expect(formatBytes(3 * 1024 * 1024 * 1024)).toBe('3.0 GB');
  });

  it('dataUrlToBase64：剥 dataURL 前缀；无前缀原样', () => {
    expect(dataUrlToBase64('data:image/png;base64,QUJD')).toBe('QUJD');
    expect(dataUrlToBase64('QUJD')).toBe('QUJD');
  });
});

describe('下载（A42 修复：壳层代存通路 downloadRecord）', () => {
  const contentOf = (local: string | null, remote: string | null = null) => ({
    readBlob: vi.fn(async () => local),
    fetchBlob: vi.fn(async () => remote)
  });
  const sysOf = (cancelled: boolean) => ({
    saveFile: vi.fn(async () => (cancelled ? { cancelled: true as const } : { cancelled: false as const, path: '/tmp/out' }))
  });

  it('本地命中直接代存：不触发网络拉取，saveFile 收文件名+base64', async () => {
    const content = contentOf('TE9DQUw=');
    const sys = sysOf(false);
    const outcome = await downloadRecord(content as never, sys as never, recordOf({ name: 'a.txt' }));
    expect(outcome).toBe('saved');
    expect(content.fetchBlob).not.toHaveBeenCalled();
    expect(sys.saveFile).toHaveBeenCalledWith({ name: 'a.txt', dataBase64: 'TE9DQUw=' });
  });

  it('本地未命中经 fetchBlob 拉取后代存', async () => {
    const content = contentOf(null, 'UkVNT1RF');
    const sys = sysOf(false);
    const outcome = await downloadRecord(content as never, sys as never, recordOf({}));
    expect(outcome).toBe('saved');
    expect(content.fetchBlob).toHaveBeenCalledTimes(1);
    expect(sys.saveFile).toHaveBeenCalledWith(expect.objectContaining({ dataBase64: 'UkVNT1RF' }));
  });

  it('两端都未命中返回 pending，不打开保存对话框', async () => {
    const content = contentOf(null, null);
    const sys = sysOf(false);
    const outcome = await downloadRecord(content as never, sys as never, recordOf({}));
    expect(outcome).toBe('pending');
    expect(sys.saveFile).not.toHaveBeenCalled();
  });

  it('用户取消保存对话框返回 cancelled', async () => {
    const outcome = await downloadRecord(contentOf('QUJD') as never, sysOf(true) as never, recordOf({}));
    expect(outcome).toBe('cancelled');
  });
});

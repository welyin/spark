import { mkdtemp, mkdir, readFile, rm, writeFile } from 'fs/promises';
import { tmpdir } from 'os';
import path from 'path';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import {
  hashVendorTree,
  readLock,
  verifyLock,
  vendorPathForRepo,
  writeLock
} from '../src/lockfile.js';

const COMMIT = '0123456789abcdef0123456789abcdef01234567';
const REPO = 'github.com/spark-samples/sample-lib';

let dir: string;

async function makeVendorTree(files: Record<string, string>) {
  const vendorDir = path.join(dir, ...vendorPathForRepo(REPO).split('/'));
  for (const [rel, content] of Object.entries(files)) {
    const target = path.join(vendorDir, ...rel.split('/'));
    await mkdir(path.dirname(target), { recursive: true });
    await writeFile(target, content, 'utf8');
  }
  return vendorDir;
}

beforeEach(async () => {
  dir = await mkdtemp(path.join(tmpdir(), 'spark-cli-lock-'));
});

afterEach(async () => {
  await rm(dir, { recursive: true, force: true });
});

describe('hashVendorTree', () => {
  it('同内容同哈希，与文件创建顺序无关', async () => {
    const a = await makeVendorTree({ 'b.js': 'B', 'sub/a.js': 'A' });
    const first = await hashVendorTree(a);
    await rm(dir, { recursive: true, force: true });
    await mkdir(dir, { recursive: true });
    const b = await makeVendorTree({ 'sub/a.js': 'A', 'b.js': 'B' });
    const second = await hashVendorTree(b);
    expect(first.hash).toBe(second.hash);
    expect(first.files).toBe(2);
  });

  it('内容变更即变哈希', async () => {
    const vendorDir = await makeVendorTree({ 'a.js': 'A' });
    const before = await hashVendorTree(vendorDir);
    await writeFile(path.join(vendorDir, 'a.js'), 'A-tampered', 'utf8');
    const after = await hashVendorTree(vendorDir);
    expect(after.hash).not.toBe(before.hash);
  });
});

describe('lock 写入 / 读取 / 核验', () => {
  it('writeLock → readLock → verifyLock 全绿', async () => {
    const vendorDir = await makeVendorTree({ 'index.js': 'export const x = 1;\n' });
    const { hash, files } = await hashVendorTree(vendorDir);
    await writeLock(dir, [{ repo: REPO, commit: COMMIT, hash, vendorPath: vendorPathForRepo(REPO), files }]);
    const lock = await readLock(dir);
    expect(lock.lockfileVersion).toBe(1);
    const info = await verifyLock(dir, [{ repo: REPO, commit: COMMIT, hash }], lock);
    expect(info.get(REPO)?.files).toBe(1);
  });

  it('锁文件缺失必拒（E_LOCK_MISSING）', async () => {
    await expect(readLock(dir)).rejects.toThrowError(/E_LOCK_MISSING/);
  });

  it('vendor 树被篡改必拒（E_LIB_INTEGRITY）', async () => {
    const vendorDir = await makeVendorTree({ 'index.js': 'victim\n' });
    const { hash } = await hashVendorTree(vendorDir);
    const lock = await writeLock(dir, [{ repo: REPO, commit: COMMIT, hash, vendorPath: vendorPathForRepo(REPO), files: 1 }]);
    await writeFile(path.join(vendorDir, 'index.js'), 'malicious\n', 'utf8');
    await expect(verifyLock(dir, [{ repo: REPO, commit: COMMIT, hash }], lock))
      .rejects.toThrowError(/E_LIB_INTEGRITY/);
  });

  it('manifest 与锁不一致必拒（E_LOCK_MISMATCH）', async () => {
    const vendorDir = await makeVendorTree({ 'index.js': 'x\n' });
    const { hash } = await hashVendorTree(vendorDir);
    const lock = await writeLock(dir, [{ repo: REPO, commit: COMMIT, hash, vendorPath: vendorPathForRepo(REPO), files: 1 }]);
    // commit 漂移
    await expect(
      verifyLock(dir, [{ repo: REPO, commit: 'f'.repeat(40), hash }], lock)
    ).rejects.toThrowError(/E_LOCK_MISMATCH/);
    // 锁中多出未声明条目
    await expect(
      verifyLock(dir, [], lock)
    ).rejects.toThrowError(/E_LOCK_MISMATCH/);
  });
});

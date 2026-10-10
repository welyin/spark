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

describe('A33 评审下批项（U4/S1）', () => {
  it('路径按码位升序参与哈希（非 BMP 字符与 UTF-16 码元序分歧场景）', async () => {
    // U+20BB7（𠮷，增补平面）码位 0x20BB7 > U+FF21（Ａ）0xFF21，码位序 Ａ.js 在前；
    // 但 𠮷 的 UTF-16 首码元 0xD842 < 0xFF21，码元序相反——两序分歧，
    // 哈希须按码位序复算，跨实现一致
    const vendorDir = await makeVendorTree({ '\u{20BB7}.js': 'X', '\uFF21.js': 'A' });
    const { createHash } = await import('crypto');
    const sha = (s: string) => createHash('sha256').update(s).digest('hex');
    const expected = sha(`${sha('A')}  \uFF21.js\n${sha('X')}  \u{20BB7}.js\n`);
    const { hash } = await hashVendorTree(vendorDir);
    expect(hash).toBe(expected);
  });

  it('vendor 内含符号链接必拒（E_VENDOR_SYMLINK，不静默跳过）', async () => {
    const vendorDir = await makeVendorTree({ 'index.js': 'x\n' });
    const { symlink } = await import('fs/promises');
    await symlink(path.join(vendorDir, 'index.js'), path.join(vendorDir, 'link.js'));
    await expect(hashVendorTree(vendorDir)).rejects.toThrowError(/E_VENDOR_SYMLINK/);
  });

  it('writeLock 幂等：libraries 不变保留 generatedAt，变化才刷新', async () => {
    const entry = { repo: REPO, commit: COMMIT, hash: 'a'.repeat(64), vendorPath: vendorPathForRepo(REPO), files: 1 };
    const first = await writeLock(dir, [entry]);
    await new Promise((resolve) => setTimeout(resolve, 10));
    const second = await writeLock(dir, [entry]);
    expect(second.generatedAt).toBe(first.generatedAt);
    // 落盘内容逐字节一致（无谓 diff 消除）
    const onDisk = JSON.parse(await readFile(path.join(dir, 'spark-libraries.lock.json'), 'utf8'));
    expect(onDisk.generatedAt).toBe(first.generatedAt);
    const third = await writeLock(dir, [{ ...entry, hash: 'b'.repeat(64) }]);
    expect(third.generatedAt).not.toBe(first.generatedAt);
  });
});

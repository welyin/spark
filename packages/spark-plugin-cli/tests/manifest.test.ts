import { describe, expect, it } from 'vitest';
import {
  ManifestError,
  isValidCommit,
  isValidHash,
  normalizeRepoId,
  validateLibraryEntry,
  validateManifestExtensions
} from '../src/manifest.js';

const VALID_COMMIT = '0123456789abcdef0123456789abcdef01234567';
const VALID_HASH = 'a'.repeat(64);

describe('normalizeRepoId（plugin-dist §1.2 同序）', () => {
  it('剥 https 前缀 / 尾斜杠 / .git / 转小写', () => {
    expect(normalizeRepoId('HTTPS://GitHub.com/Owner/Repo.git/')).toBe('github.com/owner/repo');
    expect(normalizeRepoId('github.com/acme/spark-kanban-lib')).toBe('github.com/acme/spark-kanban-lib');
  });

  it('保留 monorepo sub-path', () => {
    expect(normalizeRepoId('github.com/acme/mono/libs/kanban')).toBe('github.com/acme/mono/libs/kanban');
  });

  it('拒绝非法 host / 空段 / 越界字符 / . 与 .. 段', () => {
    for (const bad of [
      'npmjs.com/acme/lib',
      'github.com/acme',
      'github.com//lib',
      'github.com/acme/lib%20x',
      'github.com/acme/../lib',
      'github.com/acme/lib?x=1'
    ]) {
      expect(() => normalizeRepoId(bad), bad).toThrowError(ManifestError);
    }
  });
});

describe('validateLibraryEntry', () => {
  it('接受合法条目并规范化 repo', () => {
    const entry = validateLibraryEntry(
      { repo: 'GitHub.com/Acme/Lib.git', commit: VALID_COMMIT, hash: VALID_HASH },
      0
    );
    expect(entry).toEqual({ repo: 'github.com/acme/lib', commit: VALID_COMMIT, hash: VALID_HASH });
  });

  it('拒绝浮动引用（分支/标签/短哈希）', () => {
    for (const ref of ['main', 'v1.0.0', '0123456', 'HEAD']) {
      expect(() => validateLibraryEntry({ repo: 'github.com/a/b', commit: ref, hash: VALID_HASH }, 0))
        .toThrowError(/E_LIB_COMMIT_FLOAT/);
    }
  });

  it('hash 必填且必须 64 位 hex；lock 流程允许暂缺', () => {
    expect(() => validateLibraryEntry({ repo: 'github.com/a/b', commit: VALID_COMMIT }, 0))
      .toThrowError(/E_LIB_HASH/);
    expect(() => validateLibraryEntry({ repo: 'github.com/a/b', commit: VALID_COMMIT, hash: 'xyz' }, 0))
      .toThrowError(/E_LIB_HASH/);
    const entry = validateLibraryEntry(
      { repo: 'github.com/a/b', commit: VALID_COMMIT },
      0,
      { allowMissingHash: true }
    );
    expect(entry.hash).toBe('');
  });
});

describe('validateManifestExtensions', () => {
  it('kind 缺省 app；libraries 缺省空', () => {
    expect(validateManifestExtensions({})).toEqual({ kind: 'app', libraries: [] });
    expect(validateManifestExtensions({ kind: 'library' }).kind).toBe('library');
  });

  it('拒绝非法 kind / 非数组 libraries / 重复 repo', () => {
    expect(() => validateManifestExtensions({ kind: 'module' })).toThrowError(/E_MANIFEST_KIND/);
    expect(() => validateManifestExtensions({ libraries: 'github.com/a/b' })).toThrowError(/E_LIBRARIES_TYPE/);
    expect(() => validateManifestExtensions({
      libraries: [
        { repo: 'github.com/a/b', commit: VALID_COMMIT, hash: VALID_HASH },
        { repo: 'https://github.com/A/B.git', commit: VALID_COMMIT, hash: VALID_HASH }
      ]
    })).toThrowError(/E_LIB_DUP/);
  });

  it('isValidCommit / isValidHash 边界', () => {
    expect(isValidCommit(VALID_COMMIT)).toBe(true);
    expect(isValidCommit(VALID_COMMIT.toUpperCase())).toBe(false);
    expect(isValidHash(VALID_HASH)).toBe(true);
    expect(isValidHash(VALID_HASH.slice(0, 63))).toBe(false);
  });
});

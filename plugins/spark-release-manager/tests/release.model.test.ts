import { describe, expect, it } from 'vitest';
import {
  buildReleaseCardSummary,
  buildReleaseHistorySummary,
  buildReleaseSignPayload,
  buildRetractionSummary,
  canAppendEvent,
  canManageReleaseConfig,
  canPublishRelease,
  canRetractRelease,
  compareReleasePackage,
  compareVersions,
  deriveReleaseState,
  deriveVersionDistribution,
  filterAuthorizedReleaseEvents,
  hasSignatureMaterial,
  parseUpdateManifest,
  pluginDisplayName,
  releaseEventOperatorSet,
  releaseEventSignContent,
  releaseSignContent,
  selectReleaseBackfillBatch,
  validateArtifacts,
  validateVersion,
  RELEASE_SUMMARY_LIMIT,
  type ReleaseArtifact,
  type ReleaseEvent,
  type ReleaseManagerConfig,
  type ReleaseRecord,
  type VersionReport
} from '../model';

const SHA_A = 'a'.repeat(64);
const SHA_B = 'b'.repeat(64);
const SHA_C = 'c'.repeat(64);

const mkArtifacts = (overrides: Partial<ReleaseArtifact> = {}): ReleaseArtifact[] => [
  { kind: 'package', fileName: 'spark-plugin-foo-0.1.0.spkg', sha256: SHA_A, size: 1024, ...overrides }
];

const mkConfig = (publisherRootIds: string[] = ['root-pub']): ReleaseManagerConfig => ({
  orgId: 'org-1',
  publisherRootIds,
  createdBy: 'root-admin',
  createdAt: 1,
  updatedAt: 1
});

const mkEvent = (overrides: Partial<ReleaseEvent> = {}): ReleaseEvent => ({
  id: 'evt_1',
  orgId: 'org-1',
  releaseId: 'rel-1',
  type: 'verified',
  operatorRootId: 'root-pub',
  at: 100,
  ...overrides
});

describe('spark-release-manager model · update-manifest 解析', () => {
  it('parses CI update-manifest.json into artifacts（build-plugin-package.mjs 线形）', () => {
    const raw = JSON.stringify({
      pluginId: 'spark-foo',
      domain: 'plugin:spark-foo',
      manifestVersion: 1,
      version: '0.1.0',
      releaseTime: '2026-10-09T00:00:00.000Z',
      assets: [
        { kind: 'package', fileName: 'spark-plugin-spark-foo-0.1.0.spkg', url: 'https://example.com/x.spkg', sha256: SHA_A, size: 1024 },
        { kind: 'sbom', fileName: 'sbom.json', sha256: SHA_B, size: 256 }
      ]
    });
    const { manifest, artifacts } = parseUpdateManifest(raw);
    expect(manifest.version).toBe('0.1.0');
    expect(artifacts).toHaveLength(2);
    expect(artifacts[0]).toMatchObject({ kind: 'package', sha256: SHA_A, size: 1024, url: 'https://example.com/x.spkg' });
    expect(artifacts[1]).toMatchObject({ kind: 'sbom', sha256: SHA_B });
  });

  it('rejects malformed manifest with original reason（不替发布者遮掩）', () => {
    expect(() => parseUpdateManifest('{not json')).toThrow(/不是合法 JSON/);
    expect(() => parseUpdateManifest('{"assets":[]}')).toThrow(/缺少 assets/);
    expect(() =>
      parseUpdateManifest(JSON.stringify({ assets: [{ kind: 'package', fileName: 'a.spkg', sha256: 'xyz', size: 1 }] }))
    ).toThrow(/sha256 非法/);
    expect(() =>
      parseUpdateManifest(JSON.stringify({ assets: [{ kind: 'package', fileName: 'a.spkg', sha256: SHA_A, size: -1 }] }))
    ).toThrow(/size 非法/);
  });

  it('missing kind no longer defaults to package（多 package 伪造面收口：仅 .spkg 可推断）', () => {
    // 非 .spkg 缺省 kind → 报错，不静默回落 'package'
    expect(() =>
      parseUpdateManifest(JSON.stringify({ assets: [{ fileName: 'sbom.json', sha256: SHA_B, size: 256 }] }))
    ).toThrow(/缺少 kind/);
    // .spkg 缺省 kind → 推断为 package
    const { artifacts } = parseUpdateManifest(
      JSON.stringify({ assets: [{ fileName: 'x-0.1.0.spkg', sha256: SHA_A, size: 1024 }] })
    );
    expect(artifacts[0].kind).toBe('package');
  });
});

describe('spark-release-manager model · 校验', () => {
  it('validateVersion enforces semver shape', () => {
    expect(validateVersion('0.1.0').ok).toBe(true);
    expect(validateVersion('1.2.3-rc.1').ok).toBe(true);
    expect(validateVersion('').ok).toBe(false);
    expect(validateVersion('v1').ok).toBe(false);
    expect(validateVersion('1.0').ok).toBe(false);
  });

  it('validateArtifacts requires a package asset and well-formed fields', () => {
    expect(validateArtifacts(mkArtifacts()).ok).toBe(true);
    expect(validateArtifacts([]).ok).toBe(false);
    expect(validateArtifacts([{ kind: 'sig', fileName: 'x.sig', sha256: SHA_A, size: 64 }]).ok).toBe(false);
    expect(validateArtifacts(mkArtifacts({ sha256: 'AB'.repeat(32) })).ok).toBe(false);
    expect(validateArtifacts(mkArtifacts({ size: 0 })).ok).toBe(false);
  });

  it('validateArtifacts enforces exactly one package asset and unique fileName（多包资产伪造面收口）', () => {
    // 多条 kind=package → 拒绝（第二条起永不参与复算比对的豁口）
    const multiPackage = validateArtifacts([
      { kind: 'package', fileName: 'a.spkg', sha256: SHA_A, size: 1024 },
      { kind: 'package', fileName: 'b.spkg', sha256: SHA_B, size: 2048 }
    ]);
    expect(multiPackage.ok).toBe(false);
    expect(multiPackage.reason).toContain('恰好一条');
    // fileName 重复 → 拒绝（同名资产会让其中一条不参与双重核对）
    const dupName = validateArtifacts([
      { kind: 'package', fileName: 'a.spkg', sha256: SHA_A, size: 1024 },
      { kind: 'sig', fileName: 'a.spkg', sha256: SHA_B, size: 88 }
    ]);
    expect(dupName.ok).toBe(false);
    expect(dupName.reason).toContain('fileName 重复');
    // package + sig/pubkey/checksums 正常登记仍通过
    expect(
      validateArtifacts([
        { kind: 'package', fileName: 'a.spkg', sha256: SHA_A, size: 1024 },
        { kind: 'sig', fileName: 'a.sig', sha256: SHA_B, size: 88 },
        { kind: 'pubkey', fileName: 'a.pub.pem', sha256: SHA_C, size: 113 }
      ]).ok
    ).toBe(true);
  });

  it('hasSignatureMaterial requires both sig and pubkey assets', () => {
    expect(hasSignatureMaterial(mkArtifacts())).toBe(false);
    expect(
      hasSignatureMaterial([
        ...mkArtifacts(),
        { kind: 'sig', fileName: 'm.sig', sha256: SHA_B, size: 88 },
        { kind: 'pubkey', fileName: 'm.pub.pem', sha256: SHA_C, size: 113 }
      ])
    ).toBe(true);
  });
});

describe('spark-release-manager model · 本机导入复算三方比对（档一-6）', () => {
  const release = {
    pluginId: 'spark-foo',
    version: '0.1.0',
    artifacts: mkArtifacts(),
    updateManifest: {
      pluginId: 'spark-foo',
      version: '0.1.0',
      assets: [{ kind: 'package', fileName: 'spark-plugin-foo-0.1.0.spkg', sha256: SHA_A, size: 1024 }]
    }
  };
  const recomputed = { sha256: SHA_A, size: 1024, pluginId: 'spark-foo', version: '0.1.0' };

  it('passes when registered values, manifest and recompute all agree', () => {
    expect(compareReleasePackage(release, recomputed)).toEqual([]);
  });

  it('flags tampered package hash（验收①：任一资产哈希被篡改必判失败）', () => {
    const mismatches = compareReleasePackage(release, { ...recomputed, sha256: SHA_B });
    expect(mismatches.length).toBeGreaterThan(0);
    expect(mismatches.join('\n')).toContain('包哈希不一致');
    expect(mismatches.join('\n')).toContain(SHA_B);
  });

  it('flags size mismatch and plugin/version mismatch（张冠李戴防护）', () => {
    expect(compareReleasePackage(release, { ...recomputed, size: 2048 }).join('\n')).toContain('包大小不一致');
    expect(compareReleasePackage(release, { ...recomputed, pluginId: 'spark-bar' }).join('\n')).toContain('不一致');
    expect(compareReleasePackage(release, { ...recomputed, version: '0.2.0' }).join('\n')).toContain('不一致');
  });

  it('flags registered-vs-manifest double-check divergence（登记值不是新信任源）', () => {
    const diverged = {
      ...release,
      updateManifest: {
        pluginId: 'spark-foo',
        version: '0.1.0',
        assets: [{ kind: 'package', fileName: 'spark-plugin-foo-0.1.0.spkg', sha256: SHA_C, size: 1024 }]
      }
    };
    const mismatches = compareReleasePackage(diverged, recomputed);
    expect(mismatches.join('\n')).toContain('双重核对不一致');
  });

  it('flags manifest assets missing from the registered artifact list', () => {
    const missing = {
      ...release,
      updateManifest: {
        pluginId: 'spark-foo',
        version: '0.1.0',
        assets: [{ kind: 'sbom', fileName: 'sbom.json', sha256: SHA_B, size: 256 }]
      }
    };
    expect(compareReleasePackage(missing, recomputed).join('\n')).toContain('未在发布单资产清单中登记');
  });

  it('compares every package asset against the recomputed value（逐资产比对，第二条不豁免）', () => {
    // 历史/旁路写入的发布单若含多条 package，每条登记哈希都必须比对——
    // 「第一条真哈希 + 第二条任意哈希」不得核验通过
    const forged = {
      pluginId: 'spark-foo',
      version: '0.1.0',
      artifacts: [
        { kind: 'package', fileName: 'spark-plugin-foo-0.1.0.spkg', sha256: SHA_A, size: 1024 },
        { kind: 'package', fileName: 'spark-plugin-foo-evil.spkg', sha256: SHA_C, size: 9999 }
      ]
    };
    const mismatches = compareReleasePackage(forged, recomputed);
    expect(mismatches.join('\n')).toContain('资产「spark-plugin-foo-evil.spkg」哈希不一致');
    expect(mismatches.join('\n')).toContain(SHA_C);
    // 全量一致时逐条比对不误报
    const honest = {
      ...forged,
      artifacts: [
        { kind: 'package', fileName: 'spark-plugin-foo-0.1.0.spkg', sha256: SHA_A, size: 1024 },
        { kind: 'package', fileName: 'spark-plugin-foo-copy.spkg', sha256: SHA_A, size: 1024 }
      ]
    };
    expect(compareReleasePackage(honest, recomputed)).toEqual([]);
  });
});

describe('spark-release-manager model · 状态机派生与写侧守卫', () => {
  it('derives five states from the event stream（事件流派生，不改原发布单）', () => {
    expect(deriveReleaseState('rel-1', [])).toBe('registered');
    expect(deriveReleaseState('rel-1', [mkEvent()])).toBe('verified');
    expect(deriveReleaseState('rel-1', [mkEvent(), mkEvent({ id: 'evt_2', type: 'published', at: 200 })])).toBe('published');
    expect(deriveReleaseState('rel-1', [mkEvent({ type: 'verify-failed' })])).toBe('verify-failed');
    expect(
      deriveReleaseState('rel-1', [
        mkEvent(),
        mkEvent({ id: 'evt_2', type: 'published', at: 200 }),
        mkEvent({ id: 'evt_3', type: 'retracted', at: 300 })
      ])
    ).toBe('retracted');
  });

  it('channel-pushed does not change release state（回看最近状态事件）', () => {
    const events = [
      mkEvent(),
      mkEvent({ id: 'evt_2', type: 'published', at: 200 }),
      mkEvent({ id: 'evt_3', type: 'channel-pushed', channelId: 'chan-1', at: 300 })
    ];
    expect(deriveReleaseState('rel-1', events)).toBe('published');
  });

  it('deterministic ordering ties break by id（跨设备时钟不齐各端一致）', () => {
    const events = [
      mkEvent({ id: 'evt_b', type: 'verify-failed', at: 100 }),
      mkEvent({ id: 'evt_a', type: 'verified', at: 100 })
    ];
    // 同一时刻：id 字典序 evt_b 更大 → 最新为 verify-failed
    expect(deriveReleaseState('rel-1', events)).toBe('verify-failed');
  });

  it('write-side guard enforces the orchestration flow（核验未过不得发布）', () => {
    expect(canAppendEvent('registered', 'verified').ok).toBe(true);
    expect(canAppendEvent('registered', 'published').ok).toBe(false);
    expect(canAppendEvent('verified', 'published').ok).toBe(true);
    expect(canAppendEvent('verified', 'verified').ok).toBe(false);
    expect(canAppendEvent('verify-failed', 'verified').ok).toBe(true);
    expect(canAppendEvent('published', 'channel-pushed').ok).toBe(true);
    expect(canAppendEvent('verified', 'channel-pushed').ok).toBe(false);
    expect(canAppendEvent('published', 'retracted').ok).toBe(true);
    expect(canAppendEvent('retracted', 'retracted').ok).toBe(false);
    expect(canAppendEvent('retracted', 'verified').ok).toBe(false);
  });
});

describe('spark-release-manager model · 事件读侧鉴权（伪造状态事件 fail-closed）', () => {
  it('filters forged events from operators outside the authorized set（非法操作者事件不参与派生）', () => {
    const forged = mkEvent({ id: 'evt_forge', type: 'published', operatorRootId: 'root-attacker', at: 200 });
    const events = [mkEvent(), forged];
    const operators = releaseEventOperatorSet(mkConfig(), []);
    const filtered = filterAuthorizedReleaseEvents(events, operators);
    // 伪造 published 被滤除：状态停在 verified，不会被伪造事件推进
    expect(filtered).toHaveLength(1);
    expect(deriveReleaseState('rel-1', filtered)).toBe('verified');
    // 未过滤时会误判已发布（对照，证明过滤的必要性）
    expect(deriveReleaseState('rel-1', events)).toBe('published');
  });

  it('authorized set = publisherRootIds ∪ roster admins（管理员撤回可派生）', () => {
    const operators = releaseEventOperatorSet(mkConfig(['root-pub']), ['root-admin2']);
    expect(operators.has('root-pub')).toBe(true);
    expect(operators.has('root-admin2')).toBe(true);
    const adminRetract = mkEvent({ id: 'evt_r', type: 'retracted', operatorRootId: 'root-admin2', at: 200 });
    expect(deriveReleaseState('rel-1', filterAuthorizedReleaseEvents([mkEvent(), adminRetract], operators))).toBe('retracted');
  });

  it('missing config fails closed：empty operator set ignores all events（宁可不派生）', () => {
    const operators = releaseEventOperatorSet(null, ['root-admin2']);
    expect(operators.size).toBe(0);
    const events = [mkEvent(), mkEvent({ id: 'evt_2', type: 'published', at: 200 })];
    expect(filterAuthorizedReleaseEvents(events, operators)).toHaveLength(0);
    expect(deriveReleaseState('rel-1', filterAuthorizedReleaseEvents(events, operators))).toBe('registered');
  });
});

describe('spark-release-manager model · 权限（业务层校验，fail-closed）', () => {
  it('publisher right requires initialized config and membership', () => {
    expect(canPublishRelease(null, 'root-pub')).toBe(false);
    expect(canPublishRelease(mkConfig(), 'root-pub')).toBe(true);
    expect(canPublishRelease(mkConfig(), 'root-other')).toBe(false);
    expect(canPublishRelease(mkConfig(), null)).toBe(false);
  });

  it('config management is roster-admin only（档三-23）', () => {
    expect(canManageReleaseConfig('admin')).toBe(true);
    expect(canManageReleaseConfig('member')).toBe(false);
    expect(canManageReleaseConfig(null)).toBe(false);
  });

  it('retract right = publisher set member or roster admin（档三-26）', () => {
    expect(canRetractRelease(mkConfig(), 'root-pub', 'member')).toBe(true);
    expect(canRetractRelease(mkConfig(), 'root-other', 'admin')).toBe(true);
    expect(canRetractRelease(mkConfig(), 'root-other', 'member')).toBe(false);
  });
});

describe('spark-release-manager model · 版本分布骨架（档三-25）', () => {
  const mkReport = (overrides: Partial<VersionReport> = {}): VersionReport => ({
    id: 'vrpt_1',
    orgId: 'org-1',
    pluginId: 'spark-foo',
    version: '0.1.0',
    trust: 'signed',
    reporterRootId: 'root-a',
    at: 100,
    ...overrides
  });

  it('takes the latest report per reporter and counts distribution', () => {
    const reports = [
      mkReport({ version: '0.1.0', at: 100 }),
      mkReport({ id: 'vrpt_2', version: '0.2.0', at: 200 }), // 同一成员更新上报
      mkReport({ id: 'vrpt_3', reporterRootId: 'root-b', version: '0.1.0', trust: 'sideloaded' })
    ];
    const dist = deriveVersionDistribution('spark-foo', reports);
    expect(dist.reporterCount).toBe(2);
    expect(dist.latestVersion).toBe('0.2.0');
    expect(dist.versions).toEqual([
      { version: '0.2.0', count: 1 },
      { version: '0.1.0', count: 1 }
    ]);
    expect(dist.behindCount).toBe(1);
    expect(dist.trusts).toContainEqual({ trust: 'sideloaded', count: 1 });
  });

  it('compareVersions orders semver numerically with prerelease rule', () => {
    expect(compareVersions('0.2.0', '0.10.0')).toBeLessThan(0);
    expect(compareVersions('1.0.0', '1.0.0-rc.1')).toBeGreaterThan(0);
    expect(compareVersions('1.0.0', '1.0.0')).toBe(0);
  });
});

describe('spark-release-manager model · summary 纪律与签名载荷', () => {
  it('release card summary is self-contained and within 200 chars（未装插件原生降级渲染）', () => {
    const summary = buildReleaseCardSummary({
      pluginId: 'https://github.com/org/spark-foo',
      version: '0.1.0',
      changelog: '修复若干问题'.repeat(100)
    });
    expect([...summary].length).toBeLessThanOrEqual(RELEASE_SUMMARY_LIMIT);
    expect(summary).toContain('【版本发布·spark-foo v0.1.0】');
  });

  it('retraction summary falls back to honest guidance when reason is absent（档三-26）', () => {
    const summary = buildRetractionSummary({ pluginId: 'spark-foo', version: '0.1.0' });
    expect(summary).toContain('不会被回滚');
    expect([...summary].length).toBeLessThanOrEqual(RELEASE_SUMMARY_LIMIT);
  });

  it('history summary states the count', () => {
    expect(buildReleaseHistorySummary(7)).toContain('7');
  });

  it('pluginDisplayName takes the repo tail', () => {
    expect(pluginDisplayName('https://github.com/org/spark-foo.git')).toBe('spark-foo');
    expect(pluginDisplayName('spark-foo')).toBe('spark-foo');
  });

  it('sign payload binds org/record/operator/content（四元绑定）', () => {
    const payload = buildReleaseSignPayload('org-1', 'rel-1', 'root-pub', 'content');
    expect(payload.startsWith('org-1:rel-1:root-pub:')).toBe(true);
    expect(buildReleaseSignPayload('org-1', 'rel-1', 'root-pub', 'content')).toBe(payload);
    expect(buildReleaseSignPayload('org-1', 'rel-1', 'root-pub', 'other')).not.toBe(payload);
  });

  it('release sign content covers artifacts so hash tampering breaks verification', () => {
    const base = { pluginId: 'spark-foo', version: '0.1.0', artifacts: mkArtifacts(), channels: [] };
    expect(releaseSignContent(base)).not.toBe(releaseSignContent({ ...base, artifacts: mkArtifacts({ sha256: SHA_B }) }));
    expect(releaseEventSignContent({ releaseId: 'rel-1', type: 'published' })).toContain('published');
  });
});

describe('spark-release-manager model · 补发节流（档二-4 MVP 降级）', () => {
  const mkRelease = (id: string): ReleaseRecord => ({
    id,
    orgId: 'org-1',
    pluginId: 'spark-foo',
    version: '0.1.0',
    artifacts: mkArtifacts(),
    channels: [],
    publisherRootId: 'root-pub',
    createdAt: 1
  });

  it('below threshold sends cards for all pending', () => {
    const batch = selectReleaseBackfillBatch([mkRelease('a'), mkRelease('b')]);
    expect(batch.cards).toHaveLength(2);
    expect(batch.summarizedCount).toBe(0);
  });

  it('above threshold keeps only the latest card plus a summary message', () => {
    const pending = ['a', 'b', 'c', 'd', 'e', 'f', 'g'].map(mkRelease);
    const batch = selectReleaseBackfillBatch(pending);
    expect(batch.cards.map((release) => release.id)).toEqual(['g']);
    expect(batch.summarizedCount).toBe(6);
    expect(batch.summarized).toHaveLength(6);
  });
});

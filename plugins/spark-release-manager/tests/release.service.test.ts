import { describe, expect, it, vi } from 'vitest';
import { ReleaseManagerService, RELEASE_COLLECTIONS } from '../service';
import {
  deriveReleaseState,
  filterAuthorizedReleaseEvents,
  hasSignatureMaterial,
  releaseIdOf,
  type ReleaseManagerConfig,
  type ReleaseRecord
} from '../model';

/**
 * mock SDK：docs 为内存集合后端（defineCollection/put/get/query，按声明 enforce
 * append-only——对齐内核 plugindocs 口径，防 mock 过松掩盖违规调用）；data 为
 * 内存 local 台账；identity/market/messages/evidence 按已落地 SDK 面实现
 * （钉住真实契约）。
 */
const SHA_A = 'a'.repeat(64);
const SHA_B = 'b'.repeat(64);
const PLUGIN_PK = 'cGsx';
const PUBLISHER = 'f'.repeat(64);
const ADMIN = 'e'.repeat(64);

type MockOptions = {
  withMarket?: boolean;
  signError?: boolean;
  /** inspectLocal 复算结果（缺省与登记包资产一致） */
  recomputedSha256?: string;
  recomputedSize?: number;
  messageError?: boolean;
};

function createMockSdk(options: MockOptions = {}) {
  const docsStore = new Map<string, Record<string, unknown>>();
  const collectionSchemas = new Map<string, { syncStrategy: string; governance?: boolean }>();
  const dataStore = new Map<string, unknown>();
  const sentMessages: Array<{ payload: Record<string, unknown>; card?: unknown }> = [];

  const sdk = {
    domain: 'plugin:spark-release-manager',
    docs: {
      defineCollection: vi.fn().mockImplementation(async (collection: string, schema: { syncStrategy: string; governance?: boolean }) => {
        collectionSchemas.set(collection, schema);
        return { collection, syncStrategy: schema.syncStrategy, governance: schema.governance ?? false, enableEvidence: true };
      }),
      put: vi.fn().mockImplementation(async (collection: string, id: string, doc: Record<string, unknown>) => {
        const schema = collectionSchemas.get(collection);
        if (schema?.syncStrategy === 'append-only' && docsStore.has(`${collection}/${id}`)) {
          throw new Error(`AppendOnlyViolation: put-overwrite rejected on ${collection}（mock enforce）`);
        }
        docsStore.set(`${collection}/${id}`, doc);
        return { success: true };
      }),
      get: vi.fn().mockImplementation(async (collection: string, id: string) => docsStore.get(`${collection}/${id}`) ?? null),
      delete: vi.fn().mockImplementation(async (collection: string, id: string) => {
        const schema = collectionSchemas.get(collection);
        if (schema?.syncStrategy === 'append-only') {
          throw new Error(`AppendOnlyViolation: delete rejected on ${collection}（mock enforce）`);
        }
        docsStore.delete(`${collection}/${id}`);
        return { success: true };
      }),
      query: vi.fn().mockImplementation(async (collection: string, opts?: { filter?: Array<{ field: string; value: unknown }> }) => ({
        items: [...docsStore.entries()]
          .filter(([key]) => key.startsWith(`${collection}/`))
          .map(([key, data]) => ({ id: key.slice(collection.length + 1), data }))
          .filter((item) =>
            (opts?.filter ?? []).every(
              (f) => (item.data as Record<string, unknown>)[f.field] === f.value
            )
          )
      }))
    },
    data: {
      declareCollection: vi.fn().mockResolvedValue({}),
      save: vi.fn().mockImplementation(async (name: string, key: string, value: unknown) => {
        dataStore.set(`${name}/${key}`, value);
        return { success: true };
      }),
      get: vi.fn().mockImplementation(async (name: string, key: string) => dataStore.get(`${name}/${key}`) ?? null),
      query: vi.fn().mockImplementation(async (name: string, opts?: { prefix?: string }) => ({
        items: [...dataStore.entries()]
          .filter(([key]) => key.startsWith(`${name}/${opts?.prefix ?? ''}`))
          .map(([key, value]) => ({ key: key.slice(name.length + 1), value }))
      }))
    },
    identity: {
      sign: vi.fn().mockImplementation(async (payload: string) => {
        if (options.signError) {
          throw new Error('用户拒绝签名授权');
        }
        return { domain: 'plugin:spark-release-manager', domainId: 'spark-release-manager', publicKey: PLUGIN_PK, signature: 'sig-1', payloadHash: 'h' };
      }),
      verify: vi.fn().mockResolvedValue({ valid: true })
    },
    market:
      options.withMarket === false
        ? undefined
        : {
            inspectLocal: vi.fn().mockImplementation(async (path: string) => ({
              pluginId: 'spark-foo',
              domain: 'plugin:spark-foo',
              version: '0.1.0',
              name: 'Foo',
              permissions: [],
              sha256: options.recomputedSha256 ?? SHA_A,
              size: options.recomputedSize ?? 1024,
              fileName: path.split(/[\\/]/).pop()
            })),
            list: vi.fn().mockResolvedValue([]),
            checkUpdates: vi.fn().mockResolvedValue([]),
            pickSpkg: vi.fn().mockResolvedValue('/tmp/x.spkg')
          },
    messages: {
      sendAppMessage: vi.fn().mockImplementation(async (payload: Record<string, unknown>, card?: unknown) => {
        if (options.messageError) {
          throw new Error('rate-limited');
        }
        sentMessages.push({ payload, card });
        return { id: `m-${sentMessages.length}` };
      }),
      onCardAction: vi.fn()
    },
    evidence: {
      headHash: vi.fn().mockResolvedValue({ hash: 'ab'.repeat(32) }),
      verify: vi.fn().mockResolvedValue({ valid: true, height: 42 })
    }
  } as any;

  return { sdk, docsStore, collectionSchemas, dataStore, sentMessages };
}

const mkConfig = (publisherRootIds: string[] = [PUBLISHER]): ReleaseManagerConfig => ({
  orgId: 'org-1',
  publisherRootIds,
  createdBy: ADMIN,
  createdAt: 1,
  updatedAt: 1
});

const REGISTER_INPUT = {
  pluginId: 'spark-foo',
  version: '0.1.0',
  updateManifestJson: JSON.stringify({
    pluginId: 'spark-foo',
    version: '0.1.0',
    assets: [{ kind: 'package', fileName: 'spark-plugin-spark-foo-0.1.0.spkg', sha256: SHA_A, size: 1024 }]
  }),
  changelog: '首个登记版本'
};

async function registerFixture(sdk: any, config = mkConfig()) {
  const service = new ReleaseManagerService(sdk);
  const release = await service.registerRelease('org-1', PUBLISHER, REGISTER_INPUT, config);
  return { service, release };
}

describe('spark-release-manager service · 集合声明与发布单登记', () => {
  it('declares governance append-only for releases/events before writing（包哈希入存证链的挂接点）', async () => {
    const { sdk, collectionSchemas } = createMockSdk();
    await registerFixture(sdk);

    expect(collectionSchemas.get(RELEASE_COLLECTIONS.releases)).toEqual({ syncStrategy: 'append-only', governance: true });
    expect(collectionSchemas.get(RELEASE_COLLECTIONS.events)).toEqual({ syncStrategy: 'append-only', governance: true });
    expect(collectionSchemas.get(RELEASE_COLLECTIONS.reports)).toEqual({ syncStrategy: 'append-only' });
    expect(collectionSchemas.get(RELEASE_COLLECTIONS.channels)).toEqual({ syncStrategy: 'lww' });
    expect(collectionSchemas.get(RELEASE_COLLECTIONS.config)).toEqual({ syncStrategy: 'lww' });
    // 声明幂等：第二次登记不重复声明
    const service = new ReleaseManagerService(sdk);
    await service.registerRelease('org-1', PUBLISHER, { ...REGISTER_INPUT, version: '0.2.0' }, mkConfig());
    expect(sdk.docs.defineCollection).toHaveBeenCalledTimes(10); // 两个实例各 5 条（实例级幂等）
  });

  it('registers a release with parsed manifest artifacts and publisher signature（防抵赖）', async () => {
    const { sdk } = createMockSdk();
    const { release } = await registerFixture(sdk);

    expect(release.pluginId).toBe('spark-foo');
    expect(release.version).toBe('0.1.0');
    expect(release.artifacts).toHaveLength(1);
    expect(release.artifacts[0]).toMatchObject({ kind: 'package', sha256: SHA_A, size: 1024 });
    expect(release.signature?.payload).toContain(`org-1:${release.id}:${PUBLISHER}:`);
    expect(release.publisherRootId).toBe(PUBLISHER);
  });

  it('fail-closed without publisher-right config（验收⑤：非发布权集合成员无登记路径）', async () => {
    const { sdk } = createMockSdk();
    const service = new ReleaseManagerService(sdk);
    await expect(service.registerRelease('org-1', PUBLISHER, REGISTER_INPUT, null)).rejects.toThrow(/发布权集合成员/);
    await expect(service.registerRelease('org-1', 'x'.repeat(64), REGISTER_INPUT, mkConfig())).rejects.toThrow(/发布权集合成员/);
  });

  it('rejects duplicate (org, plugin, version) registration（releaseRef/版本号幂等键）', async () => {
    const { sdk } = createMockSdk();
    const { service } = await registerFixture(sdk);
    await expect(service.registerRelease('org-1', PUBLISHER, REGISTER_INPUT, mkConfig())).rejects.toThrow(/幂等键/);
    // 同插件不同版本可登记
    await expect(service.registerRelease('org-1', PUBLISHER, { ...REGISTER_INPUT, version: '0.2.0' }, mkConfig())).resolves.toBeTruthy();
  });

  it('derives release id deterministically from (org, plugin, version)（竞态/跨设备窗口由 append-only 拒重兜底）', async () => {
    const { sdk } = createMockSdk();
    const { release } = await registerFixture(sdk);

    expect(release.id).toBe(releaseIdOf('org-1', 'spark-foo', '0.1.0'));
    expect(release.id).toMatch(/^rel_[0-9a-f]{64}$/);
    // 任一字段不同 → 不同 id
    expect(releaseIdOf('org-1', 'spark-foo', '0.2.0')).not.toBe(release.id);
    expect(releaseIdOf('org-2', 'spark-foo', '0.1.0')).not.toBe(release.id);
    expect(releaseIdOf('org-1', 'spark-bar', '0.1.0')).not.toBe(release.id);
  });

  it('merges extraArtifacts with manifest-parsed assets（签名材料手工补登随发布单入链）', async () => {
    const { sdk } = createMockSdk();
    const service = new ReleaseManagerService(sdk);
    const release = await service.registerRelease(
      'org-1',
      PUBLISHER,
      {
        ...REGISTER_INPUT,
        extraArtifacts: [
          { kind: 'sig', fileName: 'spark-plugin-spark-foo-0.1.0.spkg.sig', sha256: SHA_B, size: 128 },
          { kind: 'pubkey', fileName: 'spark-plugin-spark-foo-0.1.0.pub.pem', sha256: SHA_B, size: 256 }
        ]
      },
      mkConfig()
    );

    expect(release.artifacts).toHaveLength(3);
    expect(release.artifacts[1]).toMatchObject({ kind: 'sig', fileName: 'spark-plugin-spark-foo-0.1.0.spkg.sig' });
    expect(hasSignatureMaterial(release.artifacts)).toBe(true);
    // updateManifest 原文仍随发布单留存（核验时双向比对）
    expect(release.updateManifest?.version).toBe('0.1.0');
    // extraArtifacts 与 manifest 资产同受 validateArtifacts 把关（fileName 撞车即拒）
    await expect(
      service.registerRelease(
        'org-1',
        PUBLISHER,
        {
          ...REGISTER_INPUT,
          version: '0.3.0',
          extraArtifacts: [{ kind: 'signature', fileName: 'spark-plugin-spark-foo-0.1.0.spkg', sha256: SHA_B, size: 128 }]
        },
        mkConfig()
      )
    ).rejects.toThrow(/fileName 重复/);
  });

  it('degrades to unsigned record when identity:sign is refused（不阻断主流程）', async () => {
    const { sdk } = createMockSdk({ signError: true });
    const { release } = await registerFixture(sdk);
    expect(release.signature).toBeUndefined();
  });

  it('config save is roster-admin only and rejects empty publisher set', async () => {
    const { sdk } = createMockSdk();
    const service = new ReleaseManagerService(sdk);
    await expect(service.saveConfig('org-1', ADMIN, { publisherRootIds: [PUBLISHER] }, 'member')).rejects.toThrow(/管理员/);
    await expect(service.saveConfig('org-1', ADMIN, { publisherRootIds: [] }, 'admin')).rejects.toThrow(/不能为空/);
    const config = await service.saveConfig('org-1', ADMIN, { publisherRootIds: [PUBLISHER, PUBLISHER] }, 'admin');
    expect(config.publisherRootIds).toEqual([PUBLISHER]);
  });
});

describe('spark-release-manager service · 核验编排（档一-6 本机导入复算）', () => {
  it('rejects verify orchestration on mobile at service layer（档二-8 只做登记，不依赖视图层隐藏入口）', async () => {
    const { sdk } = createMockSdk();
    const service = new ReleaseManagerService(sdk, undefined, { platform: 'android' });
    const release = await service.registerRelease('org-1', PUBLISHER, REGISTER_INPUT, mkConfig());
    await expect(
      service.verifyRelease('org-1', PUBLISHER, release.id, { spkgPath: '/tmp/pkg.spkg' }, mkConfig())
    ).rejects.toThrow(/只做登记/);
  });

  it('appends verified event when recompute matches（核验证据入链）', async () => {
    const { sdk } = createMockSdk();
    const { service, release } = await registerFixture(sdk);
    const event = await service.verifyRelease('org-1', PUBLISHER, release.id, { spkgPath: '/tmp/pkg.spkg' }, mkConfig());

    expect(event.type).toBe('verified');
    expect(event.verification?.recomputed?.sha256).toBe(SHA_A);
    expect(event.verification?.mismatches).toEqual([]);
    expect(event.verification?.trustNote).toContain('只验不签');
    expect(event.verification?.manifestAssetsChecked).toBe(1);
    const events = await service.loadEvents('org-1');
    expect(deriveReleaseState(release.id, events)).toBe('verified');
  });

  it('appends verify-failed with original reason when hash was tampered（验收①：篡改必判失败且原因原样可见）', async () => {
    const { sdk } = createMockSdk({ recomputedSha256: SHA_B });
    const { service, release } = await registerFixture(sdk);
    const event = await service.verifyRelease('org-1', PUBLISHER, release.id, { spkgPath: '/tmp/pkg.spkg' }, mkConfig());

    expect(event.type).toBe('verify-failed');
    expect(event.reason).toContain('包哈希不一致');
    expect(event.reason).toContain(SHA_B);
    const events = await service.loadEvents('org-1');
    expect(deriveReleaseState(release.id, events)).toBe('verify-failed');
    // 失败态可复核重验
    sdk.market.inspectLocal.mockResolvedValue({
      pluginId: 'spark-foo',
      version: '0.1.0',
      sha256: SHA_A,
      size: 1024,
      fileName: 'pkg.spkg'
    });
    const retry = await service.verifyRelease('org-1', PUBLISHER, release.id, { spkgPath: '/tmp/pkg.spkg' }, mkConfig());
    expect(retry.type).toBe('verified');
  });

  it('fails honestly when market module is absent（档二-8：移动端降级只做登记）', async () => {
    const { sdk } = createMockSdk({ withMarket: false });
    const { service, release } = await registerFixture(sdk);
    await expect(
      service.verifyRelease('org-1', PUBLISHER, release.id, { spkgPath: '/tmp/pkg.spkg' }, mkConfig())
    ).rejects.toThrow(/委托桌面端成员核验/);
  });
});

describe('spark-release-manager service · 状态推进与版本卡片（档一-2 唯一推送源）', () => {
  it('enforces no-publish-before-verified（业务层硬约束）', async () => {
    const { sdk } = createMockSdk();
    const { service, release } = await registerFixture(sdk);
    await expect(service.publishRelease('org-1', PUBLISHER, release.id, mkConfig())).rejects.toThrow(/核验未通过/);
  });

  it('publishRelease appends published event and pushes the version card with releaseRef', async () => {
    const { sdk, sentMessages } = createMockSdk();
    const { service, release } = await registerFixture(sdk);
    await service.verifyRelease('org-1', PUBLISHER, release.id, { spkgPath: '/tmp/pkg.spkg' }, mkConfig());
    await service.publishRelease('org-1', PUBLISHER, release.id, mkConfig());

    expect(sentMessages).toHaveLength(1);
    expect(sentMessages[0].payload.releaseRef).toBe(release.id);
    expect(sentMessages[0].payload.summary).toContain('【版本发布·spark-foo v0.1.0】');
    expect([...(sentMessages[0].payload.summary as string)].length).toBeLessThanOrEqual(200);
    expect(sentMessages[0].card).toEqual({ viewId: 'release-card', data: { releaseId: release.id, orgId: 'org-1' } });
    const events = await service.loadEvents('org-1');
    expect(deriveReleaseState(release.id, events)).toBe('published');
  });

  it('member-side backfill is idempotent via releaseRef delivery ledger（重启不重复推卡片）', async () => {
    const { sdk, sentMessages } = createMockSdk();
    const { service, release } = await registerFixture(sdk);
    await service.verifyRelease('org-1', PUBLISHER, release.id, { spkgPath: '/tmp/pkg.spkg' }, mkConfig());
    await service.publishRelease('org-1', PUBLISHER, release.id, mkConfig());
    expect(sentMessages).toHaveLength(1);

    const releases = await service.loadReleases('org-1');
    const events = await service.loadEvents('org-1');
    // 新设备实例（共享同一持久台账后端 = 同一 org 同步面）：补发不再重复
    const memberService = new ReleaseManagerService(sdk);
    const sent = await memberService.notifyNewReleases('org-1', releases, events, new Set([PUBLISHER]));
    expect(sent).toBe(0);
    expect(sentMessages).toHaveLength(1);
  });

  it('backfill on a fresh device generates cards only for published releases（本地生成本地消费）', async () => {
    const { sdk, sentMessages } = createMockSdk();
    const config = mkConfig();
    const publisherService = new ReleaseManagerService(sdk);
    // 两条发布单：一条推进到已发布，一条停在已登记
    const r1 = await publisherService.registerRelease('org-1', PUBLISHER, REGISTER_INPUT, config);
    await publisherService.registerRelease('org-1', PUBLISHER, { ...REGISTER_INPUT, version: '0.2.0' }, config);
    await publisherService.verifyRelease('org-1', PUBLISHER, r1.id, { spkgPath: '/tmp/pkg.spkg' }, config);
    await publisherService.publishRelease('org-1', PUBLISHER, r1.id, config);
    expect(sentMessages).toHaveLength(1);

    // 全新设备（独立持久台账）：只为已发布的补一条卡片
    const { sdk: freshSdk, sentMessages: freshSent } = createMockSdk();
    const memberService = new ReleaseManagerService(freshSdk);
    const releases = await publisherService.loadReleases('org-1');
    const events = await publisherService.loadEvents('org-1');
    const sent = await memberService.notifyNewReleases('org-1', releases, events, new Set([PUBLISHER]));
    expect(sent).toBe(1);
    expect(freshSent).toHaveLength(1);
    expect(freshSent[0].payload.releaseRef).toBe(r1.id);
  });

  it('forged published event from a non-publisher drives no member cards（读侧鉴权 fail-closed）', async () => {
    const { sdk, sentMessages } = createMockSdk();
    const { service, release } = await registerFixture(sdk);
    // 伪造面：自制客户端/库形态直调 sdk.docs.put 写入 published 事件（绕过服务层守卫）
    const forged = {
      id: 'evt_forge_1',
      orgId: 'org-1',
      releaseId: release.id,
      type: 'published',
      operatorRootId: 'b'.repeat(64),
      at: Date.now()
    };
    await sdk.docs.put(RELEASE_COLLECTIONS.events, forged.id, forged);

    const releases = await service.loadReleases('org-1');
    const events = await service.loadEvents('org-1');
    // 伪造事件留痕（append-only 不可删）
    expect(events.some((event) => event.id === 'evt_forge_1')).toBe(true);

    // 全新成员设备：合法操作者集合不含伪造者 → 不派生已发布、不推卡片
    const { sdk: freshSdk, sentMessages: freshSent } = createMockSdk();
    const memberService = new ReleaseManagerService(freshSdk);
    const sent = await memberService.notifyNewReleases('org-1', releases, events, new Set([PUBLISHER]));
    expect(sent).toBe(0);
    expect(freshSent).toHaveLength(0);
    expect(deriveReleaseState(release.id, filterAuthorizedReleaseEvents(events, new Set([PUBLISHER])))).toBe('registered');

    // 配置不可得（null 集合）→ fail-closed，同样不推任何卡片
    const sentClosed = await memberService.notifyNewReleases('org-1', releases, events, null);
    expect(sentClosed).toBe(0);
    expect(freshSent).toHaveLength(0);
    expect(sentMessages).toHaveLength(0);
  });

  it('card push failure degrades without blocking the publish flow（权限/限流降级）', async () => {
    const { sdk } = createMockSdk({ messageError: true });
    const { service, release } = await registerFixture(sdk);
    await service.verifyRelease('org-1', PUBLISHER, release.id, { spkgPath: '/tmp/pkg.spkg' }, mkConfig());
    const event = await service.publishRelease('org-1', PUBLISHER, release.id, mkConfig());
    expect(event.type).toBe('published');
    expect(service.getDeliveryStats().rejectedCount).toBeGreaterThan(0);
  });
});

describe('spark-release-manager service · 渠道登记与跟踪（验收④ 渠道可追责）', () => {
  it('channel push requires published state and a registered channel（append-only 留痕）', async () => {
    const { sdk } = createMockSdk();
    const { service, release } = await registerFixture(sdk);
    const channel = await service.saveChannel(
      'org-1',
      PUBLISHER,
      { kind: 'market-catalog', target: 'builtin:spark-foo' },
      mkConfig(),
      'member'
    );
    // 未发布：拒绝登记渠道推送
    await expect(
      service.recordChannelPush('org-1', PUBLISHER, release.id, { channelId: channel.id }, mkConfig())
    ).rejects.toThrow(/已发布/);

    await service.verifyRelease('org-1', PUBLISHER, release.id, { spkgPath: '/tmp/pkg.spkg' }, mkConfig());
    await service.publishRelease('org-1', PUBLISHER, release.id, mkConfig());
    const event = await service.recordChannelPush(
      'org-1',
      PUBLISHER,
      release.id,
      { channelId: channel.id, resultRef: 'catalog-entry-v0.1.0' },
      mkConfig()
    );
    expect(event.type).toBe('channel-pushed');
    expect(event.channelId).toBe(channel.id);
    expect(event.resultRef).toBe('catalog-entry-v0.1.0');
    expect(event.operatorRootId).toBe(PUBLISHER);
    // 渠道推送不改变发布状态
    const events = await service.loadEvents('org-1');
    expect(deriveReleaseState(release.id, events)).toBe('published');
  });

  it('rejects unknown channel and invalid kind', async () => {
    const { sdk } = createMockSdk();
    const { service, release } = await registerFixture(sdk);
    await service.verifyRelease('org-1', PUBLISHER, release.id, { spkgPath: '/tmp/pkg.spkg' }, mkConfig());
    await service.publishRelease('org-1', PUBLISHER, release.id, mkConfig());
    await expect(
      service.recordChannelPush('org-1', PUBLISHER, release.id, { channelId: 'chan-missing' }, mkConfig())
    ).rejects.toThrow(/未登记/);
    await expect(
      service.saveChannel('org-1', PUBLISHER, { kind: 'ftp' as any, target: 'x' }, mkConfig(), 'member')
    ).rejects.toThrow(/渠道类型非法/);
  });
});

describe('spark-release-manager service · 撤回（档三-26：组织内登记 + 公告告知，不回滚）', () => {
  it('retract appends append-only event and pushes retraction notice card', async () => {
    const { sdk, sentMessages } = createMockSdk();
    const { service, release } = await registerFixture(sdk);
    await service.verifyRelease('org-1', PUBLISHER, release.id, { spkgPath: '/tmp/pkg.spkg' }, mkConfig());
    await service.publishRelease('org-1', PUBLISHER, release.id, mkConfig());
    const event = await service.retractRelease('org-1', PUBLISHER, release.id, { reason: '发现严重回退' }, mkConfig(), 'member');

    expect(event.type).toBe('retracted');
    expect(event.reason).toBe('发现严重回退');
    const events = await service.loadEvents('org-1');
    expect(deriveReleaseState(release.id, events)).toBe('retracted');
    // 发布卡片 + 撤回告知卡片各一条
    expect(sentMessages).toHaveLength(2);
    expect(sentMessages[1].payload.retracted).toBe(true);
    expect(sentMessages[1].payload.summary).toContain('【版本撤回·spark-foo v0.1.0】');
    // 撤回不可重复
    await expect(service.retractRelease('org-1', PUBLISHER, release.id, {}, mkConfig(), 'member')).rejects.toThrow(/已撤回/);
  });

  it('non-publisher non-admin cannot retract（业务层校验实测）', async () => {
    const { sdk } = createMockSdk();
    const { service, release } = await registerFixture(sdk);
    await expect(service.retractRelease('org-1', 'x'.repeat(64), release.id, {}, mkConfig(), 'member')).rejects.toThrow(/撤回/);
    // 名册管理员可撤回
    const event = await service.retractRelease('org-1', ADMIN, release.id, {}, mkConfig(), 'admin');
    expect(event.type).toBe('retracted');
  });
});

describe('spark-release-manager service · 验签独立复算（验收⑤）', () => {
  it('verifies release signature via payload recomputation（重算比对而非回放）', async () => {
    const { sdk } = createMockSdk();
    const { service, release } = await registerFixture(sdk);
    await expect(service.verifyReleaseSignature(release)).resolves.toBe(true);

    // 篡改资产哈希后：重算载荷与随记录载荷不一致 → 验签失败（不依赖密码学面）
    const tampered: ReleaseRecord = {
      ...release,
      artifacts: [{ ...release.artifacts[0], sha256: SHA_B }]
    };
    await expect(service.verifyReleaseSignature(tampered)).resolves.toBe(false);
    expect(sdk.identity.verify).toHaveBeenCalledTimes(1);
  });
});

describe('spark-release-manager service · 版本上报骨架与存证状态', () => {
  it('opt-in version report appends minimal record（档三-25：不含设备指纹）', async () => {
    const { sdk } = createMockSdk();
    const service = new ReleaseManagerService(sdk);
    const report = await service.reportVersion(
      'org-1',
      'a'.repeat(64),
      { pluginId: 'spark-foo', version: '0.1.0', trust: 'signed' },
      'member'
    );
    expect(report.trust).toBe('signed');
    expect(Object.keys(report).sort()).toEqual(['at', 'id', 'orgId', 'pluginId', 'reporterRootId', 'trust', 'version']);
    await expect(
      service.reportVersion('org-1', 'a'.repeat(64), { pluginId: 'spark-foo', version: '0.1.0', trust: '' }, 'member')
    ).rejects.toThrow(/信任级/);
    // 服务层枚举收敛：自由字符串拒收（防污染分布视图）
    await expect(
      service.reportVersion('org-1', 'a'.repeat(64), { pluginId: 'spark-foo', version: '0.1.0', trust: 'totally-legit' }, 'member')
    ).rejects.toThrow(/信任级/);
    await expect(
      service.reportVersion('org-1', 'a'.repeat(64), { pluginId: 'spark-foo', version: '0.1.0', trust: 'signed' }, null)
    ).rejects.toThrow(/组织成员/);
  });

  it('exposes evidence head hash and chain verification without faking anchoring（不伪造时效）', async () => {
    const { sdk } = createMockSdk();
    const service = new ReleaseManagerService(sdk);
    const status = await service.getEvidenceStatus();
    expect(status.headHash).toBe('ab'.repeat(32));
    expect(status.chainValid).toBe(true);
    expect(status.chainHeight).toBe(42);
  });
});

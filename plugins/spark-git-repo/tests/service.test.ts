import { beforeEach, describe, expect, it, vi } from 'vitest';
import { GitRepoService, probeCapabilities } from '../service';
import { resetShellCacheForTest, type ExecFn } from '../git';
import { utf8ToBytes, bytesToBase64, type MirrorManifest } from '../model';
import { buildMirrorManifestPayload, deriveIdentity } from '../wire';

/**
 * mock SDK：affairs/content 按已落地 SDK 面实现（钉住真实契约）；data/identity/
 * messages/sys 对齐 spark-affairs 测试的 mock 形态。git CLI 走可编程 exec mock。
 */
const PUB_KEY = '0EqyMnQrtKs6E2i9RhXk5tAiSrcaAWuvhSCjMsl3hzc=';
/** 本机 actor 身份（插件域身份 id = deriveIdentity(publicKey)，与签名操作 actor.identity 同口径） */
const ACTOR_IDENTITY = deriveIdentity(PUB_KEY);
const PROJECT_ID = 'ab'.repeat(32);
const PR_ID = 'cd'.repeat(32);
const CID = 'c'.repeat(64);
const HEAD_MAIN = 'a'.repeat(40);
const HEAD_FEATURE = 'b'.repeat(40);
const HEAD_MERGED = 'e'.repeat(40);

function manifestOp(version: number, objects: MirrorManifest['objects'] = [], opHash = 'ff'.repeat(32)) {
  return {
    opHash,
    op: {
      opType: 'content',
      payload: buildMirrorManifestPayload({
        repo: 'spark',
        defaultBranch: 'main',
        branches: [{ name: 'main', head: HEAD_MAIN }],
        version,
        objects,
        ...(version === 1 ? { importHead: HEAD_MAIN } : {})
      }),
      actor: { kind: 'person', identity: 'id-maintainer', publicKey: PUB_KEY },
      declaredAt: 1700000000000
    }
  };
}

function prOpenOp() {
  return {
    opHash: '01'.repeat(32),
    op: {
      opType: 'content',
      payload: {
        kind: 'pr.open',
        title: '修复空指针',
        description: '详见附件',
        base: 'main',
        head: HEAD_FEATURE,
        attachments: [{ kind: 'bundle', cid: CID, size: 128, name: 'fix.bundle' }]
      },
      actor: { kind: 'person', identity: 'id-contributor', publicKey: PUB_KEY },
      declaredAt: 1700000000000
    }
  };
}

function prGenesis() {
  return {
    affairV: 1,
    type: 'spark-git-repo:pr',
    title: '修复空指针',
    refs: [{ target: PROJECT_ID, rel: 'parent' }],
    sig: 'sig'
  };
}

type MockOptions = {
  projectOps?: Array<{ opHash: string; op: Record<string, unknown> }>;
  prOps?: Array<{ opHash: string; op: Record<string, unknown> }>;
  followed?: string[];
  blobs?: Map<string, string>;
  fetchable?: Set<string>;
  /** 项目议题规则文档的写权集合（rules.maintainers）；缺省 = 本机 actor */
  maintainers?: string[];
  /** readRules 抛错（fail-closed 路径） */
  rulesError?: boolean;
};

function createMockSdk(options: MockOptions = {}) {
  const projectOps = options.projectOps ?? [manifestOp(1)];
  const prOps = options.prOps ?? [prOpenOp()];
  const followed = options.followed ?? [PROJECT_ID, PR_ID];
  const blobs = options.blobs ?? new Map<string, string>();
  const fetchable = options.fetchable ?? new Set<string>();
  const maintainers = options.maintainers ?? [ACTOR_IDENTITY];
  const pins: Array<{ cid: string; root: string }> = [];
  const unpins: Array<{ cid: string; root: string }> = [];
  const submitted: Array<Record<string, unknown>> = [];
  const dataDocs = new Map<string, Record<string, unknown>>();
  let opCounter = 0;

  const affairs = {
    create: vi.fn().mockResolvedValue({ affairId: PR_ID, genesis: prGenesis() }),
    follow: vi.fn().mockResolvedValue(PROJECT_ID),
    unfollow: vi.fn().mockResolvedValue(undefined),
    listFollowed: vi.fn().mockResolvedValue(followed),
    submitOp: vi.fn().mockImplementation((op: Record<string, unknown>) => {
      submitted.push(op);
      opCounter += 1;
      return Promise.resolve({ affairId: op.affairId, opHash: `hash-${opCounter}`, status: 'accepted' });
    }),
    readLog: vi.fn().mockImplementation((affairId: string) => {
      if (affairId === PROJECT_ID) {
        return Promise.resolve({
          affairId,
          genesis: { affairV: 1, type: 'spark-project:topic', title: '星火', refs: [], sig: 's' },
          ops: projectOps,
          heads: [],
          followedAt: 1700000000000
        });
      }
      if (affairId === PR_ID) {
        return Promise.resolve({ affairId, genesis: prGenesis(), ops: prOps, heads: [], followedAt: 1700000000000 });
      }
      return Promise.resolve({ affairId, genesis: null, ops: [], heads: [], followedAt: null });
    }),
    readRules: vi.fn().mockImplementation(() => {
      if (options.rulesError) {
        return Promise.reject(new Error('规则文档读取失败（mock）'));
      }
      return Promise.resolve({
        affairId: PROJECT_ID,
        nowMs: 1700000000000,
        current: { seq: 0, rulesHash: 'rh', rules: { maintainers } },
        versions: [],
        changes: []
      });
    }),
    onChange: vi.fn().mockResolvedValue(undefined)
  };
  const content = {
    saveBlob: vi.fn().mockImplementation(async (base64: string) => {
      const cid = `cid-${blobs.size + 1}`.padEnd(64, '0');
      blobs.set(cid, base64);
      return { cid, size: base64.length };
    }),
    readBlob: vi.fn().mockImplementation(async (cid: string) => blobs.get(cid) ?? null),
    fetchBlob: vi.fn().mockImplementation(async (cid: string) => (blobs.has(cid) ? blobs.get(cid)! : fetchable.has(cid) ? `fetched-${cid}` : null)),
    listBlobs: vi.fn().mockImplementation(async () => [...blobs.keys()]),
    pinRoot: vi.fn().mockImplementation(async (cid: string, root: string) => {
      pins.push({ cid, root });
      return { success: true };
    }),
    unpinRoot: vi.fn().mockImplementation(async (cid: string, root: string) => {
      unpins.push({ cid, root });
      return { success: true };
    }),
    gcSweep: vi.fn().mockResolvedValue([])
  };
  const sdk = {
    domain: 'plugin:spark-git-repo',
    affairs,
    content,
    data: {
      declareCollection: vi.fn().mockResolvedValue({}),
      get: vi.fn().mockImplementation(async (collection: string, id: string) => dataDocs.get(`${collection}/${id}`) ?? null),
      save: vi.fn().mockImplementation(async (collection: string, id: string, doc: Record<string, unknown>) => {
        dataDocs.set(`${collection}/${id}`, doc);
        return { success: true };
      }),
      delete: vi.fn().mockResolvedValue({ success: true }),
      query: vi.fn().mockResolvedValue({ items: [] })
    },
    identity: {
      sign: vi.fn().mockResolvedValue({ domain: 'plugin:spark-git-repo', publicKey: PUB_KEY, signature: 'sig-1', payloadHash: 'ph' }),
      verify: vi.fn().mockResolvedValue({ valid: true })
    },
    messages: { sendAppMessage: vi.fn().mockResolvedValue({ id: 'm1' }) },
    sys: { exec: vi.fn() }
  };
  return { sdk, affairs, content, pins, unpins, submitted, dataDocs, blobs };
}

type Call = { program: string; args: string[] };

function gitExec(routes: Array<{ match: (program: string, args: string[]) => boolean; stdout?: string; stderr?: string; exitCode?: number }>) {
  const calls: Call[] = [];
  const exec: ExecFn = async (program, args) => {
    calls.push({ program, args });
    const route = routes.find((r) => r.match(program, args));
    if (!route) {
      return { stdout: '', stderr: `unmocked: ${program} ${args.join(' ')}`, exitCode: 127 };
    }
    return { stdout: route.stdout ?? '', stderr: route.stderr ?? '', exitCode: route.exitCode ?? 0 };
  };
  return { exec, calls };
}

const SH_PROBE = { match: (p: string, a: string[]) => p === 'sh' && a[1] === 'echo spark-probe', stdout: 'spark-probe\n' };

/** 发起 PR 路径的 git 路由（分支以镜像 base 为祖先） */
function openPrGitRoutes() {
  return [
    SH_PROBE,
    { match: (p, a) => p === 'git' && a[2] === 'rev-parse' && a[3] === 'feature', stdout: `${HEAD_FEATURE}\n` },
    { match: (p, a) => p === 'git' && a[2] === 'merge-base', exitCode: 0, stdout: '' },
    { match: (p, a) => p === 'git' && a[2] === 'bundle', stdout: '' },
    { match: (p, a) => p === 'sh' && a[1]?.includes('base64 -w0'), stdout: 'QlVORExFLQ==' },
    { match: (p, a) => p === 'sh' && a[1]?.includes('rm -f'), stdout: '' }
  ];
}

beforeEach(() => {
  resetShellCacheForTest();
});

describe('构造与能力探测', () => {
  it('affairs / content 缺失时构造抛出明确错误', () => {
    const { sdk } = createMockSdk();
    expect(() => new GitRepoService({ ...sdk, affairs: undefined } as never)).toThrow('sdk.affairs');
    expect(() => new GitRepoService({ ...sdk, content: undefined } as never)).toThrow('sdk.content');
  });

  it('probeCapabilities：sys 在且非移动 UA → 桌面写路径可用', () => {
    const { sdk } = createMockSdk();
    const caps = probeCapabilities(sdk as never);
    expect(caps.affairs).toBe(true);
    expect(caps.content).toBe(true);
    expect(caps.desktopWrite).toBe(true);
    expect(probeCapabilities({ ...sdk, sys: undefined } as never).desktopWrite).toBe(false);
  });
});

describe('绑定与关注', () => {
  it('绑定读写（lww 集合）；非法 affairId 拒绝', async () => {
    const { sdk } = createMockSdk();
    const service = new GitRepoService(sdk as never);
    expect(await service.getBinding()).toBeNull();
    await service.bindProject(PROJECT_ID, 'spark');
    expect(await service.getBinding()).toEqual({ projectAffairId: PROJECT_ID, repoName: 'spark' });
    await expect(service.bindProject('not-hex', 'x')).rejects.toThrow('形状非法');
  });

  it('listFollowedTopics 跳过创世未同步的事务', async () => {
    const { sdk } = createMockSdk({ followed: [PROJECT_ID, 'ee'.repeat(32)] });
    const service = new GitRepoService(sdk as never);
    const topics = await service.listFollowedTopics();
    expect(topics).toHaveLength(1);
    expect(topics[0].title).toBe('星火');
  });
});

describe('镜像清单读取与对象装载', () => {
  it('getMirrorView：最新清单 + 本地持有度', async () => {
    const objects = [{ sha: HEAD_MAIN, type: 'commit' as const, cid: CID }];
    const { sdk } = createMockSdk({ projectOps: [manifestOp(1, objects), manifestOp(2, objects, 'fe'.repeat(32))] });
    const service = new GitRepoService(sdk as never);
    const view = await service.getMirrorView(PROJECT_ID);
    expect(view?.manifest.version).toBe(2);
    expect(view?.status).toEqual({ total: 1, local: 0, missing: [CID] });
  });

  it('无清单 → null', async () => {
    const { sdk } = createMockSdk({ projectOps: [] });
    const service = new GitRepoService(sdk as never);
    expect(await service.getMirrorView(PROJECT_ID)).toBeNull();
  });

  it('loadObjectStore：命中本地 / 拉回缺失 / 不可拉取进 missing', async () => {
    const commitBytes = utf8ToBytes(`tree ${'f'.repeat(40)}\n\nroot`);
    const objects = [
      { sha: HEAD_MAIN, type: 'commit' as const, cid: CID },
      { sha: 'd'.repeat(40), type: 'commit' as const, cid: 'd'.repeat(64) }
    ];
    const blobs = new Map([[CID, bytesToBase64(commitBytes)]]);
    const { sdk } = createMockSdk({ projectOps: [manifestOp(1, objects)], blobs });
    const service = new GitRepoService(sdk as never);
    const view = (await service.getMirrorView(PROJECT_ID))!;
    const { store, missing, loaded } = await service.loadObjectStore(view.manifest, { fetch: true });
    expect(loaded).toBe(1);
    expect(missing).toEqual(['d'.repeat(64)]);
    expect(store.get(HEAD_MAIN)?.type).toBe('commit');
  });
});

describe('PR 发现与详情', () => {
  it('listKnownPrs 只收 PR 类型 + parent 指向本项目的子事务', async () => {
    const { sdk } = createMockSdk({ followed: [PROJECT_ID, PR_ID] });
    const service = new GitRepoService(sdk as never);
    const prs = await service.listKnownPrs(PROJECT_ID);
    expect(prs).toHaveLength(1);
    expect(prs[0].affairId).toBe(PR_ID);
    expect(prs[0].state.open?.title).toBe('修复空指针');
    // 其他项目的 PR 不收
    expect(await service.listKnownPrs('ff'.repeat(32))).toHaveLength(0);
  });

  it('getPrDetail 推导状态机', async () => {
    const { sdk } = createMockSdk();
    const service = new GitRepoService(sdk as never);
    const state = await service.getPrDetail(PR_ID);
    expect(state.status).toBe('open');
    expect(state.currentHead).toBe(HEAD_FEATURE);
  });
});

describe('发起 PR', () => {
  it('happy path：bundle → saveBlob → 子事务 → pr.open → pinRoot(pr:{id})', async () => {
    const { sdk, affairs, content, pins, submitted } = createMockSdk();
    const { exec } = gitExec(openPrGitRoutes());
    const service = new GitRepoService(sdk as never, exec);
    const result = await service.openPr({
      projectAffairId: PROJECT_ID,
      repoDir: '/repo',
      branch: 'feature',
      base: 'main',
      title: '修复空指针',
      description: 'desc'
    });
    expect(result.prAffairId).toBe(PR_ID);
    expect(affairs.create).toHaveBeenCalledOnce();
    const genesis = affairs.create.mock.calls[0][0] as { type: string; refs: Array<{ target: string; rel: string }> };
    expect(genesis.type).toBe('spark-git-repo:pr');
    expect(genesis.refs).toEqual([{ target: PROJECT_ID, rel: 'parent' }]);
    const openOp = submitted.find((op) => (op.payload as { kind: string }).kind === 'pr.open');
    expect(openOp).toBeDefined();
    const payload = openOp!.payload as { head: string; attachments: Array<{ kind: string; cid: string }>; base: string };
    expect(payload.head).toBe(HEAD_FEATURE);
    expect(payload.base).toBe('main');
    expect(payload.attachments[0].kind).toBe('bundle');
    expect(content.saveBlob).toHaveBeenCalledWith('QlVORExFLQ==');
    expect(pins.some((p) => p.root === `pr:${PR_ID}` && p.cid === payload.attachments[0].cid)).toBe(true);
    // 操作已签名（内核入站硬要求）
    expect(openOp).toHaveProperty('sig', 'sig-1');
  });

  it('分支不以 base 为祖先 → 拒绝（先 rebase）', async () => {
    const { sdk } = createMockSdk();
    const { exec } = gitExec([
      SH_PROBE,
      { match: (p, a) => p === 'git' && a[2] === 'rev-parse', stdout: `${HEAD_FEATURE}\n` },
      { match: (p, a) => p === 'git' && a[2] === 'merge-base', exitCode: 1 }
    ]);
    const service = new GitRepoService(sdk as never, exec);
    await expect(
      service.openPr({ projectAffairId: PROJECT_ID, repoDir: '/r', branch: 'feature', base: 'main', title: 'tt', description: '' })
    ).rejects.toThrow('不以镜像 main head 为祖先');
  });

  it('无镜像清单 → 拒绝发起', async () => {
    const { sdk } = createMockSdk({ projectOps: [] });
    const service = new GitRepoService(sdk as never, gitExec([]).exec);
    await expect(
      service.openPr({ projectAffairId: PROJECT_ID, repoDir: '/r', branch: 'f', base: 'main', title: 'tt', description: '' })
    ).rejects.toThrow('尚无镜像清单');
  });
});

describe('维护者合并', () => {
  function mergeGitRoutes(opts: { localHead?: string } = {}) {
    return [
      SH_PROBE,
      { match: (p, a) => p === 'sh' && a[1]?.includes('base64 -d'), stdout: '' },
      { match: (p, a) => p === 'sh' && a[1]?.includes('batch-check'), stdout: `${HEAD_MERGED} commit\n` },
      { match: (p, a) => p === 'sh' && a[1]?.includes('base64 -w0'), stdout: 'T0JK' },
      { match: (p, a) => p === 'sh' && a[1]?.includes('rm -f'), stdout: '' },
      { match: (p, a) => p === 'sh' && a[1]?.includes('cat-file'), stdout: 'T0JK' },
      { match: (p, a) => p === 'git' && a[2] === 'fetch', stdout: '' },
      { match: (p, a) => p === 'git' && a[2] === 'checkout', stdout: '' },
      { match: (p, a) => p === 'git' && a[2] === 'rev-parse' && a[3] === 'main', stdout: `${opts.localHead ?? HEAD_MAIN}\n` },
      { match: (p, a) => p === 'git' && a[2] === 'rev-parse' && a[3] === 'HEAD', stdout: `${HEAD_MERGED}\n` },
      { match: (p, a) => p === 'git' && a[2] === 'merge' && a.includes('--ff-only'), stdout: '' },
      { match: (p, a) => p === 'git' && a[2] === 'merge-base', exitCode: 0 },
      { match: (p, a) => p === 'git' && a[2] === 'for-each-ref', stdout: `${HEAD_MERGED} main\n` },
      { match: (p, a) => p === 'git' && a[2] === 'rev-list', stdout: `${HEAD_MERGED}\n` },
      { match: (p) => p === 'git', stdout: '' }
    ];
  }

  it('happy path：fetch → ff-only 合并 → FF 复核 → 发布 v2 → pr.merged 回执 → 附件解 pin', async () => {
    const { sdk, submitted, unpins, pins } = createMockSdk({ blobs: new Map([[CID, 'QlVORExFLQ==']]) });
    const { exec } = gitExec(mergeGitRoutes());
    const service = new GitRepoService(sdk as never, exec);
    const result = await service.mergePr({ prAffairId: PR_ID, projectAffairId: PROJECT_ID, repoDir: '/auth' });
    expect(result.resultCommit).toBe(HEAD_MERGED);
    expect(result.mirrorVersion).toBe(2);
    const mergedOp = submitted.find((op) => (op.payload as { kind: string }).kind === 'pr.merged');
    expect(mergedOp).toBeDefined();
    expect(mergedOp!.payload).toMatchObject({ kind: 'pr.merged', resultCommit: HEAD_MERGED, mirrorVersion: 2 });
    const manifestOpSubmitted = submitted.find((op) => (op.payload as { kind: string }).kind === 'git.mirror.manifest');
    expect((manifestOpSubmitted!.payload as { version: number }).version).toBe(2);
    expect(unpins).toContainEqual({ cid: CID, root: `pr:${PR_ID}` });
    // 新镜像对象被清单 pin 住
    expect(pins.some((p) => p.root === `mirror:${PROJECT_ID}`)).toBe(true);
  });

  it('附件全部 provider 不可达 → 如实报错「附件暂不可拉取」', async () => {
    const { sdk } = createMockSdk();
    const { exec } = gitExec([SH_PROBE]);
    const service = new GitRepoService(sdk as never, exec);
    await expect(service.mergePr({ prAffairId: PR_ID, projectAffairId: PROJECT_ID, repoDir: '/auth' })).rejects.toThrow(
      '附件暂不可拉取'
    );
  });

  it('本地 head 与镜像清单不一致 → 中止（串行合并约定）', async () => {
    const { sdk } = createMockSdk({ blobs: new Map([[CID, 'QlVORExFLQ==']]) });
    const { exec } = gitExec(mergeGitRoutes({ localHead: '9'.repeat(40) }));
    const service = new GitRepoService(sdk as never, exec);
    await expect(service.mergePr({ prAffairId: PR_ID, projectAffairId: PROJECT_ID, repoDir: '/auth' })).rejects.toThrow(
      '与镜像清单 head'
    );
  });

  it('已终态的 PR 不能再合并', async () => {
    const mergedPrOps = [
      prOpenOp(),
      {
        opHash: '02'.repeat(32),
        op: {
          opType: 'content',
          payload: { kind: 'pr.merged', resultCommit: HEAD_MERGED, mirrorVersion: 2 },
          actor: { kind: 'person', identity: ACTOR_IDENTITY, publicKey: PUB_KEY },
          declaredAt: 1700000001000
        }
      }
    ];
    const { sdk } = createMockSdk({ prOps: mergedPrOps });
    const service = new GitRepoService(sdk as never, gitExec([SH_PROBE]).exec);
    await expect(service.mergePr({ prAffairId: PR_ID, projectAffairId: PROJECT_ID, repoDir: '/auth' })).rejects.toThrow('不在开放状态');
  });
});

describe('关闭 PR / 评论 / 评审', () => {
  it('closePr：理由入日志 + 附件解 pin', async () => {
    const { sdk, submitted, unpins } = createMockSdk();
    const service = new GitRepoService(sdk as never);
    await service.closePr(PR_ID, '重复 PR');
    const op = submitted.find((item) => (item.payload as { kind: string }).kind === 'pr.closed');
    expect((op!.payload as { reason: string }).reason).toBe('重复 PR');
    expect(unpins).toContainEqual({ cid: CID, root: `pr:${PR_ID}` });
  });

  it('comment / review 提交签名操作', async () => {
    const { sdk, submitted } = createMockSdk();
    const service = new GitRepoService(sdk as never);
    await service.commentPr(PR_ID, '这一行有问题', { path: 'a.ts', line: 3 });
    await service.reviewPr(PR_ID, 'approve', 'LGTM');
    const kinds = submitted.map((op) => (op.payload as { kind: string }).kind);
    expect(kinds).toEqual(['pr.comment', 'pr.review']);
    await expect(service.commentPr(PR_ID, 'x')).rejects.toThrow('过短');
  });
});

describe('终态写权校验（档一-3/§3.2 硬伤修复）', () => {
  const OUTSIDER_MAINTAINERS = ['0'.repeat(64)];

  it('伪造 pr.merged（actor ∉ 写权集合）→ mergePr 拒绝，无 merged 提交、无镜像发布', async () => {
    const { sdk, submitted, content } = createMockSdk({ maintainers: OUTSIDER_MAINTAINERS });
    const service = new GitRepoService(sdk as never, gitExec([SH_PROBE]).exec);
    await expect(service.mergePr({ prAffairId: PR_ID, projectAffairId: PROJECT_ID, repoDir: '/auth' })).rejects.toThrow(
      '不在项目议题规则声明的写权集合'
    );
    expect(submitted.some((op) => (op.payload as { kind: string }).kind === 'pr.merged')).toBe(false);
    expect(submitted.some((op) => (op.payload as { kind: string }).kind === 'git.mirror.manifest')).toBe(false);
    expect(content.saveBlob).not.toHaveBeenCalled();
  });

  it('伪造 pr.closed（actor ∉ 写权集合）→ closePr 拒绝，附件保持 pin', async () => {
    const { sdk, submitted, unpins } = createMockSdk({ maintainers: OUTSIDER_MAINTAINERS });
    const service = new GitRepoService(sdk as never);
    await expect(service.closePr(PR_ID, '恶意关闭')).rejects.toThrow('不在项目议题规则声明的写权集合');
    expect(submitted.some((op) => (op.payload as { kind: string }).kind === 'pr.closed')).toBe(false);
    expect(unpins).toHaveLength(0);
  });

  it('规则文档不可读 → 写权校验 fail-closed 中止（不降级放行）', async () => {
    const { sdk, submitted } = createMockSdk({ rulesError: true });
    const service = new GitRepoService(sdk as never);
    await expect(service.closePr(PR_ID, '重复 PR')).rejects.toThrow('fail-closed');
    expect(submitted.some((op) => (op.payload as { kind: string }).kind === 'pr.closed')).toBe(false);
  });

  it('读路径：集合外伪造的 merged 不改状态，留 timeline 并如实标注「未采纳」', async () => {
    const forgedPrOps = [
      prOpenOp(),
      {
        opHash: '02'.repeat(32),
        op: {
          opType: 'content',
          payload: { kind: 'pr.merged', resultCommit: HEAD_MERGED, mirrorVersion: 2 },
          actor: { kind: 'person', identity: '1'.repeat(64), publicKey: PUB_KEY },
          declaredAt: 1700000001000
        }
      }
    ];
    const { sdk } = createMockSdk({ prOps: forgedPrOps, maintainers: OUTSIDER_MAINTAINERS });
    const service = new GitRepoService(sdk as never);
    const state = await service.getPrDetail(PR_ID);
    expect(state.status).toBe('open');
    expect(state.merged).toBeNull();
    const forged = state.timeline.find((item) => item.kind === 'pr.merged');
    expect(forged?.rejectedReason).toContain('未采纳');
  });
});

describe('合并回执独立核验（§3.4）', () => {
  it('回执 commit ∈ 新镜像历史 → ok；版本未同步 → null', async () => {
    // 镜像 v2 对象：merged(head) → feature → main
    const treeSha = 'f'.repeat(40);
    const commit = (sha: string, parents: string[]) =>
      bytesToBase64(utf8ToBytes([`tree ${treeSha}`, ...parents.map((p) => `parent ${p}`), 'author A <a@b> 1700000000 +0000', 'committer A <a@b> 1700000000 +0000', '', 'm'].join('\n')));
    const cidMain = '1'.repeat(64);
    const cidFeature = '2'.repeat(64);
    const cidMerged = '3'.repeat(64);
    const objects = [
      { sha: HEAD_MAIN, type: 'commit' as const, cid: cidMain },
      { sha: HEAD_FEATURE, type: 'commit' as const, cid: cidFeature },
      { sha: HEAD_MERGED, type: 'commit' as const, cid: cidMerged }
    ];
    const blobs = new Map([
      [cidMain, commit(HEAD_MAIN, [])],
      [cidFeature, commit(HEAD_FEATURE, [HEAD_MAIN])],
      [cidMerged, commit(HEAD_MERGED, [HEAD_MAIN, HEAD_FEATURE])]
    ]);
    const prOps = [
      prOpenOp(),
      {
        opHash: '02'.repeat(32),
        op: {
          opType: 'content',
          payload: { kind: 'pr.merged', resultCommit: HEAD_FEATURE, mirrorVersion: 2 },
          actor: { kind: 'person', identity: ACTOR_IDENTITY, publicKey: PUB_KEY },
          declaredAt: 1700000001000
        }
      }
    ];
    // v2 清单 main head = HEAD_MERGED
    const v2Op = {
      opHash: 'fd'.repeat(32),
      op: {
        opType: 'content',
        payload: buildMirrorManifestPayload({
          repo: 'spark',
          defaultBranch: 'main',
          branches: [{ name: 'main', head: HEAD_MERGED }],
          version: 2,
          objects
        })
      }
    };
    const { sdk } = createMockSdk({ projectOps: [manifestOp(1), v2Op], prOps, blobs });
    const service = new GitRepoService(sdk as never);
    const state = await service.getPrDetail(PR_ID);
    const verdict = await service.verifyMergedPr(PROJECT_ID, state);
    expect(verdict).toEqual({ ok: true, checkedBranch: 'main', reason: null });
    // 未合并的 PR → null；回执指向未同步版本 → null
    const openState = { ...state, merged: { resultCommit: HEAD_FEATURE, mirrorVersion: 99, actor: 'a', declaredAt: 0, opHash: 'x' } };
    expect(await service.verifyMergedPr(PROJECT_ID, openState as never)).toBeNull();
  });
});

describe('发布镜像', () => {
  it('首版记录 importHead（档一-4），版本从 1 起；对象逐散 blob 并 pin', async () => {
    const { sdk, submitted, pins, content } = createMockSdk({ projectOps: [] });
    const { exec } = gitExec([
      SH_PROBE,
      { match: (p, a) => p === 'git' && a[2] === 'for-each-ref', stdout: `${HEAD_MAIN} main\n` },
      { match: (p, a) => p === 'git' && a[2] === 'rev-list', stdout: `${HEAD_MAIN}\n` },
      { match: (p, a) => p === 'sh' && a[1]?.includes('base64 -d'), stdout: '' },
      { match: (p, a) => p === 'sh' && a[1]?.includes('rm -f'), stdout: '' },
      { match: (p, a) => p === 'sh' && a[1]?.includes('batch-check'), stdout: `${HEAD_MAIN} commit\n` },
      { match: (p, a) => p === 'sh' && a[1]?.includes('cat-file'), stdout: 'T0JK' }
    ]);
    const service = new GitRepoService(sdk as never, exec);
    const result = await service.publishMirror(PROJECT_ID, { repoDir: '/auth', repoName: 'spark' });
    expect(result).toMatchObject({ version: 1, objectCount: 1 });
    const op = submitted.find((item) => (item.payload as { kind: string }).kind === 'git.mirror.manifest');
    expect(op).toBeDefined();
    expect(op!.payload).toMatchObject({ version: 1, importHead: HEAD_MAIN, repo: 'spark' });
    expect(content.saveBlob).toHaveBeenCalledWith('T0JK');
    expect(pins.some((p) => p.root === `mirror:${PROJECT_ID}`)).toBe(true);
  });

  it('空仓库（无分支）→ 拒绝', async () => {
    const { sdk } = createMockSdk({ projectOps: [] });
    const { exec } = gitExec([
      SH_PROBE,
      { match: (p, a) => p === 'git' && a[2] === 'for-each-ref', stdout: '' }
    ]);
    const service = new GitRepoService(sdk as never, exec);
    await expect(service.publishMirror(PROJECT_ID, { repoDir: '/auth', repoName: 'spark' })).rejects.toThrow('没有任何分支');
  });
});

describe('通知（message:app，降级不阻断）', () => {
  it('成功发送返回 true；异常降级 false', async () => {
    const { sdk } = createMockSdk();
    const service = new GitRepoService(sdk as never);
    expect(await service.notifyPrCard({ affairId: PR_ID, title: 't', status: '开放', base: 'main' })).toBe(true);
    const failing = createMockSdk();
    failing.sdk.messages.sendAppMessage.mockRejectedValue(new Error('限流'));
    const service2 = new GitRepoService(failing.sdk as never);
    expect(await service2.notifyPrCard({ affairId: PR_ID, title: 't', status: '开放', base: 'main' })).toBe(false);
  });
});

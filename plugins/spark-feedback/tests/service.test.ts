import { beforeEach, describe, expect, it, vi } from 'vitest';
import { FeedbackService, probeCapabilities, AFFAIRS_MODULE_MISSING, CONTENT_MODULE_MISSING } from '../service';
import { FEEDBACK_CHANNEL, type FeedbackInput } from '../model';

/**
 * mock SDK：affairs/content/messages 按已落地 SDK 面实现（钉住真实契约）；
 * data 为内存集合后端（declareCollection/save/get/delete/query）。
 */
const PROJECT_ID = 'ab'.repeat(32);
const CHILD_ID = 'cd'.repeat(32);
const CID = 'c'.repeat(64);

function validInput(overrides: Partial<FeedbackInput> = {}): FeedbackInput {
  return {
    type: 'bug',
    title: '启动后白屏',
    body: '升级到 0.3.2 后首次启动白屏，重启无效。',
    includeEnvironment: true,
    attachments: [],
    ...overrides
  };
}

type MockOptions = {
  followed?: string[];
  genesisByAffair?: Record<string, Record<string, unknown> | null>;
  resolutionsByAffair?: Record<string, Array<{ state: 'pending' | 'effective' | 'vetoed' | 'unanchored' }>>;
  createError?: boolean;
  readLogError?: boolean;
  sendMessageError?: boolean;
  /** data.delete 抛错（模拟合并规则拒绝等删除失败路径） */
  deleteError?: boolean;
};

function createMockSdk(options: MockOptions = {}) {
  const followed = options.followed ?? [PROJECT_ID];
  const blobs = new Map<string, string>();
  const pins: Array<{ cid: string; root: string }> = [];
  const unpins: Array<{ cid: string; root: string }> = [];
  const dataDocs = new Map<string, Record<string, unknown>>();
  const appMessages: Array<{ payload: Record<string, unknown>; card?: unknown }> = [];
  const created: Array<Record<string, unknown>> = [];

  const affairs = {
    create: vi.fn().mockImplementation((input: Record<string, unknown>) => {
      if (options.createError) {
        return Promise.reject(new Error('复制面不可达（mock）'));
      }
      created.push(input);
      return Promise.resolve({ affairId: CHILD_ID, genesis: { type: input.type } });
    }),
    follow: vi.fn().mockResolvedValue(PROJECT_ID),
    unfollow: vi.fn().mockResolvedValue(undefined),
    listFollowed: vi.fn().mockResolvedValue(followed),
    readLog: vi.fn().mockImplementation((affairId: string) => {
      if (options.readLogError) {
        return Promise.reject(new Error('本地副本读取失败（mock）'));
      }
      const genesis = options.genesisByAffair
        ? options.genesisByAffair[affairId] ?? null
        : { affairV: 1, type: 'spark-project:topic', title: '星火', publish: true, refs: [] };
      return Promise.resolve({ affairId, genesis, ops: [], heads: [], followedAt: 1700000000000 });
    }),
    readResolution: vi.fn().mockImplementation((affairId: string) =>
      Promise.resolve({
        affairId,
        resolutions: (options.resolutionsByAffair?.[affairId] ?? []).map((r, i) => ({
          opHash: `op-${i}`,
          result: null,
          condition: null,
          countedOps: null,
          rulesHash: 'rh',
          pubPeriodMs: 86400000,
          anchoredMs: null,
          objections: 0,
          state: r.state
        }))
      })
    )
  };
  const content = {
    saveBlob: vi.fn().mockImplementation(async (base64: string) => {
      const cid = CID.slice(0, Math.max(1, 64 - String(blobs.size).length)) + String(blobs.size).padStart(2, '0');
      blobs.set(cid, base64);
      return { cid, size: base64.length };
    }),
    pinRoot: vi.fn().mockImplementation(async (cid: string, root: string) => {
      pins.push({ cid, root });
      return { success: true };
    }),
    unpinRoot: vi.fn().mockImplementation(async (cid: string, root: string) => {
      unpins.push({ cid, root });
      return { success: true };
    })
  };
  // 合并规则台账：declareCollection 记录各集合 merge，save/delete 按声明 enforce
  // （对齐内核 plugindata 口径：append-only 拒绝覆盖与删除），防 mock 过松掩盖违规调用
  const collectionMerge = new Map<string, string>();
  const enforceAppendOnly = (name: string, op: string, keyExists = true): void => {
    if (collectionMerge.get(name) === 'append-only' && keyExists) {
      throw new Error(`AppendOnlyViolation: ${op} rejected on append-only collection ${name}（mock enforce）`);
    }
  };

  const sdk = {
    domain: 'plugin:spark-feedback',
    affairs,
    content,
    data: {
      declareCollection: vi.fn().mockImplementation(async (decl: { name: string; merge?: string }) => {
        collectionMerge.set(decl.name, decl.merge ?? 'lww-record');
        return {};
      }),
      get: vi.fn().mockImplementation(async (name: string, key: string) => dataDocs.get(`${name}/${key}`) ?? null),
      save: vi.fn().mockImplementation(async (name: string, key: string, value: Record<string, unknown>) => {
        enforceAppendOnly(name, 'save-overwrite', dataDocs.has(`${name}/${key}`));
        dataDocs.set(`${name}/${key}`, value);
        return { success: true };
      }),
      delete: vi.fn().mockImplementation(async (name: string, key: string) => {
        if (options.deleteError) {
          throw new Error('删除被拒绝（mock）');
        }
        enforceAppendOnly(name, 'delete', dataDocs.has(`${name}/${key}`));
        dataDocs.delete(`${name}/${key}`);
        return { success: true };
      }),
      query: vi.fn().mockImplementation(async (name: string) => ({
        items: [...dataDocs.entries()]
          .filter(([k]) => k.startsWith(`${name}/`))
          .map(([k, value]) => ({ key: k.slice(name.length + 1), value }))
      }))
    },
    messages: {
      sendAppMessage: vi.fn().mockImplementation(async (payload: Record<string, unknown>, card?: unknown) => {
        if (options.sendMessageError) {
          throw new Error('rate-limited（mock）');
        }
        appMessages.push({ payload, card });
        return { id: `m-${appMessages.length}` };
      }),
      onCardAction: vi.fn()
    }
  };
  return { sdk, affairs, content, pins, unpins, dataDocs, appMessages, created };
}

const CTX = {
  pluginId: 'spark-feedback',
  viewId: 'default',
  domain: 'plugin:spark-feedback',
  space: { type: 'personal' as const, id: 'personal' },
  theme: 'light' as const,
  mount: { viewType: 'app' as const },
  appVersion: '0.3.2',
  platform: 'windows' as const,
  shellVersion: '1'
};

beforeEach(() => {
  vi.restoreAllMocks();
});

describe('能力探测与降级', () => {
  it('affairs / content / messages 在场 → 全能力；缺席 → 对应位 false', () => {
    const { sdk } = createMockSdk();
    expect(probeCapabilities(sdk as never, CTX)).toEqual({
      affairs: true,
      content: true,
      messages: true,
      environment: true
    });
    expect(probeCapabilities({ ...sdk, affairs: undefined } as never, CTX).affairs).toBe(false);
    expect(probeCapabilities({ ...sdk, content: undefined } as never, CTX).content).toBe(false);
    expect(probeCapabilities({ ...sdk, messages: undefined } as never, CTX).messages).toBe(false);
    // 旧壳层 ctx 缺环境字段（档二-9 注入前降级）
    expect(probeCapabilities(sdk as never, { ...CTX, appVersion: undefined, platform: undefined, shellVersion: undefined }).environment).toBe(false);
  });

  it('构造不硬失败：affairs 缺失时 submit 给明确错误，导出仍可用（验收第 3 条）', async () => {
    const { sdk } = createMockSdk();
    const service = new FeedbackService({ ...sdk, affairs: undefined } as never, CTX);
    await expect(service.submit(validInput(), PROJECT_ID)).rejects.toThrow(AFFAIRS_MODULE_MISSING);
    const text = service.buildExport(validInput(), PROJECT_ID);
    expect(JSON.parse(text).genesisInput.type).toBe('bug');
  });

  it('content 缺失时上传附件给明确错误', async () => {
    const { sdk } = createMockSdk();
    const service = new FeedbackService({ ...sdk, content: undefined } as never, CTX);
    await expect(service.uploadAttachment('d1', { dataBase64: 'QQ==', name: 'a.png' })).rejects.toThrow(CONTENT_MODULE_MISSING);
  });
});

describe('目标议题解析（档三-15 三路兜底）', () => {
  it('偏好默认目标优先（须已关注）', async () => {
    const { sdk } = createMockSdk();
    const service = new FeedbackService(sdk as never, CTX);
    expect(await service.resolveTarget()).toBeNull();
    await service.savePrefs({ defaultTargetAffairId: PROJECT_ID });
    expect(await service.resolveTarget()).toEqual({ affairId: PROJECT_ID, source: 'prefs' });
  });

  it('偏好目标未关注 → 不采用（关注即副本语义）', async () => {
    const { sdk } = createMockSdk({ followed: [] });
    const service = new FeedbackService(sdk as never, CTX);
    await service.savePrefs({ defaultTargetAffairId: PROJECT_ID });
    expect(await service.resolveTarget()).toBeNull();
  });

  it('savePrefs 拒绝形状非法的 affairId', async () => {
    const { sdk } = createMockSdk();
    const service = new FeedbackService(sdk as never, CTX);
    await expect(service.savePrefs({ defaultTargetAffairId: 'bad' })).rejects.toThrow('形状非法');
  });

  it('targetMeta：publish===true 判公开；创世不可达 → null', async () => {
    const { sdk } = createMockSdk();
    const service = new FeedbackService(sdk as never, CTX);
    expect(await service.targetMeta(PROJECT_ID)).toEqual({ title: '星火', isPublic: true });
    const { sdk: sdk2 } = createMockSdk({ genesisByAffair: { [PROJECT_ID]: { title: '内部', publish: false } } });
    const service2 = new FeedbackService(sdk2 as never, CTX);
    expect(await service2.targetMeta(PROJECT_ID)).toEqual({ title: '内部', isPublic: false });
    expect(await service2.targetMeta('ff'.repeat(32))).toBeNull();
  });

  it('followGenesis：非对象即拒；对象委托内核全链校验', async () => {
    const { sdk, affairs } = createMockSdk();
    const service = new FeedbackService(sdk as never, CTX);
    await expect(service.followGenesis('not-an-object')).rejects.toThrow('JSON 对象');
    expect(await service.followGenesis({ affairV: 1 })).toBe(PROJECT_ID);
    expect(affairs.follow).toHaveBeenCalledWith({ affairV: 1 });
  });
});

describe('提交主路径（档一-1：sdk.affairs.create + refs parent）', () => {
  it('happy path：创世输入 type/refs/载荷 → 台账登记 → 回执卡片', async () => {
    const { sdk, created, appMessages, dataDocs } = createMockSdk();
    const service = new FeedbackService(sdk as never, CTX);
    const attachment = { cid: CID, name: 'shot.png', size: 128, mime: 'image/png' };
    const { entry, cardSent } = await service.submit(validInput({ attachments: [attachment] }), PROJECT_ID, { draftId: 'draft-1', confirmedPublic: true });

    expect(created).toHaveLength(1);
    const genesis = created[0] as {
      type: string;
      title: string;
      refs: Array<{ target: string; rel: string }>;
      extra: { feedback: Record<string, unknown> };
    };
    expect(genesis.type).toBe('bug');
    expect(genesis.title).toBe('启动后白屏');
    expect(genesis.refs).toEqual([{ target: PROJECT_ID, rel: 'parent' }]);
    // 缺省不携带 publish（fail-closed）
    expect('publish' in genesis).toBe(false);
    const feedback = genesis.extra.feedback;
    expect(feedback.feedbackChannel).toBe(FEEDBACK_CHANNEL);
    expect(feedback.environment).toEqual({
      spaceKind: 'personal',
      appVersion: '0.3.2',
      platform: 'windows',
      shellVersion: '1'
    });
    expect(feedback.attachments).toEqual([attachment]);

    // 台账登记（append-only 集合）
    const ledgerItems = [...dataDocs.keys()].filter((k) => k.startsWith('spark-feedback:ledger/'));
    expect(ledgerItems).toHaveLength(1);
    expect(entry.childAffairId).toBe(CHILD_ID);

    // 回执卡片（每次提交至多一条；summary 自含语义）
    expect(cardSent).toBe(true);
    expect(appMessages).toHaveLength(1);
    expect(String(appMessages[0].payload.summary).length).toBeLessThanOrEqual(200);
    expect(appMessages[0].payload.summary).toContain('启动后白屏');
    expect((appMessages[0].card as { viewId: string }).viewId).toBe('feedback-card');
  });

  it('附件 pinRoot 迁移：草稿根解除、台账根挂上', async () => {
    const { sdk, pins, unpins } = createMockSdk();
    const service = new FeedbackService(sdk as never, CTX);
    await service.submit(validInput({ attachments: [{ cid: CID, name: 'a.png', size: 1 }] }), PROJECT_ID, { draftId: 'draft-9', confirmedPublic: true });
    expect(pins.some((p) => p.cid === CID && p.root === `feedback:${CHILD_ID}`)).toBe(true);
    expect(unpins.some((p) => p.cid === CID && p.root === 'feedback:draft:draft-9')).toBe(true);
  });

  it('未关注目标议题 → 拒绝并给关注引导', async () => {
    const { sdk, created } = createMockSdk({ followed: [] });
    const service = new FeedbackService(sdk as never, CTX);
    await expect(service.submit(validInput(), PROJECT_ID)).rejects.toThrow('尚未关注目标议题');
    expect(created).toHaveLength(0);
  });

  it('输入非法 / 目标 affairId 形状非法 → 前置拒绝（不发创世）', async () => {
    const { sdk, created } = createMockSdk();
    const service = new FeedbackService(sdk as never, CTX);
    await expect(service.submit(validInput({ body: '短' }), PROJECT_ID)).rejects.toThrow('描述长度');
    await expect(service.submit(validInput(), 'bad-id')).rejects.toThrow('形状非法');
    expect(created).toHaveLength(0);
  });

  it('includeEnvironment=false → 载荷无 environment 段（opt-in 纪律）', async () => {
    const { sdk, created } = createMockSdk();
    const service = new FeedbackService(sdk as never, CTX);
    await service.submit(validInput({ includeEnvironment: false }), PROJECT_ID, { confirmedPublic: true });
    const feedback = (created[0].extra as { feedback: Record<string, unknown> }).feedback;
    expect('environment' in feedback).toBe(false);
  });

  it('ctx 缺环境字段 → reportedVersion 降级通道（档二-9）', async () => {
    const { sdk, created } = createMockSdk();
    const legacyCtx = { ...CTX, appVersion: undefined, platform: undefined, shellVersion: undefined };
    const service = new FeedbackService(sdk as never, legacyCtx);
    await service.submit(validInput({ reportedVersion: '0.3.1' }), PROJECT_ID, { confirmedPublic: true });
    const feedback = (created[0].extra as { feedback: { environment: Record<string, unknown> } }).feedback;
    expect(feedback.environment).toEqual({ spaceKind: 'personal', reportedVersion: '0.3.1' });
  });

  it('回执卡片失败（限流/权限）降级不阻断主流程', async () => {
    const { sdk } = createMockSdk({ sendMessageError: true });
    const service = new FeedbackService(sdk as never, CTX);
    const { cardSent } = await service.submit(validInput(), PROJECT_ID, { confirmedPublic: true });
    expect(cardSent).toBe(false);
    expect(await service.listLedger()).toHaveLength(1);
  });

  it('提交失败（复制面不可达）上抛，由用户重试（不做补偿机制）', async () => {
    const { sdk } = createMockSdk({ createError: true });
    const service = new FeedbackService(sdk as never, CTX);
    await expect(service.submit(validInput(), PROJECT_ID, { confirmedPublic: true })).rejects.toThrow('复制面不可达');
    expect(await service.listLedger()).toHaveLength(0);
  });

  it('公开性纵深校验：公开议题未确认即拒（不依赖视图层纪律）；确认后放行', async () => {
    // 默认 mock 目标议题 genesis.publish === true
    const { sdk, created } = createMockSdk();
    const service = new FeedbackService(sdk as never, CTX);
    await expect(service.submit(validInput(), PROJECT_ID)).rejects.toThrow('公开议题');
    await expect(service.submit(validInput(), PROJECT_ID, { confirmedPublic: false })).rejects.toThrow('公开议题');
    expect(created).toHaveLength(0);
    await service.submit(validInput(), PROJECT_ID, { confirmedPublic: true });
    expect(created).toHaveLength(1);
  });

  it('非公开议题无需确认；元数据不可达按未知处理（草稿补投路径不拦截）', async () => {
    const { sdk, created } = createMockSdk({
      genesisByAffair: { [PROJECT_ID]: { title: '内部议题', publish: false } }
    });
    const service = new FeedbackService(sdk as never, CTX);
    await service.submit(validInput(), PROJECT_ID);
    expect(created).toHaveLength(1);

    const { sdk: sdk2, created: created2 } = createMockSdk({ genesisByAffair: { [PROJECT_ID]: null } });
    const service2 = new FeedbackService(sdk2 as never, CTX);
    await service2.submit(validInput(), PROJECT_ID);
    expect(created2).toHaveLength(1);
  });
});

describe('草稿（local 集合，元数据不可达先存后补投）', () => {
  it('保存 / 列出 / 删除（删除解除附件草稿根）', async () => {
    const { sdk, unpins } = createMockSdk();
    const service = new FeedbackService(sdk as never, CTX);
    expect(await service.listDrafts()).toHaveLength(0);
    await service.saveDraft({
      id: 'd1',
      type: 'bug',
      title: 't1',
      body: 'b1',
      reproduction: '',
      attachments: [{ cid: CID, name: 'a.png', size: 1 }],
      targetAffairId: PROJECT_ID
    });
    const drafts = await service.listDrafts();
    expect(drafts).toHaveLength(1);
    expect(drafts[0].title).toBe('t1');
    await service.deleteDraft(drafts[0]);
    expect(await service.listDrafts()).toHaveLength(0);
    expect(unpins.some((p) => p.cid === CID && p.root === 'feedback:draft:d1')).toBe(true);
  });

  it('提交成功后清理对应草稿', async () => {
    const { sdk, dataDocs } = createMockSdk();
    const service = new FeedbackService(sdk as never, CTX);
    await service.saveDraft({
      id: 'draft-1',
      type: 'bug',
      title: 't',
      body: 'b',
      reproduction: '',
      attachments: [],
      targetAffairId: PROJECT_ID
    });
    await service.submit(validInput(), PROJECT_ID, { draftId: 'draft-1', confirmedPublic: true });
    expect(dataDocs.has('spark-feedback:drafts/draft-1')).toBe(false);
  });
});

describe('台账与状态呈现（本地副本所见，如实标注）', () => {
  async function submitOne(sdk: unknown, options?: Parameters<typeof createMockSdk>[0]) {
    const service = new FeedbackService(sdk as never, CTX);
    await service.submit(validInput(), PROJECT_ID, { confirmedPublic: true });
    return service;
  }

  it('决议生效 → effective；公示中 → pending；无决议 → none；副本不可读 → unavailable', async () => {
    const { sdk } = createMockSdk({ resolutionsByAffair: { [CHILD_ID]: [{ state: 'effective' }] } });
    const service = await submitOne(sdk);
    expect((await service.listLedgerView())[0].status).toBe('effective');

    const { sdk: sdk2 } = createMockSdk({ resolutionsByAffair: { [CHILD_ID]: [{ state: 'pending' }] } });
    expect((await (await submitOne(sdk2)).listLedgerView())[0].status).toBe('pending');

    const { sdk: sdk3 } = createMockSdk();
    expect((await (await submitOne(sdk3)).listLedgerView())[0].status).toBe('none');

    const { sdk: sdk4 } = createMockSdk({ genesisByAffair: { [CHILD_ID]: null } });
    expect((await (await submitOne(sdk4)).listLedgerView())[0].status).toBe('unavailable');
  });

  it('readLog 抛错 → unavailable（不编造状态）；affairs 缺 → 全部 unavailable', async () => {
    const { sdk } = createMockSdk({ readLogError: true });
    const service = await submitOne(sdk);
    expect((await service.listLedgerView())[0].status).toBe('unavailable');

    const { sdk: sdk2 } = createMockSdk();
    const degraded = new FeedbackService({ ...sdk2, affairs: undefined } as never, CTX);
    // 手工塞一条台账记录
    await degraded.savePrefs({});
    await (degraded as never as { sdk: { data: { save: Function } } }).sdk.data.save('spark-feedback:ledger', 'fb-x', {
      id: 'fb-x',
      targetAffairId: PROJECT_ID,
      childAffairId: CHILD_ID,
      type: 'bug',
      title: 't',
      submittedAt: Date.now()
    });
    expect((await degraded.listLedgerView())[0].status).toBe('unavailable');
  });

  it('删除台账条目：解除附件台账根（数据主权：子事务本身不动）', async () => {
    const genesis = {
      affairV: 1,
      type: 'bug',
      feedback: { attachments: [{ cid: CID }] }
    };
    const { sdk, unpins } = createMockSdk({ genesisByAffair: { [CHILD_ID]: genesis } });
    const service = await submitOne(sdk);
    const entry = (await service.listLedger())[0];
    await service.removeLedgerEntry(entry);
    expect(await service.listLedger()).toHaveLength(0);
    expect(unpins.some((p) => p.cid === CID && p.root === `feedback:${CHILD_ID}`)).toBe(true);
  });

  it('删除失败仍尽力解除 pinRoot，随后如实上抛（两条清理路径各自尽力）', async () => {
    const genesis = { affairV: 1, type: 'bug', feedback: { attachments: [{ cid: CID }] } };
    const { sdk, unpins } = createMockSdk({ genesisByAffair: { [CHILD_ID]: genesis }, deleteError: true });
    const service = await submitOne(sdk);
    const entry = (await service.listLedger())[0];
    await expect(service.removeLedgerEntry(entry)).rejects.toThrow('台账删除失败');
    expect(unpins.some((p) => p.cid === CID && p.root === `feedback:${CHILD_ID}`)).toBe(true);
    // 删除被拒 → 台账条目仍在（诚实呈现，不假装已删）
    expect(await service.listLedger()).toHaveLength(1);
  });

  it('mock 防线：append-only 集合的覆盖/删除被 enforce（防同类违规再被过松 mock 掩盖）', async () => {
    const { sdk } = createMockSdk();
    // 直接经 mock SDK 声明一个 append-only 集合并尝试覆盖/删除
    await sdk.data.declareCollection({ name: 'spark-feedback:probe', merge: 'append-only' });
    await sdk.data.save('spark-feedback:probe', 'k1', { v: 1 });
    await expect(sdk.data.save('spark-feedback:probe', 'k1', { v: 2 })).rejects.toThrow('AppendOnlyViolation');
    await expect(sdk.data.delete('spark-feedback:probe', 'k1')).rejects.toThrow('AppendOnlyViolation');
  });

  it('countToday 只计今日提交（温馨提示依据）', async () => {
    const { sdk } = createMockSdk();
    const service = await submitOne(sdk);
    expect(await service.countToday()).toBe(1);
  });
});

describe('附件上传（opt-in 逐件确认后调用）', () => {
  it('saveBlob → pinRoot 草稿根；取消勾选解除根标记', async () => {
    const { sdk, pins, unpins } = createMockSdk();
    const service = new FeedbackService(sdk as never, CTX);
    const attachment = await service.uploadAttachment('d1', { dataBase64: 'QQ==', name: 'a.png', mime: 'image/png' });
    expect(attachment.name).toBe('a.png');
    expect(pins.some((p) => p.cid === attachment.cid && p.root === 'feedback:draft:d1')).toBe(true);
    await service.discardAttachment('d1', attachment.cid);
    expect(unpins.some((p) => p.cid === attachment.cid && p.root === 'feedback:draft:d1')).toBe(true);
  });
});

describe('手动导出兜底（不依赖 sdk.affairs）', () => {
  it('导出含 refs parent 与载荷；目标/输入非法即拒', () => {
    const { sdk } = createMockSdk();
    const service = new FeedbackService(sdk as never, CTX);
    const parsed = JSON.parse(service.buildExport(validInput(), PROJECT_ID));
    expect(parsed.genesisInput.refs).toEqual([{ target: PROJECT_ID, rel: 'parent' }]);
    expect(parsed.genesisInput.extra.feedback.feedbackChannel).toBe(FEEDBACK_CHANNEL);
    expect(() => service.buildExport(validInput(), 'bad')).toThrow('形状非法');
    expect(() => service.buildExport(validInput({ title: 'x' }), PROJECT_ID)).toThrow('标题长度');
  });
});

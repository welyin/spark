import { beforeEach, describe, expect, it, vi } from 'vitest';

const mockElectronApi = {
  evidence: {} as any,
  p2p: {} as any,
  plugin: {
    currentRoot: vi.fn(),
    identitySign: vi.fn(),
    identityVerify: vi.fn(),
    syncOrganizationData: vi.fn(),
    listMineOrganizations: vi.fn(),
    docGet: vi.fn(),
    docPut: vi.fn(),
    docDelete: vi.fn(),
    docQuery: vi.fn(),
    docDeclareCollection: vi.fn()
  }
};

describe('createPluginBackend', () => {
  beforeEach(() => {
    vi.resetModules();
    vi.clearAllMocks();
    window.electronAPI = mockElectronApi as any;
  });

  it('exposes runtime.syncOrganizationData and forwards the bound domain', async () => {
    mockElectronApi.plugin.syncOrganizationData.mockResolvedValueOnce({
      orgId: 'org_1',
      attempted: 1,
      pulled: 1
    });

    const { createPluginBackend } = await import('../../plugin-sdk-browser');
    const sdk = createPluginBackend('plugin:spark-example');

    const result = await sdk.runtime.syncOrganizationData('org_1');

    expect(mockElectronApi.plugin.syncOrganizationData).toHaveBeenCalledWith('org_1', 'plugin:spark-example');
    expect(result).toEqual({ orgId: 'org_1', attempted: 1, pulled: 1 });
  });

  it('exposes identity.sign/verify；sign 带域、verify 免域', async () => {
    mockElectronApi.plugin.identitySign.mockResolvedValueOnce({ domain: 'plugin:spark-example', signature: 'sig' });
    mockElectronApi.plugin.identityVerify.mockResolvedValueOnce({ valid: true });

    const { createPluginBackend } = await import('../../plugin-sdk-browser');
    const sdk = createPluginBackend('plugin:spark-example');

    await sdk.identity.sign('payload-1');
    expect(mockElectronApi.plugin.identitySign).toHaveBeenCalledWith('payload-1', 'plugin:spark-example');

    const result = await sdk.identity.verify('payload-1', 'sig', 'pk');
    expect(mockElectronApi.plugin.identityVerify).toHaveBeenCalledWith('payload-1', 'sig', 'pk');
    expect(result).toEqual({ valid: true });
  });

  it('exposes docs.defineCollection and forwards schema with the bound domain', async () => {
    mockElectronApi.plugin.docDeclareCollection.mockResolvedValueOnce({
      collection: 'votes',
      syncStrategy: 'append-only',
      governance: false,
      enableEvidence: true
    });

    const { createPluginBackend } = await import('../../plugin-sdk-browser');
    const sdk = createPluginBackend('plugin:spark-example');

    const declared = await sdk.docs.defineCollection('votes', { syncStrategy: 'append-only' });

    expect(mockElectronApi.plugin.docDeclareCollection).toHaveBeenCalledWith(
      'votes',
      { syncStrategy: 'append-only' },
      'plugin:spark-example'
    );
    expect(declared).toMatchObject({ collection: 'votes', syncStrategy: 'append-only' });
  });

  it('throws when host API is unavailable', async () => {
    // @ts-expect-error 模拟无宿主环境
    delete window.electronAPI;
    const { createPluginBackend } = await import('../../plugin-sdk-browser');
    expect(() => createPluginBackend('plugin:spark-example')).toThrow(/electronAPI is not available/);
  });
});

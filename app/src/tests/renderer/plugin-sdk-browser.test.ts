import { beforeEach, describe, expect, it, vi } from 'vitest';

const mockElectronApi = {
  getDomain: vi.fn(),
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

describe('initializePluginSDK', () => {
  beforeEach(() => {
    vi.resetModules();
    vi.clearAllMocks();
    window.electronAPI = mockElectronApi as any;
    window.history.replaceState({}, '', '/');
  });

  it('exposes runtime.syncOrganizationData and forwards orgId', async () => {
    mockElectronApi.getDomain.mockResolvedValueOnce({ domain: 'plugin:weibo-core' });
    mockElectronApi.plugin.syncOrganizationData.mockResolvedValueOnce({
      orgId: 'org_1',
      attempted: 1,
      pulled: 1
    });

    const { initializePluginSDK } = await import('../../plugin-sdk-browser');
    const sdk = await initializePluginSDK();

    const result = await sdk.runtime.syncOrganizationData('org_1');

    expect(mockElectronApi.plugin.syncOrganizationData).toHaveBeenCalledWith('org_1', undefined);
    expect(result).toEqual({ orgId: 'org_1', attempted: 1, pulled: 1 });
  });

  it('passes pluginDomain fallback when current domain is unavailable', async () => {
    mockElectronApi.getDomain.mockResolvedValueOnce({ domain: 'system' });
    window.history.replaceState({}, '', '/?pluginDomain=plugin:weibo-core');

    const { initializePluginSDK } = await import('../../plugin-sdk-browser');
    const sdk = await initializePluginSDK();

    await sdk.runtime.syncOrganizationData('org_2');

    expect(mockElectronApi.plugin.syncOrganizationData).toHaveBeenCalledWith('org_2', 'plugin:weibo-core');
  });

  it('exposes identity.sign/verify and forwards plugin domain fallback', async () => {
    mockElectronApi.getDomain.mockResolvedValueOnce({ domain: 'system' });
    window.history.replaceState({}, '', '/?pluginDomain=plugin:weibo-core');
    mockElectronApi.plugin.identitySign.mockResolvedValueOnce({ domain: 'plugin:weibo-core', signature: 'sig' });
    mockElectronApi.plugin.identityVerify.mockResolvedValueOnce({ valid: true });

    const { initializePluginSDK } = await import('../../plugin-sdk-browser');
    const sdk = await initializePluginSDK();

    await sdk.identity.sign('payload-1');
    expect(mockElectronApi.plugin.identitySign).toHaveBeenCalledWith('payload-1', 'plugin:weibo-core');

    const result = await sdk.identity.verify('payload-1', 'sig', 'pk');
    expect(mockElectronApi.plugin.identityVerify).toHaveBeenCalledWith('payload-1', 'sig', 'pk');
    expect(result).toEqual({ valid: true });
  });

  it('exposes docs.defineCollection and forwards schema with plugin domain fallback', async () => {
    mockElectronApi.getDomain.mockResolvedValueOnce({ domain: 'system' });
    window.history.replaceState({}, '', '/?pluginDomain=plugin:weibo-core');
    mockElectronApi.plugin.docDeclareCollection.mockResolvedValueOnce({
      collection: 'votes',
      syncStrategy: 'append-only',
      governance: false,
      enableEvidence: true
    });

    const { initializePluginSDK } = await import('../../plugin-sdk-browser');
    const sdk = await initializePluginSDK();

    const declared = await sdk.docs.defineCollection('votes', { syncStrategy: 'append-only' });

    expect(mockElectronApi.plugin.docDeclareCollection).toHaveBeenCalledWith(
      'votes',
      { syncStrategy: 'append-only' },
      'plugin:weibo-core'
    );
    expect(declared).toMatchObject({ collection: 'votes', syncStrategy: 'append-only' });
  });

  it('identity.sign omits pluginDomain when window domain is bound', async () => {
    mockElectronApi.getDomain.mockResolvedValueOnce({ domain: 'plugin:weibo-core' });
    mockElectronApi.plugin.identitySign.mockResolvedValueOnce({ domain: 'plugin:weibo-core', signature: 'sig' });

    const { initializePluginSDK } = await import('../../plugin-sdk-browser');
    const sdk = await initializePluginSDK();

    await sdk.identity.sign('payload-2');
    expect(mockElectronApi.plugin.identitySign).toHaveBeenCalledWith('payload-2', undefined);
  });

  it('rejects in main-window context (no plugin domain anywhere)', async () => {
    // 新壳 getDomain 主窗口恒 null，URL 也无 pluginDomain → 无法解析插件域
    mockElectronApi.getDomain.mockResolvedValueOnce({ domain: null });
    window.history.replaceState({}, '', '/');

    const { initializePluginSDK, getPluginSDK } = await import('../../plugin-sdk-browser');
    await expect(initializePluginSDK()).rejects.toThrow(/Plugin SDK initialization failed/);
    expect(() => getPluginSDK()).toThrow(/not initialized/);
  });
});

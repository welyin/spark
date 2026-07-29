/**
 * vitest 全局环境桩（挂载测试用）：
 * - window.electronAPI：默认任意层级任意方法返回 Promise<null>（多数组件的 onMounted 调用都有 try/catch 兜底）；
 *   少数需要合法返回形状才能渲染的端点在下方显式覆盖。
 * - window.matchMedia：jsdom 无此 API（stores/theme 加载时需要）。
 */
const makeApi = (): any =>
  new Proxy(function () {}, {
    get(target, key) {
      return key in target ? (target as any)[key] : makeApi();
    },
    apply() {
      return Promise.resolve(null);
    }
  });

const api = makeApi();
// 需要合法形状才能正常渲染的端点
api.rootIdentity = {
  status: async () => ({ initialized: true, unlocked: true, rootId: 'root-test', nickname: '测试用户', avatar: null })
};
api.organization = { listMine: async () => [] };
api.plugin = { listCatalog: async () => [] };
api.pluginMarket = { list: async () => [] };
api.p2p = {
  info: async () => ({
    initialized: true,
    started: true,
    peerId: 'peer-test',
    addresses: [],
    connectedPeers: [],
    sparkSyncSubscribers: [],
    error: null
  })
};

(window as any).electronAPI = api;

(window as any).matchMedia =
  (window as any).matchMedia ||
  ((query: string) => ({
    matches: false,
    media: query,
    onchange: null,
    addEventListener() {},
    removeEventListener() {},
    addListener() {},
    removeListener() {},
    dispatchEvent: () => false
  }));

/**
 * 后台入口（内核 QuickJS 沙箱，plugin_system.md「后台运行时」）。
 *
 * 参考实现：注册「回声 Bot」联系人，收到发给它的消息原样回显——演示
 * 后台三要素：bot 注册（spark.ensureBot）、消息监听（spark.onMessage）、
 * 回复（spark.reply）。
 *
 * 运行环境与视图 bundle 完全不同，写作约束：
 * - 无 DOM/无 window/无 SDK 桥——宿主注入的全局只有 `spark`（下方声明）；
 * - 不 import 任何模块（SDK 后台 transport 抽象落地前，本文件须保持
 *   零依赖，bundle 产物为可直接 eval 的纯脚本）。
 */

/** 会话消息载荷（与内核 ChatReceived 事件同构的子集） */
type SparkBackgroundMessage = {
  spaceKey: string;
  conversation: { id: string; peerId: string };
  message: { senderId: string; senderName: string; content: string };
};

/** 宿主注入的后台 API（内核 plugin/runtime.rs PRELUDE 的最小声明） */
declare const spark: {
  /** 注册消息监听（每插件一个；重复调用后者覆盖前者） */
  onMessage: (fn: (payload: SparkBackgroundMessage) => void) => void;
  /** 注册/刷新本插件的 bot 联系人，返回内核拼定的 botRootId */
  ensureBot: (botId: string, displayName: string) => string;
  /** 向消息所属会话写入 bot 回复 */
  reply: (payload: SparkBackgroundMessage, text: string) => unknown;
  /** 写宿主日志（内核 stderr，[plugin:<id>] 前缀） */
  log: (msg: string) => void;
};

const botRootId = spark.ensureBot('echo', '回声 Bot');
spark.log(`echo bot registered: ${botRootId}`);

spark.onMessage((payload) => {
  spark.reply(payload, `echo: ${payload.message.content}`);
});

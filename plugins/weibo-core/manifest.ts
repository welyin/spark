export const WEIBO_CORE_PLUGIN_MANIFEST = {
  id: 'weibo-core',
  domain: 'plugin:weibo-core',
  name: '组织微博基础插件',
  version: '0.1.0',
  entryView: 'default',
  description: '单主管理员可发260字以内短文，组织成员可评论与回复。',
  package: {
    updateManifestUrl: 'https://github.com/welyin/spark/releases/latest/download/spark-plugin-weibo-core-manifest.json',
    packageName: 'spark-plugin-weibo-core-0.1.0.spkg'
  }
} as const;

export type WeiboCorePluginManifest = typeof WEIBO_CORE_PLUGIN_MANIFEST;

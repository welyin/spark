import { definePlugin, type PluginManifest } from '../../packages/plugin-sdk/src';
import WeiboCoreView from './WeiboCoreView.vue';
import manifestJson from './manifest.json';

// JSON import 的类型是放宽后的结构（views.type 推为 string），此处收敛到 PluginManifest
const manifest = manifestJson as PluginManifest;

export default definePlugin({
  manifest,
  setup(ctx) {
    ctx.registerView('default', WeiboCoreView);
  }
});

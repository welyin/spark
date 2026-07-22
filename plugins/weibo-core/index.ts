import { registerPluginView } from '../../app/src/plugin-view-registry';
import WeiboCoreView from './WeiboCoreView.vue';
import { WEIBO_CORE_PLUGIN_MANIFEST } from './manifest';

registerPluginView(
  WEIBO_CORE_PLUGIN_MANIFEST.domain,
  WEIBO_CORE_PLUGIN_MANIFEST.entryView,
  WeiboCoreView
);

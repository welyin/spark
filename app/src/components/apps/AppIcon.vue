<!-- 应用图标统一组件（plugin-dist §2.3「壳层统一回退链」）：
     各展示位（桌面 / Dock / Launchpad / 窗口标题栏 / 市场 / 详情 / 属性窗口 /
     手机桌面 / 全局搜索）不再各自拼装图标，统一走本组件的回退链：
       ① 包内图标：已安装且当前空间已启用（app-enablement per-space 事实源）时，
          异步读包内 manifest.json 的 `icon` 相对路径，经 pluginSourceBaseUrl 拼成
          plugin:// 源 URL 以 <img> 引用（该协议 fail-closed：只服务已安装且启用的包，
          与「启用才出现在桌面」口径天然一致）；
       ② 声明图标：市场条目 icon（spark-plugin.json §2.1），渲染前过
          safeAnnounceIcon 白名单（仅 https:// 与 data:image/）；
       ③ 回退：首字符 + hashGradient 哈希渐变（与 UserAvatar 自动头像同一套色板）。
     包内图标 <img> 加载失败（onerror）直接回退 ③，不再回落 ②（规格口径）。
     本组件只负责「里面画什么」：尺寸/圆角/字体由调用方既有类名控制
     （父组件 scoped 样式穿透到本组件根元素），size 属性为可选的像素级覆盖。 -->
<template>
  <span class="app-icon-root" :style="rootStyle">
    <img v-if="imgSrc" :src="imgSrc" class="app-icon-img" alt="" @error="onImgError" />
    <template v-else>{{ fallbackChar }}</template>
  </span>
</template>

<script lang="ts">
import { computed, defineComponent, ref, watch, type PropType } from 'vue';
import type { PluginManifest } from '../../../../packages/plugin-sdk/src';
import { fetchPluginManifest, isValidPluginId, pluginSourceBaseUrl } from '../../plugin/source';
import { currentSpace } from '../../stores/current-space';
import { isAppEnabledInSpace, type EnablementSpace } from '../../stores/app-enablement';
import { safeAnnounceIcon } from './apps-explore';
import { sanitizePackageIconPath, type AppIconItem } from './app-icon';
import { hashGradient } from '../../utils/palette';

/** manifest 拉取缓存（模块级）：同一插件多展示位共享一次请求；
 *  失败缓存 null，避免整屏图标各自重试（fetchPluginManifest 本身 best-effort） */
const manifestCache = new Map<string, Promise<PluginManifest | null>>();

function loadManifest(pluginId: string): Promise<PluginManifest | null> {
  let cached = manifestCache.get(pluginId);
  if (!cached) {
    cached = fetchPluginManifest(pluginId);
    manifestCache.set(pluginId, cached);
  }
  return cached;
}

export default defineComponent({
  name: 'AppIcon',
  props: {
    /** 市场条目（或其最小子集）；缺省时以 appId + 首字符回退渲染 */
    item: { type: Object as PropType<AppIconItem | null>, default: null },
    /** 无条目时的应用 id（壳层内置窗口等无市场条目的场景） */
    appId: { type: String, default: '' },
    /** 可选像素尺寸覆盖（缺省由调用方类名控制尺寸） */
    size: { type: Number, default: 0 }
  },
  setup(props) {
    const appId = computed(() => props.item?.id ?? props.appId);
    const appName = computed(() => props.item?.name ?? '');
    /** 回退首字符（无名称时退 id 首字符，再退 '?'） */
    const fallbackChar = computed(() => (appName.value || appId.value).slice(0, 1) || '?');

    /** 当前空间（包内图标启用判定的作用域，per-space 事实源） */
    const space = computed<EnablementSpace>(() =>
      currentSpace.value.type === 'org'
        ? { type: 'org', orgId: currentSpace.value.orgId }
        : { type: 'personal' }
    );

    /** ① 包内图标资格：已安装且当前空间已启用（plugin:// 协议 fail-closed 的前端同口径预判；
     *  无条目 / installed 缺省一律按不可用处理） */
    const packageEligible = computed(() => {
      const item = props.item;
      if (!item || item.installed !== true || !isValidPluginId(item.id)) {
        return false;
      }
      return isAppEnabledInSpace(space.value, {
        id: item.id,
        supportedSpaces: item.supportedSpaces,
        enabled: item.enabled ?? false
      });
    });

    /** ② 声明图标（过 safeAnnounceIcon 白名单；空串 = 无） */
    const declaredIcon = computed(() => safeAnnounceIcon(props.item?.icon ?? ''));

    const packageIconUrl = ref('');
    /** <img> 加载失败标记：回退链 ③（规格口径：onerror 不再回落 ②） */
    const imgFailed = ref(false);

    watch(
      [packageEligible, appId],
      async ([eligible, id]) => {
        packageIconUrl.value = '';
        imgFailed.value = false;
        if (!eligible || !id) {
          return;
        }
        // 挂载后异步解析：先显示回退/声明图标，manifest 就位后换上包内图标
        const manifest = await loadManifest(id);
        const iconPath = sanitizePackageIconPath(manifest?.icon);
        // 竞态防护：等待期间条目已切换则不写回
        if (iconPath && appId.value === id && packageEligible.value) {
          packageIconUrl.value = `${pluginSourceBaseUrl(id)}/${iconPath}`;
        }
      },
      { immediate: true }
    );

    const imgSrc = computed(() => {
      if (imgFailed.value) {
        return '';
      }
      return packageIconUrl.value || declaredIcon.value;
    });

    const onImgError = () => {
      imgFailed.value = true;
    };

    const rootStyle = computed(() => {
      const style: Record<string, string> = {};
      if (props.size > 0) {
        style.width = `${props.size}px`;
        style.height = `${props.size}px`;
      }
      // 回退态才铺哈希渐变底色（img 态图标自含底色，规格 §2.3 视觉要求）
      if (!imgSrc.value) {
        style.background = hashGradient(appId.value || appName.value);
      }
      return style;
    });

    return { imgSrc, fallbackChar, rootStyle, onImgError };
  }
});
</script>

<style scoped>
.app-icon-root {
  overflow: hidden;
  flex-shrink: 0;
}

/* 图标图铺满调用方给定尺寸并继承其圆角（壳层统一套圆角，规格 §2.3） */
.app-icon-img {
  display: block;
  width: 100%;
  height: 100%;
  object-fit: cover;
  border-radius: inherit;
}
</style>

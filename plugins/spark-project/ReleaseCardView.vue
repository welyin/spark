<template>
  <section class="release-card">
    <header class="card-header">
      <span class="card-badge" :class="`state-${detail?.state ?? 'unknown'}`">{{ detail?.stateLabel ?? '发布' }}</span>
      <span class="card-title">{{ title }}</span>
    </header>
    <p v-if="detail" class="card-line">渠道：{{ channels }}；资产 {{ detail.release.artifacts.length }} 项（包哈希已随发布单入存证链）</p>
    <p v-if="error" class="card-hint">{{ error }}</p>
    <p v-else-if="!detail" class="card-hint">发布单读取中或尚未同步到本机……</p>
  </section>
</template>

<script lang="ts">
import { computed, defineComponent, onMounted, ref } from 'vue';
import { ensurePluginSDK } from '../../packages/plugin-sdk/src';
import { pluginDisplayName } from './vendor/github.com/welyin/spark/plugins/spark-release-manager/model';
import { ProjectService } from './service';

/**
 * 发布卡片（组合形态渲染面，档一-2）：发布管理件推送的卡片 data 只携带
 * { releaseId, orgId } 引用，本视图经组合服务（namespace='spark-project'，
 * 数据在本插件域）重读发布单与状态。读取失败如实标注，不编造状态。
 */
export default defineComponent({
  name: 'ReleaseCardView',
  props: {
    cardData: {
      type: Object as () => { releaseId?: string; orgId?: string } | undefined,
      required: false,
      default: undefined
    }
  },
  setup(props) {
    const detail = ref<{ release: { pluginId: string; version: string; artifacts: unknown[]; channels: string[] }; stateLabel: string; state: string } | null>(null);
    const error = ref('');

    onMounted(async () => {
      const { releaseId, orgId } = props.cardData ?? {};
      if (!releaseId || !orgId) {
        error.value = '卡片数据缺少发布单引用（releaseId/orgId）';
        return;
      }
      try {
        const sdk = await ensurePluginSDK();
        const service = new ProjectService(sdk);
        // 卡片场景不读名册（免 org:read 依赖）：读侧鉴权集合仅取发布权配置，
        // 配置不可得时状态回落「已登记」如实呈现（不伪造已发布）
        detail.value = await service.getReleaseDetail(orgId, releaseId, []);
        if (!detail.value) {
          error.value = '发布单尚未同步到本机（等待复制收敛）';
        }
      } catch (err) {
        error.value = `发布单读取失败：${(err as Error).message}`;
      }
    });

    const title = computed(() =>
      detail.value ? `${pluginDisplayName(detail.value.release.pluginId)} v${detail.value.release.version}` : '版本发布'
    );
    const channels = computed(() => (detail.value?.release.channels.length ?? 0) > 0 ? detail.value!.release.channels.join('、') : '未登记');
    return { detail, error, title, channels };
  }
});
</script>

<style scoped>
.release-card {
  padding: 12px 14px;
  font-size: 13px;
  color: var(--el-text-color-primary, #303133);
}
.card-header {
  display: flex;
  align-items: center;
  gap: 8px;
}
.card-badge {
  flex: none;
  padding: 2px 8px;
  border-radius: 4px;
  background: var(--el-color-success-light-8, #f0f9eb);
  color: var(--el-color-success, #67c23a);
  font-size: 12px;
}
.card-badge.state-retracted,
.card-badge.state-verify-failed {
  background: var(--el-color-danger-light-8, #fef0f0);
  color: var(--el-color-danger, #f56c6c);
}
.card-title {
  font-weight: 600;
}
.card-line,
.card-hint {
  margin: 8px 0 0;
  color: var(--el-text-color-secondary, #909399);
  font-size: 12px;
}
</style>

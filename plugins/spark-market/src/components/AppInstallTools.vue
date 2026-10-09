<!-- 应用安装工具入口（移植自壳层 AppInstallTools）：
     「按仓库地址安装」（解析声明文件 → 确认安装）与「导入 .spkg 文件」
     （网络差降级侧载，包哈希供核对）。
     插件版差异：文件选择对话框由 sdk.market.pickSpkg 壳层代开（沙箱 iframe
     无系统对话框能力）；inspect/import 走 sdk.market 同名方法。 -->
<template>
  <div class="app-install-tools">
    <template v-if="!hideButtons">
      <el-button @click="openRepoDialog">按仓库地址安装</el-button>
      <el-button @click="openSideload">导入 .spkg 文件</el-button>
    </template>

    <el-dialog v-model="repoDialogVisible" append-to-body title="按仓库地址安装" width="480">
      <div class="repo-install-dialog">
        <el-input
          v-model="repoIdInput"
          placeholder="如 github.com/owner/repo（支持 gitlab.com / gitee.com）"
          clearable
          @keyup.enter="resolveRepo"
        >
          <template #append>
            <el-button :loading="repoResolving" @click="resolveRepo">解析</el-button>
          </template>
        </el-input>
        <el-alert v-if="repoError" :title="repoError" type="error" :closable="false" show-icon />
        <div v-if="repoPreview" class="repo-preview">
          <div class="repo-preview-head">
            <img v-if="repoPreview.icon" :src="repoPreview.icon" class="repo-preview-icon" alt="" />
            <span v-else class="repo-preview-icon repo-preview-icon-fallback">{{ repoPreview.name.slice(0, 1) }}</span>
            <div>
              <h3>{{ repoPreview.name }} <el-tag size="small" effect="plain">v{{ repoPreview.version }}</el-tag></h3>
              <p class="repo-preview-id">{{ repoPreview.id }}</p>
            </div>
          </div>
          <p class="repo-preview-summary">{{ repoPreview.summary }}</p>
          <p v-if="repoPlatformsText" class="repo-preview-permissions">运行平台：{{ repoPlatformsText }}</p>
          <p v-if="repoPreview.permissions.length > 0" class="repo-preview-permissions">
            声明权限：{{ repoPreview.permissions.join('、') }}
          </p>
        </div>
      </div>
      <template #footer>
        <el-button @click="repoDialogVisible = false">取消</el-button>
        <el-button type="primary" :disabled="!repoPreview" @click="confirmRepoInstall">确认安装</el-button>
      </template>
    </el-dialog>

    <!-- .spkg 侧载导入（网络差降级）：文件选择 → 预览（名称/版本/权限/包哈希供核对）→ 复核导入 -->
    <el-dialog v-model="sideloadVisible" append-to-body title="导入 .spkg 插件包" width="480">
      <div v-if="sideloadPreview" class="repo-install-dialog">
        <div class="repo-preview-head">
          <span class="repo-preview-icon repo-preview-icon-fallback">{{ sideloadPreview.name.slice(0, 1) }}</span>
          <div>
            <h3>{{ sideloadPreview.name }} <el-tag size="small" effect="plain">v{{ sideloadPreview.version }}</el-tag></h3>
            <p class="repo-preview-id">{{ sideloadPreview.pluginId }}</p>
          </div>
        </div>
        <p v-if="sideloadPreview.permissions.length > 0" class="repo-preview-permissions">
          声明权限：{{ sideloadPreview.permissions.join('、') }}
        </p>
        <p class="repo-preview-permissions">支持空间：{{ sideloadSpacesText }}</p>
        <p v-if="sideloadPlatformsText" class="repo-preview-permissions">运行平台：{{ sideloadPlatformsText }}</p>
        <p class="repo-preview-permissions">文件：{{ sideloadPreview.fileName }}（{{ sideloadSizeText }}）</p>
        <el-alert type="warning" :closable="false" show-icon>
          <template #title>导入前请与发布者公布的哈希核对</template>
          <p class="sideload-hash">sha256：{{ sideloadPreview.sha256 }}</p>
        </el-alert>
        <el-alert v-if="sideloadError" :title="sideloadError" type="error" :closable="false" show-icon />
      </div>
      <template #footer>
        <el-button @click="sideloadVisible = false">取消</el-button>
        <el-button type="primary" :loading="sideloadImporting" @click="confirmSideload">核对无误，导入安装</el-button>
      </template>
    </el-dialog>
  </div>
</template>

<script lang="ts">
import { computed, defineComponent, ref } from 'vue';
import { ElMessage, ElMessageBox } from 'element-plus';
import type { PluginMarketRepoDeclaration, PluginMarketSideloadPreview } from '../../../../packages/plugin-sdk/src';
import { marketApi } from '../sdk-host';
import { platformsText, supportedSpacesText } from '../market-store';

export default defineComponent({
  name: 'AppInstallTools',
  props: {
    hideButtons: { type: Boolean, default: false }
  },
  emits: ['install-repo', 'sideloaded'],
  setup(_, { emit }) {
    // 仓库锚定安装（plugin-dist）：解析 spark-plugin.json 预览，确认后由父组件安装
    const repoDialogVisible = ref(false);
    const repoIdInput = ref('');
    const repoResolving = ref(false);
    const repoError = ref('');
    const repoPreview = ref<PluginMarketRepoDeclaration | null>(null);

    const openRepoDialog = () => {
      repoIdInput.value = '';
      repoError.value = '';
      repoPreview.value = null;
      repoDialogVisible.value = true;
    };

    const resolveRepo = async () => {
      const market = marketApi();
      const id = repoIdInput.value.trim();
      if (!id || !market) {
        return;
      }
      repoResolving.value = true;
      repoError.value = '';
      repoPreview.value = null;
      try {
        repoPreview.value = await market.resolveRepo(id);
      } catch (error) {
        repoError.value = `解析失败：${error}`;
      } finally {
        repoResolving.value = false;
      }
    };

    const confirmRepoInstall = () => {
      if (!repoPreview.value) {
        return;
      }
      const declaration = repoPreview.value;
      repoDialogVisible.value = false;
      emit('install-repo', declaration);
    };

    // .spkg 侧载导入（网络差降级）：壳层代开文件选择 → inspect 预览 → import 复核落状态
    const sideloadVisible = ref(false);
    const sideloadPath = ref('');
    const sideloadPreview = ref<PluginMarketSideloadPreview | null>(null);
    const sideloadError = ref('');
    const sideloadImporting = ref(false);

    const openSideload = async () => {
      const market = marketApi();
      if (!market) {
        ElMessage.error('市场接口不可用（非插件运行上下文）');
        return;
      }
      try {
        const path = await market.pickSpkg();
        if (!path) {
          return; // 用户取消选择
        }
        sideloadPath.value = path;
        sideloadError.value = '';
        sideloadPreview.value = await market.inspectLocal(path);
        sideloadVisible.value = true;
      } catch (error) {
        ElMessage.error(`读取插件包失败：${error}`);
      }
    };

    const sideloadSizeText = computed(() => {
      const size = sideloadPreview.value?.size ?? 0;
      return size >= 1024 * 1024 ? `${(size / 1024 / 1024).toFixed(1)} MB` : `${Math.ceil(size / 1024)} KB`;
    });

    const sideloadSpacesText = computed(() => supportedSpacesText(sideloadPreview.value?.supportedSpaces));
    const repoPlatformsText = computed(() => platformsText(repoPreview.value?.requires));
    const sideloadPlatformsText = computed(() => platformsText(sideloadPreview.value?.requires));

    const confirmSideload = async () => {
      const market = marketApi();
      const preview = sideloadPreview.value;
      if (!preview || !market) {
        return;
      }
      sideloadImporting.value = true;
      sideloadError.value = '';
      try {
        await market.importLocal(sideloadPath.value, preview.sha256);
        sideloadVisible.value = false;
        ElMessage.success(`「${preview.name}」导入成功，启用后即可使用`);
        emit('sideloaded');
      } catch (error) {
        // 信任降级覆盖守卫（I2）：后端结构化前缀 → 确认框标注「将覆盖现有 xx 安装」，同意后带 confirmOverwrite 重试。
        // 桥通路错误是 Error 实例（message 含内核原始串），须取 message 再判前缀
        const message = error instanceof Error ? error.message : `${error}`;
        if (message.startsWith('Sideload overwrite requires confirmation')) {
          const trust = /trust=([a-z-]+)/.exec(message)?.[1] ?? '';
          const trustLabel = trust === 'signed' ? '签名信任链' : trust === 'repo-anchored' ? '仓库锚定' : trust;
          try {
            await ElMessageBox.confirm(
              `将覆盖现有 ${trustLabel} 安装，信任层级降级为侧载导入（仅哈希核对）。确认继续？`,
              '覆盖已有安装',
              { confirmButtonText: '覆盖导入', cancelButtonText: '取消', type: 'warning' }
            );
          } catch {
            sideloadImporting.value = false;
            return; // 用户取消覆盖
          }
          try {
            await market.importLocal(sideloadPath.value, preview.sha256, true);
            sideloadVisible.value = false;
            ElMessage.success(`「${preview.name}」导入成功，启用后即可使用`);
            emit('sideloaded');
          } catch (retryError) {
            sideloadError.value = `导入失败：${retryError}`;
          }
        } else {
          sideloadError.value = `导入失败：${message}`;
        }
      } finally {
        sideloadImporting.value = false;
      }
    };

    return {
      repoDialogVisible,
      repoIdInput,
      repoResolving,
      repoError,
      repoPreview,
      openRepoDialog,
      resolveRepo,
      confirmRepoInstall,
      sideloadVisible,
      sideloadPreview,
      sideloadError,
      sideloadImporting,
      sideloadSizeText,
      sideloadSpacesText,
      repoPlatformsText,
      sideloadPlatformsText,
      openSideload,
      confirmSideload
    };
  }
});
</script>

<style scoped>
.app-install-tools {
  display: contents;
}
</style>

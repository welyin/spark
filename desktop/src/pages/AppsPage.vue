<template>
  <section class="card-page">
    <header class="page-header">
      <div class="page-header-main">
        <p class="eyebrow">应用</p>
        <h1>插件市场</h1>
        <p class="lede">支持安装、升级、启停，并可筛选基础插件作为组织创建基座。</p>
      </div>
      <div class="page-header-actions">
        <el-switch v-model="foundationOnly" active-text="仅看基础插件" />
        <el-button :loading="busyGlobal" @click="refreshMarket">刷新市场</el-button>
        <el-button type="primary" :loading="busyGlobal" @click="checkAllUpdates">检查全部更新</el-button>
      </div>
    </header>

    <el-alert v-if="message" :title="message" type="info" :closable="false" show-icon />

    <el-row :gutter="12" class="market-grid">
      <el-col v-for="item in filteredItems" :key="item.id" :xs="24" :sm="12" :lg="8">
        <el-card shadow="never" class="plugin-card">
          <template #header>
            <div class="plugin-head">
              <div>
                <h3>{{ item.name }}</h3>
                <p>{{ item.domain }}</p>
              </div>
              <el-tag :type="item.category === 'foundation' ? 'danger' : 'info'">
                {{ item.category === 'foundation' ? '基础插件' : '业务插件' }}
              </el-tag>
            </div>
          </template>

          <p class="desc">{{ item.description }}</p>

          <div class="meta">
            <span>内置版本：{{ item.version }}</span>
            <span>已装版本：{{ item.installedVersion || '-' }}</span>
            <span>最新版本：{{ item.latestVersion || '-' }}</span>
          </div>

          <div class="state-row">
            <el-tag :type="item.installed ? 'success' : 'warning'">{{ item.installed ? '已安装' : '未安装' }}</el-tag>
            <el-tag v-if="item.updateAvailable" type="danger">可升级</el-tag>
            <el-tag v-if="item.installed && !item.enabled" type="warning">已停用</el-tag>
          </div>

          <div class="actions">
            <el-button
              type="primary"
              size="small"
              :loading="busyByPlugin[item.id] === 'install'"
              :disabled="item.installed"
              @click="installPlugin(item.id)"
            >
              一键安装
            </el-button>
            <el-button
              type="warning"
              size="small"
              :loading="busyByPlugin[item.id] === 'upgrade'"
              :disabled="!item.installed || !item.updateAvailable"
              @click="upgradePlugin(item.id)"
            >
              升级
            </el-button>
            <el-button
              size="small"
              :loading="busyByPlugin[item.id] === 'toggle'"
              :disabled="!item.installed"
              @click="toggleEnabled(item)"
            >
              {{ item.enabled ? '停用' : '启用' }}
            </el-button>
            <el-button
              size="small"
              :disabled="!item.installed || !item.enabled"
              @click="openInstalledPlugin(item)"
            >
              打开
            </el-button>
            <el-button
              size="small"
              :loading="busyByPlugin[item.id] === 'check'"
              @click="checkOneUpdate(item.id)"
            >
              检查更新
            </el-button>
          </div>

          <p class="reason">{{ item.lastCheckReason }}</p>
        </el-card>
      </el-col>
    </el-row>
  </section>
</template>

<script lang="ts">
import { computed, defineComponent, onMounted, ref } from 'vue';
import { ElMessage, ElMessageBox } from 'element-plus';

const PERMISSION_LABELS: Record<string, string> = {
  'storage:read': '读取本域数据',
  'storage:write': '写入本域数据',
  'org:read': '读取组织信息',
  'org:sync': '同步组织数据',
  'network:broadcast': '网络广播',
  'proof:verify': '存证核验',
  'identity:sign': '域身份签名'
};

export type OpenPluginTabPayload = {
  pluginDomain: string;
  pluginView: string;
  title: string;
  icon: string;
  pluginContext?: {
    orgId?: string;
  };
};

export default defineComponent({
  name: 'AppsPage',
  emits: ['open-plugin-tab'],
  setup(_, { emit }) {
    const items = ref<Array<{
      id: string;
      domain: string;
      name: string;
      description: string;
      category: 'foundation' | 'business';
      version: string;
      views: string[];
      permissions: string[];
      installed: boolean;
      enabled: boolean;
      installedVersion: string | null;
      latestVersion: string | null;
      updateAvailable: boolean;
      lastCheckedAt: number | null;
      lastCheckReason: string;
    }>>([]);
    const message = ref('');
    const foundationOnly = ref(false);
    const busyGlobal = ref(false);
    const busyByPlugin = ref<Record<string, '' | 'install' | 'upgrade' | 'toggle' | 'check'>>({});

    const filteredItems = computed(() => {
      if (!foundationOnly.value) {
        return items.value;
      }
      return items.value.filter((item) => item.category === 'foundation');
    });

    const setPluginBusy = (pluginId: string, action: '' | 'install' | 'upgrade' | 'toggle' | 'check') => {
      busyByPlugin.value = {
        ...busyByPlugin.value,
        [pluginId]: action
      };
    };

    const refreshMarket = async () => {
      busyGlobal.value = true;
      try {
        items.value = await window.electronAPI.pluginMarket.list();
      } catch (error) {
        message.value = `加载插件市场失败：${error}`;
      } finally {
        busyGlobal.value = false;
      }
    };

    const checkAllUpdates = async () => {
      busyGlobal.value = true;
      try {
        await window.electronAPI.pluginMarket.checkUpdates();
        await refreshMarket();
        ElMessage.success('更新检查完成');
      } catch (error) {
        message.value = `检查更新失败：${error}`;
      } finally {
        busyGlobal.value = false;
      }
    };

    const checkOneUpdate = async (pluginId: string) => {
      setPluginBusy(pluginId, 'check');
      try {
        await window.electronAPI.pluginMarket.checkUpdates(pluginId);
        await refreshMarket();
      } catch (error) {
        message.value = `检查更新失败：${error}`;
      } finally {
        setPluginBusy(pluginId, '');
      }
    };

    const installPlugin = async (pluginId: string) => {
      const target = items.value.find((entry) => entry.id === pluginId);
      const declared = target?.permissions ?? [];
      if (declared.length > 0) {
        const labels = declared.map((permission) => `${PERMISSION_LABELS[permission] ?? permission}（${permission}）`).join('、');
        try {
          await ElMessageBox.confirm(
            `该插件声明以下高级权限：${labels}。安装即视为授权，运行时可越权调用将被系统拦截。`,
            `授权安装 ${target?.name ?? pluginId}`,
            { confirmButtonText: '授权并安装', cancelButtonText: '取消', type: 'warning' }
          );
        } catch {
          return; // 用户取消授权
        }
      }
      setPluginBusy(pluginId, 'install');
      try {
        await window.electronAPI.pluginMarket.install(pluginId);
        await refreshMarket();
        ElMessage.success('插件安装成功');
      } catch (error) {
        message.value = `插件安装失败：${error}`;
      } finally {
        setPluginBusy(pluginId, '');
      }
    };

    const upgradePlugin = async (pluginId: string) => {
      setPluginBusy(pluginId, 'upgrade');
      try {
        await window.electronAPI.pluginMarket.upgrade(pluginId);
        await refreshMarket();
        ElMessage.success('插件升级成功');
      } catch (error) {
        message.value = `插件升级失败：${error}`;
      } finally {
        setPluginBusy(pluginId, '');
      }
    };

    const toggleEnabled = async (item: { id: string; enabled: boolean }) => {
      setPluginBusy(item.id, 'toggle');
      try {
        await window.electronAPI.pluginMarket.setEnabled(item.id, !item.enabled);
        await refreshMarket();
      } catch (error) {
        message.value = `插件启停失败：${error}`;
      } finally {
        setPluginBusy(item.id, '');
      }
    };

    const openInstalledPlugin = (item: { domain: string; name: string }) => {
      emit('open-plugin-tab', {
        pluginDomain: item.domain,
        pluginView: 'default',
        title: item.name,
        icon: item.name.slice(0, 1)
      } as OpenPluginTabPayload);
    };

    onMounted(() => {
      void refreshMarket();
    });

    return {
      message,
      foundationOnly,
      busyGlobal,
      busyByPlugin,
      filteredItems,
      refreshMarket,
      checkAllUpdates,
      checkOneUpdate,
      installPlugin,
      upgradePlugin,
      toggleEnabled,
      openInstalledPlugin
    };
  }
});
</script>

<style scoped src="../styles/pages/apps.css"></style>

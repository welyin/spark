<!-- HTTP 代理设置（真实生效，非 mock）：启用开关 + 主机/端口输入 + 保存按钮。
     后端 src-tauri proxy.rs：持久化 <data_dir>/spark-proxy.json + 注入
     SPARK_PROXY/HTTPS_PROXY/ALL_PROXY 环境变量；市场 OnceLock 客户端与
     updater 客户端等已建立连接不追溯，保存后 toast 提示重启生效。
     嵌入网络状态模块详情卡（NetworkModule 的 proxy 分类） -->
<template>
  <div class="proxy-settings">
    <p class="hint">
      检查更新与插件市场下载需要访问 GitHub。直连失败（超时/连接被重置）时，可配置本地 HTTP 代理。
    </p>
    <div class="proxy-row">
      <span>启用代理</span>
      <el-switch v-model="enabled" :disabled="!bridgeAvailable" />
    </div>
    <template v-if="enabled">
      <div class="proxy-row">
        <span>主机</span>
        <el-input
          v-model="host"
          size="small"
          placeholder="如 127.0.0.1"
          class="proxy-input"
          :disabled="!bridgeAvailable"
        />
      </div>
      <div class="proxy-row">
        <span>端口</span>
        <el-input
          v-model="port"
          size="small"
          placeholder="如 29290"
          class="proxy-input"
          :disabled="!bridgeAvailable"
        />
      </div>
    </template>
    <div class="proxy-actions">
      <el-button
        type="primary"
        size="small"
        :loading="saving"
        :disabled="!bridgeAvailable || saveDisabled"
        @click="save"
      >
        保存
      </el-button>
      <span v-if="!bridgeAvailable" class="proxy-message">当前运行环境不支持代理设置</span>
    </div>
  </div>
</template>

<script lang="ts">
import { computed, defineComponent, onMounted, ref } from 'vue';
import { ElMessage } from 'element-plus';
import { errorMessage } from '../../utils/ipc';
import { isPortPlausible, joinProxy, splitProxy } from './proxy-settings';

export default defineComponent({
  name: 'ProxySettings',
  setup() {
    const enabled = ref(false);
    const host = ref('');
    const port = ref('');
    const saving = ref(false);
    // 非 Tauri 环境（单测/纯前端预览）无桥接：表单只读禁用
    const bridgeAvailable = computed(() => !!window.electronAPI?.system?.getProxy);

    // 保存按钮可用性：启用时主机非空且端口像样（完整校验由后端保存时把关）
    const saveDisabled = computed(
      () => enabled.value && (!host.value.trim() || !isPortPlausible(port.value))
    );

    /** 读取回显当前设置（"host:port" → 开关 + 两个输入框） */
    const load = async () => {
      if (!bridgeAvailable.value) {
        return;
      }
      try {
        const current = await window.electronAPI.system.getProxy();
        const parsed = splitProxy(current);
        enabled.value = parsed !== null;
        if (parsed) {
          host.value = parsed.host;
          port.value = parsed.port;
        }
      } catch {
        // 读取失败保留默认（关闭态），保存时后端错误会提示
      }
    };

    const save = async () => {
      saving.value = true;
      try {
        // 关闭时传空串（后端语义：空串=清除）；启用时拼 host:port
        const value = enabled.value ? joinProxy(host.value, port.value) : '';
        await window.electronAPI.system.setProxy(value);
        ElMessage.success(
          enabled.value
            ? '代理已保存；市场等已建立的连接需重启应用后生效'
            : '代理已关闭；市场等已建立的连接需重启应用后生效'
        );
      } catch (error) {
        ElMessage.error(`代理保存失败：${errorMessage(error)}`);
      } finally {
        saving.value = false;
      }
    };

    onMounted(load);

    return { enabled, host, port, saving, bridgeAvailable, saveDisabled, save };
  }
});
</script>

<style scoped>
/* 与系统设置「通用」卡的 settings-row 同风格：左标签右控件 */
.proxy-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  padding: 8px 0;
  font-size: 13px;
  color: var(--spark-text-1);
}

.proxy-row + .proxy-row {
  border-top: 1px solid var(--spark-border-light);
}

.proxy-input {
  width: 200px;
}

.proxy-actions {
  display: flex;
  align-items: center;
  gap: 10px;
  margin-top: 14px;
}

.proxy-message {
  font-size: 12px;
  color: var(--spark-text-2, #909399);
}
</style>

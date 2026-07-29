<!-- 职责：「发现公开组织」——主流程：输入对方公开的 DHT 组织地址，在 DHT 网络中解析该组织并与其网关
     建立联系（organization.resolveAddress）；次级：按展示名/地址片段搜索本机已缓存的公开组织记录 -->
<template>
  <el-card shadow="never" class="panel-card">
    <template #header>
      <h2>发现公开组织</h2>
    </template>

    <!-- 主流程：DHT 组织地址解析 -->
    <p class="hint">输入对方公开的 DHT 组织地址，即可在 DHT 网络中找到该组织，并与其网关建立联系。</p>
    <div class="org-search-row">
      <el-input
        v-model="orgAddress"
        placeholder="输入组织的 DHT 地址"
        clearable
        @keyup.enter="resolveOrg"
      />
      <el-button type="primary" :loading="resolving" @click="resolveOrg">查找</el-button>
    </div>
    <el-alert
      v-if="resolveFailed"
      title="没有找到该组织：请确认 DHT 地址无误，且对方已开启公开并保持网关在线。"
      type="warning"
      :closable="false"
      show-icon
      class="discover-block"
    />
    <div v-else-if="resolved" class="org-search-results discover-block">
      <div class="org-search-item">
        <div class="org-search-item-main">
          <strong>{{ resolved.displayName || resolved.orgId }}</strong>
          <el-tag size="small" type="success">已与其网关建立联系</el-tag>
        </div>
        <div class="org-search-item-meta">
          <span class="hint">网关：</span>
          <el-tag v-for="gateway in resolved.gateways" :key="gateway" size="small" type="info">
            {{ gateway.slice(0, 12) }}…
          </el-tag>
        </div>
      </div>
    </div>

    <!-- 次级：本机已缓存的公开组织记录搜索 -->
    <el-divider class="discover-divider" />
    <p class="hint">或搜索本机已发现的公开组织（来自 DHT 与 gossip 的组织地址记录）。</p>
    <div class="org-search-row">
      <el-input
        v-model="orgSearchKeyword"
        placeholder="输入组织展示名或组织地址片段"
        clearable
        @keyup.enter="runOrgSearch"
      />
      <el-button :loading="orgSearching" @click="runOrgSearch">搜索</el-button>
    </div>
    <el-empty
      v-if="orgSearched && orgSearchResults.length === 0"
      description="没有找到匹配的公开组织。对方开启公开并在线发布后才会出现在这里。"
    />
    <div v-else-if="orgSearchResults.length" class="org-search-results">
      <div v-for="record in orgSearchResults" :key="record.orgAddress" class="org-search-item">
        <div class="org-search-item-main">
          <strong>{{ record.displayName || record.orgId }}</strong>
          <code class="org-address-code">{{ record.orgAddress.slice(0, 12) }}…</code>
        </div>
        <div class="org-search-item-meta">
          <el-tag v-for="gateway in record.gateways" :key="gateway" size="small" type="info">
            {{ gateway.slice(0, 12) }}…
          </el-tag>
        </div>
      </div>
    </div>
  </el-card>
</template>

<script lang="ts">
import { defineComponent, ref } from 'vue';
import { ElMessage } from 'element-plus';
import type { OrgAddressRecordDto } from '../../api';

export default defineComponent({
  name: 'DiscoverOrgsPanel',
  setup() {
    // 主流程：按 DHT 组织地址解析（找到组织并与其网关建立联系）
    const orgAddress = ref('');
    const resolved = ref<OrgAddressRecordDto | null>(null);
    const resolving = ref(false);
    const resolveFailed = ref(false);

    const resolveOrg = async () => {
      const address = orgAddress.value.trim();
      if (!address) {
        ElMessage.warning('请输入组织的 DHT 地址');
        return;
      }
      resolving.value = true;
      resolveFailed.value = false;
      resolved.value = null;
      try {
        const record = await window.electronAPI.organization.resolveAddress(address);
        if (record) {
          resolved.value = record;
        } else {
          resolveFailed.value = true;
        }
      } catch (error) {
        resolveFailed.value = true;
        ElMessage.error(`查找组织失败：${error}`);
      } finally {
        resolving.value = false;
      }
    };

    // 次级：本机缓存的组织地址记录搜索
    const orgSearchKeyword = ref('');
    const orgSearchResults = ref<OrgAddressRecordDto[]>([]);
    const orgSearching = ref(false);
    const orgSearched = ref(false);

    const runOrgSearch = async () => {
      orgSearching.value = true;
      try {
        orgSearchResults.value = await window.electronAPI.organization.searchKnown(
          orgSearchKeyword.value.trim()
        );
        orgSearched.value = true;
      } catch (error) {
        ElMessage.error(`搜索公开组织失败：${error}`);
      } finally {
        orgSearching.value = false;
      }
    };

    return {
      orgAddress,
      resolved,
      resolving,
      resolveFailed,
      resolveOrg,
      orgSearchKeyword,
      orgSearchResults,
      orgSearching,
      orgSearched,
      runOrgSearch
    };
  }
});
</script>

<style scoped>
.discover-block {
  margin-top: 12px;
}

.discover-divider {
  margin: 20px 0 12px;
}
</style>

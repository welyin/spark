<!-- 职责：「发现公开组织」搜索卡片——按展示名/组织地址搜索本地已缓存的公开组织记录 -->
<template>
  <!-- 发现公开组织：按展示名/组织地址搜索本地已缓存的公开组织记录 -->
  <el-card shadow="never" class="panel-card">
    <template #header>
      <h2>发现公开组织</h2>
    </template>
    <div class="org-search-row">
      <el-input
        v-model="orgSearchKeyword"
        placeholder="输入组织展示名或组织地址片段"
        clearable
        @keyup.enter="runOrgSearch"
      />
      <el-button type="primary" :loading="orgSearching" @click="runOrgSearch">搜索</el-button>
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
    <p v-else class="hint">搜索本机已发现的公开组织（来自 DHT 与 gossip 的组织地址记录）。</p>
  </el-card>
</template>

<script lang="ts">
import { defineComponent, ref } from 'vue';
import { ElMessage } from 'element-plus';
import type { OrgAddressRecordDto } from '../../api';

export default defineComponent({
  name: 'DiscoverOrgsPanel',
  setup() {
    // 发现公开组织：本机缓存的组织地址记录搜索
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
      orgSearchKeyword,
      orgSearchResults,
      orgSearching,
      orgSearched,
      runOrgSearch
    };
  }
});
</script>

<!-- 文件管理插件根组件（A42）：个人空间与组织域统一的文件/照片管理。
     - 文件本体：sdk.content 内容面（cid 内容寻址；上传即做种，
       pinRoot('user-pin') 持有；下载本地未命中经 Kad provider 拉取）；
     - 元数据：sdk.data 集合 spark-files:files（写库即同步，空间边界由桥注入）。
     界面：全部文件（列表）/ 照片（网格）两 tab + 搜索 + 上传 + 下载/删除。
     已知边界（见任务报告缺口）：聊天文件消息不互通；无目录层级。
     下载走 sdk.sys.saveFile 壳层代存（A42 评审修复：沙箱 iframe 无
     allow-downloads，Blob 锚点下载被静默拦截；同 market.pickSpkg 先例）。 -->
<template>
  <section class="files-root">
    <header class="files-header">
      <el-tabs v-model="activeTab" class="files-tabs">
        <el-tab-pane label="全部文件" name="all" />
        <el-tab-pane label="照片" name="photos" />
      </el-tabs>
      <div class="files-header-actions">
        <el-input
          v-model="keyword"
          placeholder="搜索文件名"
          clearable
          class="files-search"
        />
        <el-button type="primary" :loading="uploading" @click="triggerSelect">
          {{ uploading ? '上传中...' : '上传文件' }}
        </el-button>
        <input ref="fileInput" type="file" multiple class="hidden-input" @change="onFilesSelected" />
      </div>
    </header>

    <el-alert v-if="loadError" :title="loadError" type="error" :closable="false" show-icon />

    <!-- 全部文件：列表 -->
    <div v-if="activeTab === 'all'" class="files-body">
      <el-empty v-if="loading" description="正在加载文件列表..." />
      <el-empty v-else-if="visibleFiles.length === 0" description="还没有文件，点右上角「上传文件」开始。" />
      <el-table v-else :data="visibleFiles" row-key="cid" class="files-table">
        <el-table-column prop="name" label="文件名" min-width="220" show-overflow-tooltip />
        <el-table-column label="类型" width="90">
          <template #default="{ row }">{{ fileTypeLabel(row.mime) }}</template>
        </el-table-column>
        <el-table-column label="大小" width="100">
          <template #default="{ row }">{{ formatBytes(row.size) }}</template>
        </el-table-column>
        <el-table-column label="上传时间" width="150">
          <template #default="{ row }">{{ formatDate(row.createdAt) }}</template>
        </el-table-column>
        <el-table-column label="操作" width="130">
          <template #default="{ row }">
            <el-button text type="primary" size="small" :loading="busyCid === row.cid" @click="download(row)">下载</el-button>
            <el-button text type="danger" size="small" :loading="busyCid === row.cid" @click="remove(row)">删除</el-button>
          </template>
        </el-table-column>
      </el-table>
    </div>

    <!-- 照片：网格 -->
    <div v-else class="files-body">
      <el-empty v-if="loading" description="正在加载照片..." />
      <el-empty v-else-if="visiblePhotos.length === 0" description="还没有照片。上传 image/* 文件即归入照片。" />
      <div v-else class="photos-grid">
        <div v-for="photo in visiblePhotos" :key="photo.cid" class="photo-cell" :title="photo.name">
          <img v-if="thumbUrls[photo.cid]" :src="thumbUrls[photo.cid]" :alt="photo.name" class="photo-img" />
          <div v-else class="photo-loading">加载中…</div>
          <div class="photo-actions">
            <el-button text type="primary" size="small" @click="download(photo)">下载</el-button>
            <el-button text type="danger" size="small" @click="remove(photo)">删除</el-button>
          </div>
        </div>
      </div>
    </div>
  </section>
</template>

<script lang="ts">
import { computed, defineComponent, onMounted, ref, watch } from 'vue';
import { ElMessage, ElMessageBox } from 'element-plus';
import { contentApi, dataApi, sysApi } from './sdk-host';
import {
  dataUrlToBase64,
  downloadRecord,
  FILE_PIN_ROOT,
  FILES_COLLECTION,
  FILES_COLLECTION_DECLARATION,
  fileTypeLabel,
  filterByKeyword,
  filterPhotos,
  formatBytes,
  formatDate,
  normalizeRecords,
  sortFiles,
  type SparkFileRecord
} from './files-store';

function readFileAsBase64(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(dataUrlToBase64(String(reader.result)));
    reader.onerror = () => reject(new Error(`读取文件 ${file.name} 失败`));
    reader.readAsDataURL(file);
  });
}

/** base64 → object URL（照片缩略图展示用；下载已改走 sdk.sys.saveFile 壳层代存） */
function base64ToObjectUrl(base64: string, mime: string): string {
  const binary = atob(base64);
  const bytes = new Uint8Array(binary.length);
  for (let index = 0; index < binary.length; index += 1) {
    bytes[index] = binary.charCodeAt(index);
  }
  return URL.createObjectURL(new Blob([bytes], { type: mime || 'application/octet-stream' }));
}

export default defineComponent({
  name: 'FilesApp',
  setup() {
    const activeTab = ref<'all' | 'photos'>('all');
    const keyword = ref('');
    const records = ref<SparkFileRecord[]>([]);
    const thumbUrls = ref<Record<string, string>>({});
    const loading = ref(false);
    const uploading = ref(false);
    const busyCid = ref('');
    const loadError = ref('');
    const fileInput = ref<HTMLInputElement | null>(null);

    const sortedAll = computed(() => sortFiles(records.value));
    const visibleFiles = computed(() => filterByKeyword(sortedAll.value, keyword.value));
    const visiblePhotos = computed(() => filterByKeyword(filterPhotos(sortedAll.value), keyword.value));

    const reload = async () => {
      const data = dataApi();
      if (!data) {
        return;
      }
      loading.value = true;
      try {
        // 集合声明幂等（写前必须声明；重复声明与首次一致即放行）
        await data.declareCollection(FILES_COLLECTION_DECLARATION);
        const result = await data.query(FILES_COLLECTION, { limit: 2000 });
        records.value = normalizeRecords(result.items);
        loadError.value = '';
      } catch (error) {
        loadError.value = `加载文件列表失败：${error}`;
      } finally {
        loading.value = false;
      }
    };

    const triggerSelect = () => {
      fileInput.value?.click();
    };

    const onFilesSelected = async (event: Event) => {
      const input = event.target as HTMLInputElement;
      const files = [...(input.files ?? [])];
      input.value = '';
      const data = dataApi();
      const content = contentApi();
      if (!data || !content || files.length === 0) {
        return;
      }
      uploading.value = true;
      let succeeded = 0;
      try {
        for (const file of files) {
          try {
            const base64 = await readFileAsBase64(file);
            // 本体入内容面（幂等：同内容同 cid）并声明持有
            const { cid } = await content.saveBlob(base64);
            await content.pinRoot(cid, FILE_PIN_ROOT);
            // 元数据落集合（cid 即键；同名重传 = 覆盖同键，天然去重）
            await data.save(FILES_COLLECTION, cid, {
              cid,
              name: file.name,
              size: file.size,
              mime: file.type || '',
              createdAt: Date.now()
            } satisfies SparkFileRecord);
            succeeded += 1;
          } catch (error) {
            ElMessage.error(`上传 ${file.name} 失败：${error}`);
          }
        }
        if (succeeded > 0) {
          ElMessage.success(`已上传 ${succeeded} 个文件`);
          await reload();
        }
      } finally {
        uploading.value = false;
      }
    };

    const download = async (record: SparkFileRecord) => {
      const content = contentApi();
      const sys = sysApi();
      if (!content || !sys) {
        return;
      }
      busyCid.value = record.cid;
      try {
        // 壳层代存：保存对话框由壳层代开，用户取消属主动行为，静默不提示
        const outcome = await downloadRecord(content, sys, record);
        if (outcome === 'pending') {
          ElMessage.warning('文件本体暂未取回：提供者不在线，稍后再试');
        } else if (outcome === 'saved') {
          ElMessage.success(`已保存「${record.name}」`);
        }
      } catch (error) {
        ElMessage.error(`下载失败：${error}`);
      } finally {
        busyCid.value = '';
      }
    };

    const remove = async (record: SparkFileRecord) => {
      const data = dataApi();
      const content = contentApi();
      if (!data || !content) {
        return;
      }
      try {
        await ElMessageBox.confirm(
          `确认删除「${record.name}」？元数据删除会同步到复制组；本机文件本体在不再被引用后进入回收宽限期。`,
          '删除文件',
          { type: 'warning', confirmButtonText: '删除', cancelButtonText: '取消' }
        );
      } catch {
        return;
      }
      busyCid.value = record.cid;
      try {
        await data.delete(FILES_COLLECTION, record.cid);
        await content.unpinRoot(record.cid, FILE_PIN_ROOT);
        const { [record.cid]: _dropped, ...rest } = thumbUrls.value;
        thumbUrls.value = rest;
        ElMessage.success('已删除');
        await reload();
      } catch (error) {
        ElMessage.error(`删除失败：${error}`);
      } finally {
        busyCid.value = '';
      }
    };

    // 照片缩略图：按当前可见照片惰性加载本体（本地命中才展示，未命中不触发网络拉取）
    const loadThumbs = async () => {
      const content = contentApi();
      if (!content || activeTab.value !== 'photos') {
        return;
      }
      for (const photo of visiblePhotos.value) {
        if (thumbUrls.value[photo.cid]) {
          continue;
        }
        try {
          const base64 = await content.readBlob(photo.cid);
          if (base64 !== null) {
            thumbUrls.value = { ...thumbUrls.value, [photo.cid]: base64ToObjectUrl(base64, photo.mime) };
          }
        } catch {
          // 单张失败不阻断其余
        }
      }
    };
    watch([activeTab, visiblePhotos], () => void loadThumbs());

    onMounted(() => {
      void reload();
    });

    return {
      activeTab,
      keyword,
      visibleFiles,
      visiblePhotos,
      thumbUrls,
      loading,
      uploading,
      busyCid,
      loadError,
      fileInput,
      triggerSelect,
      onFilesSelected,
      download,
      remove,
      fileTypeLabel,
      formatBytes,
      formatDate
    };
  }
});
</script>

<style>
@import './styles/tokens.css';
</style>

<style scoped>
.files-root {
  display: flex;
  flex-direction: column;
  height: 100%;
  min-height: 0;
  background: var(--spark-bg-card);
  padding: 0 16px 16px;
}

.files-header {
  display: flex;
  align-items: center;
  gap: 16px;
  border-bottom: 1px solid var(--spark-border-light);
}

.files-tabs {
  flex: 1;
  min-width: 0;
}

.files-header-actions {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 8px 0;
}

.files-search {
  width: 200px;
}

.hidden-input {
  display: none;
}

.files-body {
  flex: 1;
  min-height: 0;
  overflow-y: auto;
  padding-top: 12px;
}

.files-table {
  width: 100%;
}

.photos-grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(160px, 1fr));
  gap: 12px;
}

.photo-cell {
  border: 1px solid var(--spark-border-light);
  border-radius: var(--spark-radius-m);
  overflow: hidden;
  background: var(--spark-bg-page);
}

.photo-img {
  display: block;
  width: 100%;
  aspect-ratio: 1;
  object-fit: cover;
}

.photo-loading {
  width: 100%;
  aspect-ratio: 1;
  display: flex;
  align-items: center;
  justify-content: center;
  color: var(--spark-text-3);
  font-size: 13px;
}

.photo-actions {
  display: flex;
  justify-content: space-between;
  padding: 2px 8px;
}
</style>

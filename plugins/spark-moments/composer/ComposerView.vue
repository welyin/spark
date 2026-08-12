<!--
  朋友圈插件（spark-moments）· 发动态页（composer.md）。
  文字（≤1000）+ 图片（1–9 张，生成缩略图 saveBlob）+「谁可以看」四选一 + 发表。
  图片处理：本地 objectURL 即时预览；发表时逐张生成缩略图（canvas ~256×256 JPEG 70%）
  并 saveBlob 原图 + 缩略图。可见性选择经页栈进 VisibilityPickerView / RecipientEditorView。
-->
<template>
  <section class="composer">
    <header class="composer-top">
      <button type="button" class="cancel" @click="onCancel">取消</button>
      <span class="title">发动态</span>
      <button type="button" class="publish" :disabled="!canPublish || publishing" @click="publish">
        {{ publishing ? '处理中…' : '发表' }}
      </button>
    </header>

    <div class="body">
      <textarea
        v-model="text"
        class="text-input"
        placeholder="这一刻的想法…"
        rows="4"
        maxlength="1000"
      ></textarea>
      <div class="count-badge" v-if="text.length > 900">{{ text.length }}/1000</div>

      <div class="image-grid">
        <div v-for="(img, i) in selectedImages" :key="i" class="img-cell">
          <img :src="img.objectUrl" alt="" />
          <button type="button" class="remove" @click="removeImage(i)">✕</button>
        </div>
        <button
          v-if="selectedImages.length < 9"
          type="button"
          class="add-cell"
          @click="pickImages"
        >
          <span>＋</span>
          <small v-if="imagePickerUnsupported">当前环境不支持选择图片</small>
        </button>
      </div>
      <div class="hint">最多 9 张，单张不超过 10MB</div>

      <div class="visibility-row" @click="openVisibilityPicker">
        <span class="lock">🔒 谁可以看</span>
        <span class="value">{{ scopeLabel }}<template v-if="scopeText"> · {{ scopeText }}</template> ›</span>
      </div>
    </div>

    <p v-if="contactDenied" class="deny-tip">
      选择可见范围需要读取通讯录，仅用于展开本条动态的投递名单。
    </p>
  </section>
</template>

<script lang="ts">
import { computed, defineComponent, ref } from 'vue';
import { ElMessage, ElMessageBox } from 'element-plus';
import { ensurePluginSDK } from '../../../packages/plugin-sdk/src';
import { usePageStack } from '../composables/usePageStack';
import { useMoments } from '../composables/useMoments';
import { visibilityState } from './visibilityState';
import { processImage } from './imageUtil';
import { MOMENTS_MAX_IMAGE_BYTES, MOMENTS_MAX_IMAGES, type MomentsImage, type MomentsVisibleScope } from '../model';

type SelectedImage = { objectUrl: string; file: File };

export default defineComponent({
  name: 'ComposerView',
  emits: ['back'],
  setup() {
    const { push, pop } = usePageStack();
    const moments = useMoments();

    const text = ref('');
    const selectedImages = ref<SelectedImage[]>([]);
    // 可见性与勾选名单来自共享 state（四选一页 / 名单编辑器页改这里，本页读这里）
    const scope = computed(() => visibilityState.scope);
    const selection = computed(() => visibilityState.selection);
    const publishing = ref(false);
    const imagePickerUnsupported = ref(false);
    const contactDenied = ref(false);

    const scopeLabel = computed(() => {
      const map: Record<MomentsVisibleScope, string> = {
        all: '公开', private: '私密', partial: '部分可见', exclude: '不给谁看'
      };
      return map[scope.value];
    });
    const scopeText = computed(() => {
      if (scope.value === 'partial' || scope.value === 'exclude') {
        const count = selection.value.contactRootIds.length + selection.value.groupIds.length + selection.value.tagIds.length;
        return count === 0 ? '未选择' : `已选 ${count} 项`;
      }
      if (scope.value === 'private') return '仅自己可见';
      return '所有联系人可见';
    });

    const canPublish = computed(() => {
      const hasContent = text.value.trim().length > 0 || selectedImages.value.length > 0;
      if (scope.value === 'partial' || scope.value === 'exclude') {
        const count = selection.value.contactRootIds.length + selection.value.groupIds.length + selection.value.tagIds.length;
        return hasContent && count > 0;
      }
      return hasContent;
    });

    const pickImages = () => {
      const input = document.createElement('input');
      input.type = 'file';
      input.accept = 'image/*';
      input.multiple = true;
      input.onchange = () => {
        const files = Array.from(input.files ?? []);
        for (const file of files) {
          if (selectedImages.value.length >= MOMENTS_MAX_IMAGES) {
            ElMessage.warning('最多选择 9 张');
            break;
          }
          if (file.size > MOMENTS_MAX_IMAGE_BYTES) {
            ElMessage.warning(`单张图片不能超过 10MB（${file.name}）`);
            continue;
          }
          selectedImages.value.push({ objectUrl: URL.createObjectURL(file), file });
        }
      };
      input.click();
    };

    const removeImage = (index: number) => {
      URL.revokeObjectURL(selectedImages.value[index].objectUrl);
      selectedImages.value.splice(index, 1);
    };

    const openVisibilityPicker = () => {
      push({
        name: 'visibility',
        component: () => import('./VisibilityPickerView.vue'),
        title: '谁可以看'
      });
    };

    const onCancel = async () => {
      if (text.value.trim() || selectedImages.value.length > 0) {
        try {
          await ElMessageBox.confirm('放弃这条动态？', '提示', {
            type: 'warning', confirmButtonText: '放弃', cancelButtonText: '继续编辑', confirmButtonClass: 'danger'
          });
        } catch {
          return; // 继续编辑
        }
      }
      pop();
    };

    const publish = async () => {
      publishing.value = true;
      try {
        const sdk = await ensurePluginSDK();
        // ① 图片处理：逐张生成缩略图 + saveBlob 原图与缩略图
        const images: MomentsImage[] = [];
        for (const sel of selectedImages.value) {
          const processed = await processImage(sel.file, sdk);
          images.push(processed);
        }

        // ② 发表（service 内展开名单 + 签名 + 落库 + 投递）
        await moments.publish({
          text: text.value.trim(),
          images,
          scope: scope.value,
          selection: selection.value
        });
        ElMessage.success('已发表');
        pop();
      } catch (error) {
        ElMessage.error(`发表失败：${error}`);
      } finally {
        publishing.value = false;
      }
    };

    return {
      text, selectedImages, scope, selection, publishing, imagePickerUnsupported, contactDenied,
      scopeLabel, scopeText, canPublish,
      pickImages, removeImage, openVisibilityPicker, onCancel, publish
    };
  }
});
</script>

<style scoped>
.composer { height: 100vh; overflow: hidden; background: var(--spark-bg-page, #f5f5f5); display: flex; flex-direction: column; }
.composer-top { flex-shrink: 0; display: flex; align-items: center; justify-content: space-between; padding: 12px 16px; background: var(--spark-bg-card, #fff); }
.cancel { border: none; background: none; color: var(--spark-text-2, #475569); font-size: 14px; cursor: pointer; }
.title { font-weight: 600; }
.publish { border: none; border-radius: 6px; background: var(--spark-primary, #4f7cff); color: #fff; padding: 6px 16px; font-size: 14px; cursor: pointer; }
.publish:disabled { opacity: 0.5; cursor: not-allowed; }
.body { padding: 16px; flex: 1; min-height: 0; overflow-y: auto; }
.text-input { width: 100%; border: none; resize: none; font-size: 15px; line-height: 1.6; outline: none; background: transparent; min-height: 80px; }
.count-badge { text-align: right; color: var(--spark-text-3, #94a3b8); font-size: 12px; }
.image-grid { display: grid; grid-template-columns: repeat(3, 1fr); gap: 8px; margin-top: 8px; }
.img-cell { position: relative; aspect-ratio: 1/1; border-radius: 4px; overflow: hidden; }
.img-cell img { width: 100%; height: 100%; object-fit: cover; }
.remove { position: absolute; top: 4px; right: 4px; width: 22px; height: 22px; border-radius: 50%; border: none; background: rgba(0,0,0,0.6); color: #fff; cursor: pointer; }
.add-cell { aspect-ratio: 1/1; border: 1px dashed var(--spark-border, #cbd5e1); border-radius: 4px; background: transparent; display: flex; flex-direction: column; align-items: center; justify-content: center; color: var(--spark-text-3, #94a3b8); cursor: pointer; font-size: 28px; }
.add-cell small { font-size: 11px; padding: 0 6px; }
.hint { color: var(--spark-text-3, #94a3b8); font-size: 12px; margin-top: 8px; }
.visibility-row { display: flex; align-items: center; justify-content: space-between; margin-top: 16px; padding: 14px 0; border-top: 1px solid var(--spark-border, #e2e8f0); cursor: pointer; }
.visibility-row .lock { font-size: 14px; }
.visibility-row .value { color: var(--spark-text-2, #475569); font-size: 14px; }
.deny-tip { color: var(--spark-warning, #b45309); background: var(--spark-warning-bg, #fef3c7); padding: 10px 12px; border-radius: 6px; font-size: 12px; margin: 12px 16px; }
</style>

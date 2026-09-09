<!--
  朋友圈插件（spark-moments）· 「谁可以看」四选一页（composer.md §4.2）。
  单选 radio：公开 / 私密 / 部分可见 / 不给谁看；后两项带「›」进名单编辑器。
  选择写入共享 visibilityState（本页返回后 ComposerView 读取）。
  选择器内容依赖 contact:read：未授权时仅「私密」可发（其余置灰 + 授权引导）。
-->
<template>
  <section class="picker">
    <header class="top">
      <button type="button" class="back" @click="pop">‹ 返回</button>
      <span class="title">谁可以看</span>
      <span class="placeholder"></span>
    </header>

    <div class="options">
      <label
        v-for="opt in options"
        :key="opt.scope"
        class="option"
        :class="{ selected: visibilityState.scope === opt.scope, disabled: opt.disabled }"
        @click="select(opt)"
      >
        <span class="radio">{{ visibilityState.scope === opt.scope ? '✓' : '○' }}</span>
        <div class="opt-body">
          <span class="opt-name">{{ opt.name }}</span>
          <span class="opt-desc">{{ opt.desc }}</span>
        </div>
        <span v-if="opt.scope === 'partial' || opt.scope === 'exclude'" class="chevron">›</span>
      </label>
    </div>

    <p class="tip">说明：名单按发送时展开，之后分组/标签成员变动不影响本条动态的可见范围。</p>
  </section>
</template>

<script lang="ts">
import { computed, defineComponent } from 'vue';
import { ElMessageBox } from 'element-plus';
import { usePageStack } from '../composables/usePageStack';
import { useMoments } from '../composables/useMoments';
import { visibilityState } from './visibilityState';
import type { MomentsVisibleScope } from '../model';

export default defineComponent({
  name: 'VisibilityPickerView',
  setup() {
    const { push, pop } = usePageStack();
    const moments = useMoments();

    const options = computed(() => {
      const base: Array<{ scope: MomentsVisibleScope; name: string; desc: string; disabled: boolean }> = [
        { scope: 'all', name: '公开', desc: '所有联系人可见', disabled: false },
        { scope: 'private', name: '私密', desc: '仅自己可见', disabled: false },
        { scope: 'partial', name: '部分可见', desc: selectedDesc('partial'), disabled: false },
        { scope: 'exclude', name: '不给谁看', desc: selectedDesc('exclude'), disabled: false }
      ];
      return base;
    });

    function selectedDesc(scope: MomentsVisibleScope): string {
      const s = visibilityState.selection;
      const count = s.contactRootIds.length + s.groupIds.length + s.tagIds.length;
      return count === 0 ? '未选择' : `已选 ${count} 项`;
    }

    const select = async (opt: { scope: MomentsVisibleScope; disabled: boolean }) => {
      if (opt.disabled) {
        // contact:read 未授权：引导授权
        try {
          await ElMessageBox.alert('选择可见范围需要读取通讯录，仅用于展开本条动态的投递名单。', '需要授权', {
            confirmButtonText: '去授权'
          });
        } catch {
          /* 取消 */
        }
        return;
      }
      visibilityState.scope = opt.scope;
      if (opt.scope === 'partial' || opt.scope === 'exclude') {
        push({
          name: 'recipient-editor',
          component: () => import('./RecipientEditorView.vue'),
          props: { title: opt.scope === 'partial' ? '选择可见的人' : '选择不给看的人' },
          title: '名单编辑'
        });
      }
    };

    return { visibilityState, options, select, pop };
  }
});
</script>

<style scoped>
/* 页栈子页根 = iframe 内独立滚动容器（父级 .moments-root overflow:hidden 不滚动，须自滚动） */
.picker { height: 100%; overflow-y: auto; background: var(--spark-bg-page, #f5f5f5); }
.top { display: flex; align-items: center; justify-content: space-between; padding: 12px 16px; background: var(--spark-bg-card, #fff); }
.back { border: none; background: none; color: var(--spark-text-2, #475569); font-size: 16px; cursor: pointer; }
.title { font-weight: 600; }
.placeholder { width: 40px; }
.options { background: var(--spark-bg-card, #fff); margin: 12px; border-radius: 8px; overflow: hidden; }
.option { display: flex; align-items: center; gap: 12px; padding: 16px; border-bottom: 1px solid var(--spark-border, #f1f5f9); cursor: pointer; }
.option.selected { background: var(--spark-bg-hover, #f8fafc); }
.option.disabled { opacity: 0.45; cursor: not-allowed; }
.radio { width: 22px; color: var(--spark-primary, #4f7cff); font-size: 18px; text-align: center; }
.opt-body { flex: 1; display: flex; flex-direction: column; gap: 4px; }
.opt-name { font-size: 15px; }
.opt-desc { font-size: 12px; color: var(--spark-text-3, #94a3b8); }
.chevron { color: var(--spark-text-3, #94a3b8); font-size: 18px; }
.tip { color: var(--spark-text-3, #94a3b8); font-size: 12px; padding: 4px 16px; }
</style>

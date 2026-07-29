<!-- 技术术语通俗化标签：展示通俗名（点状下划线 + 问号图标），
     悬停 tooltip 给出专业术语与一句白话解释。
     仅改展示名称，复制/提交的仍是完整原值。 -->
<template>
  <el-tooltip :content="tip" placement="top" :show-after="150">
    <span class="term-label">
      {{ text || friendly }}
      <el-icon class="term-label-icon" :size="12"><QuestionFilled /></el-icon>
    </span>
  </el-tooltip>
</template>

<script lang="ts">
import { computed, defineComponent, type PropType } from 'vue';
import { QuestionFilled } from '@element-plus/icons-vue';

export type TermKey = 'rootId' | 'peerId' | 'addresses';

/** 术语映射表：通俗名 + tooltip（专业术语 + 白话解释） */
const TERMS: Record<TermKey, { friendly: string; tip: string }> = {
  rootId: {
    friendly: '身份 ID',
    tip: 'RootID：你的去中心化身份标识，由助记词派生，用于签名与授权。'
  },
  peerId: {
    friendly: '节点 ID',
    tip: 'PeerID：当前设备在 P2P 网络中的地址标识，设备级、可变化。'
  },
  addresses: {
    friendly: '节点地址',
    tip: 'P2P Addresses：当前设备对外可拨号的网络地址，其他节点凭它与你的设备建立连接。'
  }
};

export default defineComponent({
  name: 'TermLabel',
  components: { QuestionFilled },
  props: {
    term: { type: String as PropType<TermKey>, required: true },
    /** 覆盖默认通俗名（如「成员身份 ID」），tooltip 不变 */
    text: { type: String, default: '' }
  },
  setup(props) {
    const friendly = computed(() => TERMS[props.term].friendly);
    const tip = computed(() => TERMS[props.term].tip);
    return { friendly, tip };
  }
});
</script>

<style scoped>
.term-label {
  display: inline-flex;
  align-items: center;
  gap: 2px;
  cursor: help;
  text-decoration: underline dotted;
  text-underline-offset: 3px;
}

.term-label-icon {
  color: var(--spark-text-3);
}
</style>

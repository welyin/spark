<!-- 节点身份信息行组（灰色小字 label + 等宽字体值分行）：
     设置页「节点状态」与「我的」页各模块共用，替代 el-descriptions 表格。
     空值显示灰色「未获取/暂无」el-tag；copyable 行带复制按钮（复制完整值）+ toast；
     shorten 行中间缩略展示（前 8…后 8，title 悬停看全量）；
     身份 ID 行（term=rootId）统一缩略为前 8…后 4 + 复制按钮 + 二维码小图标
     （点击弹层展示完整 RootID 二维码，title 悬停看完整 ID） -->
<template>
  <div class="node-id-rows">
    <div
      v-for="row in rows"
      :key="row.label"
      class="node-id-row"
      :class="{ 'node-id-row-top': linesOf(row).length > 1 }"
    >
      <span class="node-id-label">
        <!-- 技术术语行展示通俗名 + 悬停解释（TermLabel），其余行纯文本 -->
        <TermLabel v-if="row.term" :term="row.term" :text="row.label" />
        <template v-else>{{ row.label }}</template>
      </span>
      <template v-if="linesOf(row).length > 0">
        <span class="mono node-id-value" :class="{ 'node-id-lines': linesOf(row).length > 1 }">
          <span
            v-for="line in linesOf(row)"
            :key="line"
            :title="showsFullOnHover(row) ? line : undefined"
          >{{ displayLine(row, line) }}</span>
        </span>
        <el-tooltip v-if="row.copyable" content="复制" placement="top">
          <el-button
            text
            size="small"
            class="node-id-copy"
            :icon="CopyDocument"
            @click="copyRow(row)"
          />
        </el-tooltip>
        <!-- 身份 ID 行：二维码小图标，点击弹层展示完整 RootID 二维码 -->
        <el-popover
          v-if="isRootIdRow(row)"
          trigger="click"
          placement="top"
          :width="212"
          @show="ensureQr(row)"
        >
          <template #reference>
            <el-button text size="small" class="node-id-copy" title="查看身份 ID 二维码">
              <el-icon :size="14">
                <svg viewBox="0 0 16 16" fill="currentColor" width="1em" height="1em"><path d="M1 1h6v6H1V1zm2 2v2h2V3H3zm6-2h6v6H9V1zm2 2v2h2V3h-2zM1 9h6v6H1V9zm2 2v2h2v-2H3zm8-2h2v2h-2V9zm3 0h2v2h-2V9zm-3 3h2v2h-2v-2zm3 3h2v-2h-2v2zm-3 0h2v2h-2v-2z"/></svg>
              </el-icon>
            </el-button>
          </template>
          <div class="node-id-qr">
            <img v-if="qrUrls[firstLine(row)]" :src="qrUrls[firstLine(row)]" alt="身份 ID 二维码" class="node-id-qr-img" />
            <span v-else>生成中...</span>
            <span class="node-id-qr-tip">扫码获取完整身份 ID</span>
          </div>
        </el-popover>
      </template>
      <el-tag v-else size="small" type="info">{{ row.emptyText ?? '未获取' }}</el-tag>
    </div>
  </div>
</template>

<script lang="ts">
import { defineComponent, ref, type PropType } from 'vue';
import { ElMessage } from 'element-plus';
import { CopyDocument } from '@element-plus/icons-vue';
import QRCode from 'qrcode';
import { shortenMiddle } from '../../utils/format';
import TermLabel, { type TermKey } from './TermLabel.vue';

/** 一行节点信息：value 传数组时逐行展示；空串/空数组视为未获取 */
export type NodeIdentityRow = {
  label: string;
  value: string | string[];
  /** 技术术语 key：设置后 label 以 TermLabel 渲染（通俗名 + 悬停专业解释） */
  term?: TermKey;
  /** 中间缩略展示（前 8…后 8），复制仍为完整值；身份 ID 行无需设置，固定前 8…后 4 */
  shorten?: boolean;
  /** 显示复制按钮 */
  copyable?: boolean;
  /** 空值时 el-tag 文案，默认「未获取」 */
  emptyText?: string;
};

export default defineComponent({
  name: 'NodeIdentityInfo',
  components: { TermLabel },
  props: {
    rows: { type: Array as PropType<NodeIdentityRow[]>, required: true }
  },
  setup() {
    /** 归一化为非空行数组（空串/空白行剔除） */
    const linesOf = (row: NodeIdentityRow): string[] => {
      const list = Array.isArray(row.value) ? row.value : [row.value];
      return list.map((line) => line.trim()).filter((line) => line.length > 0);
    };

    const firstLine = (row: NodeIdentityRow): string => linesOf(row)[0] ?? '';

    /** 身份 ID 行：固定缩略 8…4 + 二维码入口（所有使用处统一生效） */
    const isRootIdRow = (row: NodeIdentityRow): boolean => row.term === 'rootId';

    const displayLine = (row: NodeIdentityRow, line: string): string => {
      if (isRootIdRow(row)) {
        return shortenMiddle(line, 8, 4);
      }
      return row.shorten ? shortenMiddle(line) : line;
    };

    const showsFullOnHover = (row: NodeIdentityRow): boolean => isRootIdRow(row) || !!row.shorten;

    const copyRow = async (row: NodeIdentityRow) => {
      try {
        await navigator.clipboard.writeText(linesOf(row).join('\n'));
        ElMessage.success(`${row.label} 已复制`);
      } catch (error) {
        ElMessage.error(`复制失败：${error}`);
      }
    };

    // 身份 ID 二维码：弹层首次打开时按需生成（内容为完整 RootID），按值缓存
    const qrUrls = ref<Record<string, string>>({});
    const ensureQr = async (row: NodeIdentityRow) => {
      const value = firstLine(row);
      if (!value || qrUrls.value[value]) {
        return;
      }
      try {
        const url = await QRCode.toDataURL(value, { margin: 1, width: 180 });
        qrUrls.value = { ...qrUrls.value, [value]: url };
      } catch {
        ElMessage.error('二维码生成失败');
      }
    };

    return { linesOf, firstLine, isRootIdRow, displayLine, showsFullOnHover, copyRow, qrUrls, ensureQr, CopyDocument };
  }
});
</script>

<style scoped>
.node-id-rows {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.node-id-row {
  display: flex;
  align-items: center;
  gap: 12px;
}

.node-id-row-top {
  align-items: flex-start;
}

.node-id-label {
  flex: 0 0 96px;
  color: var(--spark-text-3);
  font-size: 12px;
}

.node-id-value {
  flex: 1;
  min-width: 0;
  font-size: 12px;
  color: var(--spark-text-1);
  word-break: break-all;
  user-select: text;
}

.node-id-lines {
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.node-id-copy {
  flex-shrink: 0;
  padding: 4px;
  color: var(--spark-text-3);
}

.node-id-copy:hover {
  color: var(--spark-primary);
}

/* 身份 ID 二维码弹层内容 */
.node-id-qr {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 8px;
}

.node-id-qr-img {
  width: 180px;
  height: 180px;
}

.node-id-qr-tip {
  font-size: 12px;
  color: var(--spark-text-3);
}
</style>

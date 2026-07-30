<!-- 通讯录第二栏统一列表（ui-contacts §2.2）：功能区（新的朋友/新的成员、标签）+
     分组列表，同一滚动容器、唯一选中态。
     个人空间=扁平一层分组（可增/删/改名/拖拽重排）；组织空间=树形分组
     （仅管理员增删改结构，任何人可同级拖拽排序，不能改树结构）。
     「未分组」为恒在分组区顶部的虚拟组（无分组归属的联系人）；拉黑者不进通讯录，
     黑名单统一在个人设置「朋友权限/成员权限」中管理。
     CRUD 直写 mock store（同 TagManager 模式），仅 select 上报给 ContactsPage -->
<template>
  <div class="group-panel" @dragover="onPanelDragOver" @drop.prevent="onPanelDrop">
    <!-- 功能区：新的朋友/新的成员、标签（与分组同属一个滚动列表，共享唯一选中态）。
         组织普通成员无权处理入组织申请，不显示「新的成员」行 -->
    <div
      v-if="mode === 'personal' || canEditStructure"
      class="request-item group-row"
      :class="{ active: activeId === 'new-friends' }"
      @click="$emit('select', 'new-friends')"
    >
      <span class="row-icon row-icon-new"><el-icon :size="17"><CirclePlus /></el-icon></span>
      <span class="request-item-main"><b>{{ mode === 'org' ? '新的成员' : '新的朋友' }}</b></span>
      <el-badge v-if="pendingCount > 0" :value="pendingCount" :max="99" class="row-badge" />
    </div>
    <div
      class="request-item group-row"
      :class="{ active: activeId === 'tags' }"
      @click="$emit('select', 'tags')"
    >
      <span class="row-icon row-icon-tag"><el-icon :size="17"><CollectionTag /></el-icon></span>
      <span class="request-item-main"><b>标签</b></span>
    </div>
    <!-- 组织空间：管理员行（展示全部管理员成员） -->
    <div
      v-if="mode === 'org'"
      class="request-item group-row"
      :class="{ active: activeId === 'admins' }"
      @click="$emit('select', 'admins')"
    >
      <span class="row-icon"><el-icon :size="17"><Star /></el-icon></span>
      <span class="request-item-main"><b>管理员</b></span>
      <span class="group-count">{{ adminCount }}</span>
    </div>

    <div class="contacts-request-title">
      <span>分组</span>
      <!-- 组织普通成员无权改树结构，不显示新建分组按钮（行内操作同理有 canEditStructure 守卫） -->
      <button
        v-if="mode === 'personal' || canEditStructure"
        class="request-head-btn"
        title="新建分组"
        @click="createRow('')"
      >
        <el-icon :size="16"><Plus /></el-icon>
      </button>
    </div>

    <!-- 虚拟组：未分组（没有任何分组归属的联系人） -->
    <div
      class="request-item group-row"
      :class="{ active: activeId === 'ungrouped' }"
      @click="$emit('select', 'ungrouped')"
    >
      <span class="row-icon"><el-icon :size="17"><User /></el-icon></span>
      <span class="request-item-main"><b>未分组</b></span>
      <span class="group-count">{{ counts.ungrouped || 0 }}</span>
    </div>

    <!-- 个人空间：扁平分组行（可拖拽重排）；dragOverId 行前渲染落点预测占位条 -->
    <template v-if="mode === 'personal'">
      <template v-for="group in groups" :key="group.id">
        <!-- 落点占位条本身就是预测位置：必须接收 drop（pointer-events:none 会让
             直接落在占位条上的 drop 穿透到面板背景被吞，表现为移动失败） -->
        <div
          v-if="dragOverId === group.id && dropMode === 'before'"
          class="group-drop-placeholder"
          @dragover.prevent
          @drop.prevent="onDropPersonal(group.id)"
        />
        <div
          class="request-item group-row drop-row"
          :ref="(el) => setDropRowRef(el, group.id)"
          :data-id="group.id"
          :class="{ active: activeId === group.id, 'drag-source': hiddenId === group.id }"
          :draggable="armedId === group.id"
          @mousedown="armDrag($event, group.id)"
          @dragstart="onDragStart($event, group.id)"
          @dragenter.prevent
          @dragover="onDragOverPersonal($event, group.id)"
          @drop.prevent="onDropPersonal(group.id)"
          @dragend="clearDrag"
          @click="$emit('select', group.id)"
        >
          <span class="row-icon"><el-icon :size="17"><Folder /></el-icon></span>
          <span class="request-item-main"><b>{{ group.name }}</b></span>
          <span class="group-actions">
            <button class="request-head-btn" title="重命名" @click.stop="renameRow(group)">
              <el-icon :size="14"><Edit /></el-icon>
            </button>
            <button class="request-head-btn" title="删除分组" @click.stop="deleteRow(group)">
              <el-icon :size="14"><Delete /></el-icon>
            </button>
          </span>
          <span class="group-count">{{ counts[group.id] || 0 }}</span>
        </div>
        <div
          v-if="dragOverId === group.id && dropMode === 'after'"
          class="group-drop-placeholder"
          @dragover.prevent
          @drop.prevent="onDropPersonal(group.id)"
        />
      </template>
    </template>

    <!-- 组织空间：树形分组行（仅同级拖拽；结构操作仅管理员）；
         before/after 落点渲染与目标行同深度的预测占位条，into 保持整行描边 -->
    <template v-else>
      <template v-for="row in flatRows" :key="row.id">
        <div
          v-if="dragOverId === row.id && dropMode === 'before'"
          class="group-drop-placeholder"
          :style="{ marginLeft: `${row.depth * 18}px` }"
          @dragover.prevent
          @drop.prevent="onDropOrg(row)"
        />
        <div
          class="request-item group-row drop-row"
          :ref="(el) => setDropRowRef(el, row.id)"
          :data-id="row.id"
          :class="{
            active: activeId === row.id,
            'drag-into': dragOverId === row.id && dropMode === 'into',
            'drag-source': hiddenId === row.id
          }"
          :style="{ paddingLeft: `${row.depth * 18 + 8}px` }"
          :draggable="armedId === row.id"
          @mousedown="armDrag($event, row.id)"
          @dragstart="onDragStart($event, row.id, row.parentId)"
          @dragenter.prevent
          @dragover="onDragOverOrg($event, row)"
          @drop.prevent="onDropOrg(row)"
          @dragend="clearDrag"
          @click="$emit('select', row.id)"
        >
          <button
            v-if="row.hasChildren"
            class="group-caret"
            @click.stop="toggle(row.id)"
          >
            <el-icon :size="12"><ArrowDown v-if="!collapsed.has(row.id)" /><ArrowRight v-else /></el-icon>
          </button>
          <span v-else class="group-caret group-caret-empty"></span>
          <span class="row-icon"><el-icon :size="17"><Folder /></el-icon></span>
          <span class="request-item-main"><b>{{ row.name }}</b></span>
          <span v-if="canEditStructure" class="group-actions">
            <button class="request-head-btn" title="添加子分组" @click.stop="createRow(row.id)">
              <el-icon :size="14"><Plus /></el-icon>
            </button>
            <button class="request-head-btn" title="重命名" @click.stop="renameRow(row)">
              <el-icon :size="14"><Edit /></el-icon>
            </button>
            <button class="request-head-btn" title="删除分组" @click.stop="deleteRow(row)">
              <el-icon :size="14"><Delete /></el-icon>
            </button>
          </span>
          <span class="group-count">{{ counts[row.id] || 0 }}</span>
        </div>
        <div
          v-if="dragOverId === row.id && dropMode === 'after'"
          class="group-drop-placeholder"
          :style="{ marginLeft: `${row.depth * 18}px` }"
          @dragover.prevent
          @drop.prevent="onDropOrg(row)"
        />
      </template>
    </template>
  </div>
</template>

<script lang="ts">
import { computed, defineComponent, ref, type PropType } from 'vue';
import { ElMessageBox } from 'element-plus';
import { ArrowDown, ArrowRight, CirclePlus, CollectionTag, Delete, Edit, Folder, Plus, Star, User } from '@element-plus/icons-vue';
import {
  createGroup,
  createOrgGroup,
  deleteGroup,
  deleteOrgGroup,
  moveGroup,
  moveOrgGroup,
  moveOrgGroupSibling,
  renameGroup,
  renameOrgGroup,
  type ContactGroupDef,
  type OrgGroupNode
} from '../../mock/contacts';

/** 组织树扁平化后的行（模板 v-for 渲染单元） */
type FlatRow = { id: string; name: string; depth: number; parentId: string; hasChildren: boolean };
/** 落点模式：before/after=同级插入到目标前/后；into=作为目标的子分组（仅管理员可触发 into） */
type DropMode = 'before' | 'after' | 'into';

export default defineComponent({
  name: 'GroupPanel',
  components: { ArrowDown, ArrowRight, CirclePlus, CollectionTag, Delete, Edit, Folder, Plus, Star, User },
  props: {
    mode: { type: String as PropType<'personal' | 'org'>, required: true },
    spaceKey: { type: String, required: true },
    /** 个人空间：扁平分组数组 */
    groups: { type: Array as PropType<ContactGroupDef[]>, default: () => [] },
    /** 组织空间：分组树 */
    groupTree: { type: Array as PropType<OrgGroupNode[]>, default: () => [] },
    /** 各组人数：{ ungrouped, blocked, [groupId]: n } */
    counts: { type: Object as PropType<Record<string, number>>, required: true },
    /** 待处理申请数（「新的朋友/新的成员」行角标） */
    pendingCount: { type: Number, default: 0 },
    /** 管理员人数（组织空间「管理员」行） */
    adminCount: { type: Number, default: 0 },
    /** 当前选中行：'new-friends' / 'tags' / 'admins' / 'ungrouped' / 'blocked' / 分组 id */
    activeId: { type: String, default: 'ungrouped' },
    /** 是否可增删改树结构（个人恒 true；组织=管理员） */
    canEditStructure: { type: Boolean, default: true }
  },
  emits: ['select'],
  setup(props, { emit }) {
    const collapsed = ref<Set<string>>(new Set());
    const armedId = ref(''); // mousedown 落在行空白处才武装拖拽（避免误拖行内按钮）
    const dragId = ref('');
    const dragParentId = ref('');
    // 拖拽源隐藏延迟一帧：dragstart 同步隐藏源元素会被 WebKit 视为源节点消失而取消整个拖拽会话，
    // 等浏览器拍完拖拽影像、会话建立后再隐藏
    const hiddenId = ref('');
    const dragOverId = ref('');
    const dropMode = ref<DropMode>('before');

    /** 组织树扁平化为带深度的行（折叠节点的子树不展开） */
    const flatRows = computed<FlatRow[]>(() => {
      const rows: FlatRow[] = [];
      const walk = (nodes: OrgGroupNode[], depth: number, parentId: string) => {
        for (const node of nodes) {
          rows.push({
            id: node.id,
            name: node.name,
            depth,
            parentId,
            hasChildren: node.children.length > 0
          });
          if (!collapsed.value.has(node.id)) {
            walk(node.children, depth + 1, node.id);
          }
        }
      };
      walk(props.groupTree, 0, '');
      return rows;
    });

    const toggle = (id: string) => {
      if (collapsed.value.has(id)) {
        collapsed.value.delete(id);
      } else {
        collapsed.value.add(id);
      }
      // Set 变更需触发响应式：整体替换
      collapsed.value = new Set(collapsed.value);
    };

    // ---- 拖拽：仅重排，个人=整表同级；组织=同父同级 ----
    const armDrag = (event: MouseEvent, id: string) => {
      // mousedown 落在行内按钮（展开 caret/重命名/删除等）上不武装拖拽，避免误拖；
      // 注意空 caret 占位 span 不是 button，不再误伤（之前落在行首 16px 区域永远无法起拖）
      armedId.value = (event.target as HTMLElement).closest('.group-actions, button.group-caret') ? '' : id;
    };

    const onDragStart = (event: DragEvent, id: string, parentId = '') => {
      dragId.value = id;
      dragParentId.value = parentId;
      // WebKit 兼容：dragstart 必须写入 dataTransfer，拖拽会话才算有效
      event.dataTransfer?.setData('text/plain', id);
      if (event.dataTransfer) event.dataTransfer.effectAllowed = 'move';
      // 延迟隐藏源行（同步隐藏会被 WebKit 取消拖拽会话）
      setTimeout(() => {
        hiddenId.value = dragId.value;
      }, 0);
    };

    const onDragOverOrg = (event: DragEvent, row: FlatRow) => {
      if (!dragId.value || dragId.value === row.id) return;
      const rect = (event.currentTarget as HTMLElement).getBoundingClientRect();
      const ratio = (event.clientY - rect.top) / rect.height;
      if (props.canEditStructure) {
        // 管理员可跨级：上/下 1/4=作为目标同级插到前/后，中间 1/2=作为目标的子分组；
        // 禁止拖入自己的子树（成环）
        if (subtreeIds(dragId.value).has(row.id)) return;
        event.preventDefault();
        dragOverId.value = row.id;
        dropMode.value = ratio < 0.25 ? 'before' : ratio > 0.75 ? 'after' : 'into';
        return;
      }
      // 非管理员：仅同父同级允许 drop，其余显示禁止光标（树结构不可拖拽改变）
      if (dragParentId.value === row.parentId) {
        event.preventDefault();
        dragOverId.value = row.id;
        dropMode.value = ratio > 0.5 ? 'after' : 'before';
      }
    };

    /** 收集分组子树的全部 id（含自身），环检测用：禁止把分组拖入自己的子树 */
    const subtreeIds = (id: string) => {
      const result = new Set<string>();
      const collect = (node: OrgGroupNode) => {
        result.add(node.id);
        node.children.forEach(collect);
      };
      const find = (nodes: OrgGroupNode[]): OrgGroupNode | null => {
        for (const node of nodes) {
          if (node.id === id) return node;
          const hit = find(node.children);
          if (hit) return hit;
        }
        return null;
      };
      const node = find(props.groupTree);
      if (node) collect(node);
      return result;
    };

    const onDragOverPersonal = (event: DragEvent, id: string) => {
      if (!dragId.value || dragId.value === id) return;
      event.preventDefault();
      const rect = (event.currentTarget as HTMLElement).getBoundingClientRect();
      const ratio = (event.clientY - rect.top) / rect.height;
      dragOverId.value = id;
      dropMode.value = ratio > 0.5 ? 'after' : 'before';
    };

    const onDropPersonal = (targetId: string) => {
      if (!dragId.value || dragId.value === targetId) {
        clearDrag();
        return;
      }
      let toIndex = props.groups.findIndex((g) => g.id === targetId);
      if (toIndex === -1) {
        clearDrag();
        return;
      }
      if (dropMode.value === 'after') toIndex += 1;
      moveGroup(props.spaceKey, dragId.value, toIndex);
      clearDrag();
    };

    const onDropOrg = (targetRow: FlatRow) => {
      if (!dragId.value || dragId.value === targetRow.id) {
        clearDrag();
        return;
      }
      if (props.canEditStructure) {
        // 管理员：into=作为目标子分组（末尾）；before/after=移动到目标所在层的对应位置
        if (subtreeIds(dragId.value).has(targetRow.id)) {
          clearDrag();
          return;
        }
        if (dropMode.value === 'into') {
          moveOrgGroup(props.spaceKey, dragId.value, targetRow.id, Number.MAX_SAFE_INTEGER);
        } else {
          const siblings = flatRows.value.filter((r) => r.parentId === targetRow.parentId);
          let index = siblings.findIndex((r) => r.id === targetRow.id);
          if (dropMode.value === 'after') index += 1;
          moveOrgGroup(props.spaceKey, dragId.value, targetRow.parentId, index);
        }
        clearDrag();
        return;
      }
      // 非管理员：仅同父同级重排
      if (dragParentId.value !== targetRow.parentId) {
        clearDrag();
        return;
      }
      let toIndex = flatRows.value
        .filter((r) => r.parentId === targetRow.parentId)
        .findIndex((r) => r.id === targetRow.id);
      if (toIndex === -1) {
        clearDrag();
        return;
      }
      if (dropMode.value === 'after') toIndex += 1;
      moveOrgGroupSibling(props.spaceKey, dragId.value, toIndex);
      clearDrag();
    };

    // ---- 面板级兜底：标题/未分组行/底部空隙等空白区也能预测落点与接收 drop ----
    // .drop-row 行元素经模板函数 ref 收集（id -> 元素），替代 this.$el.querySelectorAll；
    // 渲染顺序即 DOM 顺序：个人=groups，组织=flatRows
    const dropRowEls = new Map<string, HTMLElement>();
    const setDropRowRef = (el: Element | null, id: string) => {
      if (el instanceof HTMLElement) {
        dropRowEls.set(id, el);
      } else {
        dropRowEls.delete(id);
      }
    };

    const onPanelDragOver = (event: DragEvent) => {
      if (!dragId.value) return;
      event.preventDefault();
      // 行与占位条有自己的处理，不覆盖
      if ((event.target as HTMLElement).closest('.drop-row, .group-drop-placeholder')) return;
      const orderedIds =
        props.mode === 'personal' ? props.groups.map((g) => g.id) : flatRows.value.map((r) => r.id);
      const rows = orderedIds
        .map((id) => ({ id, el: dropRowEls.get(id) }))
        .filter((row): row is { id: string; el: HTMLElement } => Boolean(row.el));
      if (rows.length === 0) return;
      const firstRect = rows[0].el.getBoundingClientRect();
      const lastRect = rows[rows.length - 1].el.getBoundingClientRect();
      // 光标在首行中线以上→预测到列表最前；末行中线以下→预测到列表最后
      if (event.clientY < firstRect.top + firstRect.height / 2) {
        dragOverId.value = rows[0].id;
        dropMode.value = 'before';
      } else if (event.clientY > lastRect.bottom - lastRect.height / 2) {
        dragOverId.value = rows[rows.length - 1].id;
        dropMode.value = 'after';
      }
    };

    const onPanelDrop = (event: DragEvent) => {
      if (!dragId.value) return;
      // 行与占位条有自己的 drop，不重复执行
      if ((event.target as HTMLElement).closest('.drop-row, .group-drop-placeholder')) return;
      if (!dragOverId.value) {
        clearDrag();
        return;
      }
      // 空白区 drop：按当前预测落点执行
      if (props.mode === 'personal') {
        onDropPersonal(dragOverId.value);
        return;
      }
      const row = flatRows.value.find((r) => r.id === dragOverId.value);
      if (row) {
        onDropOrg(row);
      } else {
        clearDrag();
      }
    };

    const clearDrag = () => {
      armedId.value = '';
      dragId.value = '';
      dragParentId.value = '';
      hiddenId.value = '';
      dragOverId.value = '';
      dropMode.value = 'before';
    };

    // ---- CRUD：个人=扁平接口；组织=树接口（parentId '' 表示根层） ----
    const createRow = (parentId: string) => {
      if (props.mode === 'org' && !props.canEditStructure) return;
      ElMessageBox.prompt('请输入分组名称', '新建分组', {
        confirmButtonText: '创建',
        cancelButtonText: '取消',
        inputPlaceholder: props.mode === 'personal' ? '例如：家人、同事' : '例如：技术部、市场部'
      })
        .then(({ value }) => {
          const name = (value || '').trim();
          if (!name) return;
          if (props.mode === 'personal') {
            createGroup(props.spaceKey, name);
          } else {
            createOrgGroup(props.spaceKey, parentId, name);
          }
        })
        .catch(() => {});
    };

    const renameRow = (group: { id: string; name: string }) => {
      if (props.mode === 'org' && !props.canEditStructure) return;
      ElMessageBox.prompt('请输入新的分组名称', '重命名分组', {
        confirmButtonText: '保存',
        cancelButtonText: '取消',
        inputValue: group.name
      })
        .then(({ value }) => {
          const name = (value || '').trim();
          if (!name) return;
          if (props.mode === 'personal') {
            renameGroup(props.spaceKey, group.id, name);
          } else {
            renameOrgGroup(props.spaceKey, group.id, name);
          }
        })
        .catch(() => {});
    };

    const deleteRow = (group: { id: string; name: string }) => {
      if (props.mode === 'org' && !props.canEditStructure) return;
      const hint =
        props.mode === 'personal'
          ? `确定删除分组「${group.name}」吗？组内联系人将移到未分组。`
          : `确定删除分组「${group.name}」吗？其子分组将上移一层，组内成员将移到未分组。`;
      ElMessageBox.confirm(hint, '删除分组', {
        confirmButtonText: '删除',
        cancelButtonText: '取消',
        type: 'warning'
      })
        .then(() => {
          if (props.mode === 'personal') {
            deleteGroup(props.spaceKey, group.id);
          } else {
            deleteOrgGroup(props.spaceKey, group.id);
          }
          if (props.activeId === group.id) emit('select', 'ungrouped');
        })
        .catch(() => {});
    };

    return {
      collapsed,
      armedId,
      hiddenId,
      dragOverId,
      dropMode,
      flatRows,
      toggle,
      armDrag,
      onDragStart,
      onDragOverOrg,
      onDragOverPersonal,
      onDropPersonal,
      onDropOrg,
      setDropRowRef,
      onPanelDragOver,
      onPanelDrop,
      clearDrag,
      createRow,
      renameRow,
      deleteRow
    };
  }
});
</script>

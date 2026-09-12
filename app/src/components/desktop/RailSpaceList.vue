<!-- rail「空间」二级列表（PC）：空间项下方列出 个人空间 + 已加入组织，点击切换当前空间。
     出处：用户评审决策（顶导航左上角的空间切换收进「空间」，二级列表放 rail「空间」二字下面）。
     数据源复用：org-membership（organizations）、current-space（切换）、头像组件。
     宽栏下常驻展示完整列表；窄栏（收缩）态按 problem L11 以紧凑纵向小图标列呈现全部空间，
     悬停 tooltip（Element tooltip，G3 口径）显示空间名，点击切换空间。
     创建/加入对话框已提升为 App 根的顶级对话框（problem L10，MembershipDialogs），
     本组件不再承载；入口为「空间」区块右上角的虚线框「＋」（App.vue）。 -->
<template>
  <div class="rail-space-list" :class="{ 'rail-space-list--compact': compact }">
    <el-input v-if="!compact && organizations.length > 3" v-model="query" size="small" placeholder="搜索空间" :prefix-icon="Search" clearable />

    <!-- 窄栏紧凑态（L11）：纵向小图标列，悬停 tooltip 显示空间名，点击切换空间；
         data-drop-space = X3 落点即域（OS 文件拖入 / 壳层对象拖拽的域判定锚点） -->
    <template v-if="compact">
      <el-tooltip :content="personalSpaceName" placement="right" :show-after="200">
        <button
          type="button"
          class="rail-space-item rail-space-item--icon"
          :class="{ active: isPersonal, 'drop-hover': isDropHover({ type: 'personal' }) }"
          :title="personalSpaceName"
          data-drop-space="personal"
          @click="select({ type: 'personal' })"
          @contextmenu.prevent.stop="onSpaceContext({ type: 'personal' }, $event)"
        >
          <UserAvatar :root-id="personalSource.seed" :nickname="personalSpaceName" :avatar="personalSpaceLogo || personalSource.image" :size="22" />
        </button>
      </el-tooltip>
      <el-tooltip
        v-for="org in organizations"
        :key="org.orgId"
        :content="org.name"
        placement="right"
        :show-after="200"
      >
        <button
          type="button"
          class="rail-space-item rail-space-item--icon"
          :class="{ active: !isPersonal && currentOrgId === org.orgId, 'drop-hover': isDropHover({ type: 'org', orgId: org.orgId }) }"
          :title="org.name"
          :data-drop-space="org.orgId"
          @click="select({ type: 'org', orgId: org.orgId })"
          @contextmenu.prevent.stop="onSpaceContext({ type: 'org', orgId: org.orgId }, $event)"
        >
          <OrgAvatar :org-id="org.orgId" :name="org.name" :size="22" />
        </button>
      </el-tooltip>
    </template>

    <!-- 宽栏完整态：图标 + 名称行 -->
    <template v-else>
      <button
        type="button"
        class="rail-space-item"
        :class="{ active: isPersonal, 'drop-hover': isDropHover({ type: 'personal' }) }"
        :title="personalSpaceName"
        data-drop-space="personal"
        @click="select({ type: 'personal' })"
        @contextmenu.prevent.stop="onSpaceContext({ type: 'personal' }, $event)"
      >
        <UserAvatar :root-id="personalSource.seed" :nickname="personalSpaceName" :avatar="personalSpaceLogo || personalSource.image" :size="22" />
        <span class="rail-space-name">{{ personalSpaceName }}</span>
      </button>

      <button
        v-for="org in filteredOrganizations"
        :key="org.orgId"
        type="button"
        class="rail-space-item"
        :class="{ active: !isPersonal && currentOrgId === org.orgId, 'drop-hover': isDropHover({ type: 'org', orgId: org.orgId }) }"
        :title="org.name"
        :data-drop-space="org.orgId"
        @click="select({ type: 'org', orgId: org.orgId })"
        @contextmenu.prevent.stop="onSpaceContext({ type: 'org', orgId: org.orgId }, $event)"
      >
        <OrgAvatar :org-id="org.orgId" :name="org.name" :size="22" />
        <span class="rail-space-name">{{ org.name }}</span>
      </button>
    </template>

    <!-- X8 空间右键菜单：复制 spark:// 空间深链（光标定位弹层，同桌面菜单口径） -->
    <Teleport to="body">
      <div
        v-if="spaceMenu.visible"
        class="rail-space-menu-mask"
        @click="closeSpaceMenu"
        @contextmenu.prevent="closeSpaceMenu"
      >
        <div class="rail-space-menu" :style="{ left: `${spaceMenu.x}px`, top: `${spaceMenu.y}px` }" @click.stop>
          <button type="button" class="rail-space-menu-item" @click="onCopySpaceLink">复制空间链接</button>
        </div>
      </div>
    </Teleport>
  </div>
</template>

<script lang="ts">
import { computed, defineComponent, onMounted, reactive, ref } from 'vue';
import { ElMessage } from 'element-plus';
import { Search } from '@element-plus/icons-vue';
import { organizations, refreshOrganizations } from '../../stores/org-membership';
import { currentSpace, currentSpaceOrgId, switchToOrg, switchToPersonal, type CurrentSpace } from '../../stores/current-space';
import { personalAvatarSource } from '../../stores/avatar-sources';
import { personalSpaceLogo, personalSpaceName } from '../../stores/personal-space';
import { isDropHover as isObjectDropHover, sameSpace, type SpaceRef } from '../../stores/cross-domain-drop';
import { osDropState } from '../../stores/os-file-drop';
import { buildSparkUrl } from '../../services/deep-link';
import UserAvatar from '../UserAvatar.vue';
import OrgAvatar from '../OrgAvatar.vue';

export default defineComponent({
  name: 'RailSpaceList',
  components: { UserAvatar, OrgAvatar },
  props: {
    /** 窄栏紧凑态（problem L11）：图标竖列 + 悬停 tooltip；宽栏为完整列表 */
    compact: { type: Boolean, default: false }
  },
  setup() {
    const query = ref('');
    const filteredOrganizations = computed(() => organizations.value.filter((org) => org.name.toLocaleLowerCase().includes(query.value.trim().toLocaleLowerCase())));
    const isPersonal = computed(() => currentSpace.value.type === 'personal');
    const currentOrgId = currentSpaceOrgId;
    const personalSource = computed(() => personalAvatarSource());

    onMounted(() => {
      void refreshOrganizations().catch(() => {});
    });

    const select = (space: CurrentSpace) => {
      if (space.type === 'personal') {
        switchToPersonal();
      } else {
        switchToOrg(space.orgId);
      }
      // 二级菜单点击 = 切换空间并进入该桌面（用户评审：点击要有反应）
      window.dispatchEvent(new CustomEvent('spark:switch-tab', { detail: 'space' }));
    };

    /** X3/X4 落点高亮：壳层对象拖拽悬停 或 OS 文件拖入悬停 本空间项时高亮目标域 */
    const isDropHover = (space: SpaceRef) =>
      isObjectDropHover(space) ||
      (osDropState.value.active && !!osDropState.value.target && sameSpace(osDropState.value.target, space));

    /** X8 空间右键菜单（复制 spark:// 空间深链）：光标定位 + 遮罩关闭 */
    const spaceMenu = reactive({ visible: false, x: 0, y: 0, space: { type: 'personal' } as SpaceRef });
    const onSpaceContext = (space: SpaceRef, e: MouseEvent) => {
      spaceMenu.space = space;
      spaceMenu.x = Math.max(8, Math.min(e.clientX, window.innerWidth - 160));
      spaceMenu.y = Math.max(8, Math.min(e.clientY, window.innerHeight - 56));
      spaceMenu.visible = true;
    };
    const closeSpaceMenu = () => {
      spaceMenu.visible = false;
    };
    /** 复制空间链接：spark://space/<域>（X9：只含寻址字段，不含密钥数据、不暴露域清单） */
    const onCopySpaceLink = async () => {
      closeSpaceMenu();
      const url = buildSparkUrl(spaceMenu.space);
      try {
        await navigator.clipboard.writeText(url);
        ElMessage.success('已复制空间链接');
      } catch {
        ElMessage.warning(`复制失败，可手动复制：${url}`);
      }
    };

    return { organizations, filteredOrganizations, query, Search, isPersonal, currentOrgId, personalSource, personalSpaceName, personalSpaceLogo, select, isDropHover, spaceMenu, onSpaceContext, closeSpaceMenu, onCopySpaceLink };
  }
});
</script>

<style scoped>
.rail-space-list {
  display: flex;
  flex-direction: column;
  gap: 2px;
  margin: 2px 0 4px;
  /* 宽栏：整列铺满、左对齐，仅比「空间」行多一点缩进（父容器 .rail-space-block 是居中
     flex 列，不写宽度会 shrink-to-fit 导致整列看着居中） */
  width: 100%;
  padding-left: 12px;
}

/* 窄栏紧凑态（L11）：图标竖列居中，与窄栏 rail-item 同节奏 */
.rail-space-list--compact {
  align-items: center;
  gap: 4px;
  margin: 4px 0;
  width: auto;
  padding-left: 0;
}

.rail-space-item {
  display: flex;
  align-items: center;
  gap: 8px;
  width: 100%;
  padding: 6px 8px;
  border: 0;
  border-radius: var(--spark-radius-m);
  background: transparent;
  cursor: pointer;
  text-align: left;
  font-family: inherit;
  color: var(--spark-rail-text);
}

.rail-space-item--icon {
  justify-content: center;
  width: 32px;
  height: 32px;
  padding: 0;
  border-radius: var(--spark-radius-m);
}

.rail-space-item:hover {
  background: var(--spark-rail-item-hover);
}

.rail-space-item.active {
  background: var(--spark-rail-item-active);
  color: var(--spark-primary);
}

/* X3/X4 落点高亮：拖拽悬停本空间项时以主题色描边明示目标域 */
.rail-space-item.drop-hover {
  box-shadow: inset 0 0 0 2px var(--spark-primary);
  background: var(--spark-primary-light);
}

/* X8 空间右键菜单（光标定位弹层，同桌面菜单口径） */
.rail-space-menu-mask {
  position: fixed;
  inset: 0;
  z-index: var(--spark-z-overlay);
}

.rail-space-menu {
  position: fixed;
  min-width: 140px;
  padding: 4px;
  background: var(--spark-bg-card);
  border: 1px solid var(--spark-border-light);
  border-radius: var(--spark-radius-m);
  box-shadow: var(--spark-shadow-pop);
  display: flex;
  flex-direction: column;
}

.rail-space-menu-item {
  padding: 8px 12px;
  border: 0;
  border-radius: var(--spark-radius-s);
  background: transparent;
  text-align: left;
  font-size: var(--spark-font-size-base);
  color: var(--spark-text-1);
  cursor: pointer;
  font-family: inherit;
}

.rail-space-menu-item:hover {
  background: var(--spark-bg-hover);
}

.rail-space-name {
  font-size: var(--spark-font-size-placeholder);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  flex: 1;
  min-width: 0;
}
</style>

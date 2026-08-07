<!-- 组织身份模块（MinePage「组织身份」第三、四栏，仅组织空间）：
     与「我的资料」同构——第三栏=字段列表（头像/昵称/性别/地区/签名/使用个人身份，
     字段名居左、当前值居右），点击字段在第四栏打开对应编辑。
     TODO(mock): 头像/昵称/开关读写走 stores/org-identity.ts，性别/地区/签名走 stores/profile-extra.ts
     （均以 rootId@orgId 为键存 localStorage），待后端组织身份接口落地后改为调用内核（ui-space-navbar §9.4） -->
<template>
  <!-- 第三栏：组织身份字段列表 -->
  <div class="mine-list">
    <h2 class="mine-list-title">组织身份</h2>
    <div class="mine-list-items">
      <button
        v-for="field in fields"
        :key="field.key"
        type="button"
        class="mine-list-item"
        :class="{ active: activeField === field.key }"
        @click="activeField = field.key"
      >
        <el-icon
          class="mine-list-item-icon"
          :size="17"
          :style="isMobileLayout ? { color: field.color } : undefined"
        ><component :is="field.icon" /></el-icon>
        <b class="org-field-label">{{ field.label }}</b>
        <UserAvatar
          v-if="field.key === 'avatar'"
          class="org-field-avatar"
          :root-id="avatarSeed"
          :nickname="displayNickname"
          :avatar="identity.avatar"
          :size="28"
        />
        <span v-else class="mine-list-item-value">{{ field.value }}</span>
      </button>
    </div>
  </div>

  <!-- 详情/编辑：column 模式=第四栏；drawer 模式=抽屉（设置页「个人设置」） -->
  <MineDetailContainer
    :drawer="detailMode === 'drawer'"
    :open="activeField !== null"
    :title="activeFieldLabel"
    @close="activeField = null"
  >
    <!-- 组织内头像 -->
    <el-card v-if="activeField === 'avatar'" shadow="never" class="panel-card">
      <template #header>
        <h2>组织内头像</h2>
      </template>
      <div class="org-field-form">
        <AvatarPicker v-model="draftAvatar" :nickname="displayNickname" :seed="avatarSeed" />
        <div class="org-form-actions">
          <el-button type="primary" :disabled="!dirty" @click="save">保存</el-button>
          <el-button :disabled="!dirty" @click="resetDraft">还原</el-button>
        </div>
      </div>
    </el-card>

    <!-- 组织内昵称 -->
    <el-card v-else-if="activeField === 'nickname'" shadow="never" class="panel-card">
      <template #header>
        <h2>组织内昵称</h2>
      </template>
      <div class="org-field-form">
        <el-input v-model="draft.nickname" maxlength="24" placeholder="留空则显示个人昵称占位" @keyup.enter="save" />
        <div class="org-form-actions">
          <el-button type="primary" :disabled="!dirty" @click="save">保存</el-button>
          <el-button :disabled="!dirty" @click="resetDraft">还原</el-button>
        </div>
      </div>
    </el-card>

    <!-- 组织内性别 -->
    <el-card v-else-if="activeField === 'gender'" shadow="never" class="panel-card">
      <template #header>
        <h2>性别</h2>
      </template>
      <div class="org-field-form">
        <el-radio-group v-model="draft.gender">
          <el-radio value="男">男</el-radio>
          <el-radio value="女">女</el-radio>
        </el-radio-group>
        <div class="org-form-actions">
          <el-button type="primary" :disabled="!dirty" @click="save">保存</el-button>
          <el-button :disabled="!dirty" @click="resetDraft">还原</el-button>
        </div>
      </div>
    </el-card>

    <!-- 组织内地区 -->
    <el-card v-else-if="activeField === 'region'" shadow="never" class="panel-card">
      <template #header>
        <h2>地区</h2>
      </template>
      <div class="org-field-form">
        <div class="org-region-actions">
          <el-check-tag
            v-for="city in COMMON_CITIES"
            :key="city"
            :checked="draft.region === city"
            class="org-region-city"
            @change="draft.region = city"
          >
            {{ city }}
          </el-check-tag>
        </div>
        <el-input v-model="draft.region" maxlength="20" placeholder="手动输入地级市，如：杭州" @keyup.enter="save" />
        <div class="org-form-actions">
          <el-button type="primary" :disabled="!dirty" @click="save">保存</el-button>
          <el-button :disabled="!dirty" @click="resetDraft">还原</el-button>
        </div>
      </div>
    </el-card>

    <!-- 组织内签名 -->
    <el-card v-else-if="activeField === 'signature'" shadow="never" class="panel-card">
      <template #header>
        <h2>签名</h2>
      </template>
      <div class="org-field-form">
        <el-input v-model="draft.signature" maxlength="30" placeholder="用一句话介绍自己，最长 30 个字符" @keyup.enter="save" />
        <div class="org-form-actions">
          <el-button type="primary" :disabled="!dirty" @click="save">保存</el-button>
          <el-button :disabled="!dirty" @click="resetDraft">还原</el-button>
        </div>
      </div>
    </el-card>

    <!-- 使用个人身份 -->
    <el-card v-else shadow="never" class="panel-card">
      <template #header>
        <h2>使用个人身份</h2>
      </template>
      <p class="hint">开启后在该组织内所有场景使用个人头像/昵称替代组织身份。</p>
      <!-- TODO(mock): 开关仅本地生效，待后端组织身份接口（ui-space-navbar §9.3） -->
      <el-switch :model-value="identity.usePersonalIdentity" @change="toggleUsePersonal" />
    </el-card>
  </MineDetailContainer>
</template>

<script lang="ts">
import { computed, defineComponent, reactive, ref, watch, type Component, type PropType } from 'vue';
import { ElMessage } from 'element-plus';
import { Avatar, EditPen, Location, Switch, User, UserFilled } from '@element-plus/icons-vue';
import { currentSpaceOrgId } from '../../stores/current-space';
import { getOrgIdentity, setOrgIdentity } from '../../stores/org-identity';
import { orgIdentityAvatarSource } from '../../stores/avatar-sources';
import { getProfileExtra, setProfileExtra, type ProfileExtra } from '../../stores/profile-extra';
import { isMobileLayout } from '../../stores/ui-layout';
import UserAvatar from '../UserAvatar.vue';
import AvatarPicker from '../AvatarPicker.vue';
import MineDetailContainer from './MineDetailContainer.vue';

type FieldKey = 'avatar' | 'nickname' | 'gender' | 'region' | 'signature' | 'personal';

const COMMON_CITIES = ['北京', '上海', '广州', '深圳', '杭州', '成都', '重庆', '武汉', '西安', '南京'];

export default defineComponent({
  name: 'OrgIdentityModule',
  components: { UserAvatar, AvatarPicker, MineDetailContainer },
  props: {
    /** 详情展示方式：column=第四栏（个人中心），drawer=抽屉（设置页） */
    detailMode: { type: String as PropType<'column' | 'drawer'>, default: 'column' }
  },
  setup(props) {
    // drawer 模式初始无选中（抽屉关闭，只显示第三栏列表）；column 模式保持默认选中头像
    const activeField = ref<FieldKey | null>(props.detailMode === 'drawer' ? null : 'avatar');

    /** 抽屉标题：当前选中字段名 */
    const activeFieldLabel = computed(() => {
      const labels: Record<FieldKey, string> = {
        avatar: '组织内头像',
        nickname: '组织内昵称',
        gender: '性别',
        region: '地区',
        signature: '签名',
        personal: '使用个人身份'
      };
      return activeField.value ? labels[activeField.value] : '';
    });

    const identity = computed(() => getOrgIdentity(currentSpaceOrgId.value));

    /** 组织身份头像三件套（种子/名称/图片）统一走 avatar-sources，与 rail 头像同源 */
    const source = computed(() => orgIdentityAvatarSource(currentSpaceOrgId.value));

    /** 组织身份缺省占位名，待后端组织身份接口（ui-space-navbar §9.2） */
    const displayNickname = computed(() => source.value.name);

    /** 与 rail 头像同一套配色种子：rootId@orgId，与个人身份区分开；同时作为扩展字段的存储键 */
    const avatarSeed = computed(() => source.value.seed);

    // 性别/地区/签名：本地 mock 扩展字段（与我的资料同一 store，按 rootId@orgId 区分身份）
    const extra = computed(() => getProfileExtra(avatarSeed.value));

    // ---------------- 第三栏字段列表 ----------------
    // color 为移动端菜单图标色（微信式每项一色，与 MinePage 一级菜单同规则、同色系色板，桌面端不使用）
    const fields = computed<Array<{ key: FieldKey; label: string; value: string; icon: Component; color: string }>>(() => [
      { key: 'avatar', label: '头像', value: '', icon: Avatar, color: '#00b8a9' },
      { key: 'nickname', label: '昵称', value: displayNickname.value, icon: User, color: '#3296fa' },
      { key: 'gender', label: '性别', value: extra.value.gender || '未设置', icon: UserFilled, color: '#7b61ff' },
      { key: 'region', label: '地区', value: extra.value.region || '未设置', icon: Location, color: '#ff7d00' },
      { key: 'signature', label: '签名', value: extra.value.signature || '未设置', icon: EditPen, color: '#34c19b' },
      {
        key: 'personal',
        label: '使用个人身份',
        value: identity.value.usePersonalIdentity ? '已开启' : '已关闭',
        icon: Switch,
        color: '#f7b500'
      }
    ]);

    // ---------------- 字段编辑草稿（昵称/头像 + mock 扩展字段共用一个保存链路） ----------------
    const draftAvatar = ref(identity.value.avatar);
    const draft = reactive<{ nickname: string } & ProfileExtra>({
      nickname: identity.value.nickname,
      gender: extra.value.gender,
      region: extra.value.region,
      signature: extra.value.signature
    });

    const resetDraft = () => {
      draftAvatar.value = identity.value.avatar;
      draft.nickname = identity.value.nickname;
      draft.gender = extra.value.gender;
      draft.region = extra.value.region;
      draft.signature = extra.value.signature;
    };

    // 外部写入（如保存后 store 回写）时同步草稿
    watch(() => [identity.value, extra.value], resetDraft);

    const dirty = computed(
      () =>
        draftAvatar.value !== identity.value.avatar ||
        draft.nickname.trim() !== identity.value.nickname ||
        draft.gender !== extra.value.gender ||
        draft.region !== extra.value.region ||
        draft.signature !== extra.value.signature
    );

    const save = () => {
      setOrgIdentity(currentSpaceOrgId.value, {
        nickname: draft.nickname.trim(),
        avatar: draftAvatar.value
      });
      setProfileExtra(avatarSeed.value, {
        gender: draft.gender,
        region: draft.region.trim(),
        signature: draft.signature.trim()
      });
      ElMessage.success('已保存');
    };

    const toggleUsePersonal = (value: string | number | boolean) => {
      setOrgIdentity(currentSpaceOrgId.value, { usePersonalIdentity: value === true });
    };

    return {
      activeField,
      activeFieldLabel,
      fields,
      isMobileLayout,
      identity,
      displayNickname,
      avatarSeed,
      draft,
      draftAvatar,
      dirty,
      save,
      resetDraft,
      toggleUsePersonal,
      COMMON_CITIES
    };
  }
});
</script>

<style scoped>
/* 字段名（第三栏）：常规字重，选中行加粗 */
.org-field-label {
  font-size: 14px;
  font-weight: 400;
}

.mine-list-item.active .org-field-label {
  font-weight: 600;
}

/* 移动端：菜单主文字统一 16px；点行即开详情（无选中态），active 不再加粗 */
@media (max-width: 768px) {
  .org-field-label {
    font-size: 16px;
  }

  .mine-list-item.active .org-field-label {
    font-weight: 400;
  }
}

/* 头像字段值：与文本值一样靠右对齐 */
.org-field-avatar {
  flex-shrink: 0;
  margin-left: auto;
}

/* 第四栏字段编辑：控件自上而下排列 */
.org-field-form {
  display: flex;
  flex-direction: column;
  align-items: flex-start;
  gap: 16px;
}

.org-region-actions {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
  align-items: center;
}

.org-region-city {
  margin: 0;
}

.org-form-actions {
  display: flex;
  gap: 12px;
}

.org-form-actions .el-button + .el-button {
  margin-left: 0;
}
</style>

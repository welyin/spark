<!-- 我的资料模块（MinePage「我的资料」第三、四栏）：
     第三栏=资料字段列表（字段名居左、当前值居右），合并原「基本信息/身份信息」，
     安全设置/隐私设置两个占位页已移除；点击字段在第四栏打开对应编辑。
     昵称/头像走 rootIdentity.updateProfile 真实接口，性别/地区/签名为本地 mock（stores/profile-extra） -->
<template>
  <!-- 第三栏：资料字段列表 -->
  <div class="mine-list">
    <h2 class="mine-list-title">我的资料</h2>
    <div class="mine-list-items">
      <button
        v-for="field in fields"
        :key="field.key"
        type="button"
        class="mine-list-item"
        :class="{ active: activeField === field.key }"
        @click="activeField = field.key"
      >
        <el-icon class="mine-list-item-icon" :size="17"><component :is="field.icon" /></el-icon>
        <b class="profile-field-label">{{ field.label }}</b>
        <UserAvatar
          v-if="field.key === 'avatar'"
          class="profile-field-avatar"
          :root-id="rootId"
          :nickname="nickname"
          :avatar="avatar"
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
    <!-- 头像 -->
    <el-card v-if="activeField === 'avatar'" shadow="never" class="panel-card">
      <template #header>
        <h2>头像</h2>
      </template>
      <div class="profile-avatar-control">
        <UserAvatar :root-id="rootId" :nickname="nickname" :avatar="avatar" :size="64" />
        <el-button size="small" :disabled="saving" @click="triggerAvatarSelect">更换头像</el-button>
      </div>
    </el-card>

    <!-- 昵称 -->
    <el-card v-else-if="activeField === 'nickname'" shadow="never" class="panel-card">
      <template #header>
        <h2>昵称</h2>
      </template>
      <div class="profile-field-form">
        <el-input v-model="draft.nickname" maxlength="24" placeholder="中英文均可，最长 24 个字符" @keyup.enter="save" />
        <div class="profile-form-actions">
          <el-button type="primary" :loading="saving" :disabled="!dirty || !draft.nickname.trim()" @click="save">保存</el-button>
          <el-button :disabled="!dirty" @click="resetDraft">还原</el-button>
        </div>
      </div>
    </el-card>

    <!-- 性别 -->
    <el-card v-else-if="activeField === 'gender'" shadow="never" class="panel-card">
      <template #header>
        <h2>性别</h2>
      </template>
      <div class="profile-field-form">
        <el-radio-group v-model="draft.gender">
          <el-radio value="男">男</el-radio>
          <el-radio value="女">女</el-radio>
        </el-radio-group>
        <div class="profile-form-actions">
          <el-button type="primary" :loading="saving" :disabled="!dirty" @click="save">保存</el-button>
          <el-button :disabled="!dirty" @click="resetDraft">还原</el-button>
        </div>
      </div>
    </el-card>

    <!-- 地区 -->
    <el-card v-else-if="activeField === 'region'" shadow="never" class="panel-card">
      <template #header>
        <h2>地区</h2>
      </template>
      <div class="profile-field-form">
        <div class="profile-region-actions">
          <el-button size="small" :loading="locating" @click="locate">定位</el-button>
          <el-check-tag
            v-for="city in COMMON_CITIES"
            :key="city"
            :checked="draft.region === city"
            class="profile-region-city"
            @change="draft.region = city"
          >
            {{ city }}
          </el-check-tag>
        </div>
        <el-input v-model="draft.region" maxlength="20" placeholder="手动输入地级市，如：杭州" @keyup.enter="save" />
        <div class="profile-form-actions">
          <el-button type="primary" :loading="saving" :disabled="!dirty" @click="save">保存</el-button>
          <el-button :disabled="!dirty" @click="resetDraft">还原</el-button>
        </div>
      </div>
    </el-card>

    <!-- 签名 -->
    <el-card v-else-if="activeField === 'signature'" shadow="never" class="panel-card">
      <template #header>
        <h2>签名</h2>
      </template>
      <div class="profile-field-form">
        <el-input v-model="draft.signature" maxlength="30" placeholder="用一句话介绍自己，最长 30 个字符" @keyup.enter="save" />
        <div class="profile-form-actions">
          <el-button type="primary" :loading="saving" :disabled="!dirty" @click="save">保存</el-button>
          <el-button :disabled="!dirty" @click="resetDraft">还原</el-button>
        </div>
      </div>
    </el-card>

    <!-- 身份 ID（只读：缩略 + 复制 + 二维码） -->
    <el-card v-else shadow="never" class="panel-card">
      <template #header>
        <h2>身份 ID</h2>
      </template>
      <p class="hint">
        <TermLabel term="rootId" /> 是你的去中心化身份标识，加朋友、加入组织时提供给对方即可。
      </p>
      <NodeIdentityInfo class="profile-identity-rows" :rows="identityRows" />
    </el-card>

    <input ref="fileInput" type="file" accept="image/*" class="profile-file-input" @change="onAvatarChange" />
  </MineDetailContainer>
</template>

<script lang="ts">
import { computed, defineComponent, reactive, ref, watch, type Component, type PropType } from 'vue';
import { ElMessage } from 'element-plus';
import { Avatar, EditPen, Key, Location, User, UserFilled } from '@element-plus/icons-vue';
import { errorMessage } from '../../utils/ipc';
import { fileToAvatarDataUrl } from '../../utils/avatar';
import { shortenMiddle } from '../../utils/format';
import { getProfileExtra, setProfileExtra, type ProfileExtra } from '../../stores/profile-extra';
import UserAvatar from '../UserAvatar.vue';
import TermLabel from '../common/TermLabel.vue';
import NodeIdentityInfo, { type NodeIdentityRow } from '../common/NodeIdentityInfo.vue';
import MineDetailContainer from './MineDetailContainer.vue';

type FieldKey = 'avatar' | 'nickname' | 'gender' | 'region' | 'signature' | 'identity';

const COMMON_CITIES = ['北京', '上海', '广州', '深圳', '杭州', '成都', '重庆', '武汉', '西安', '南京'];

export default defineComponent({
  name: 'ProfileModule',
  components: { UserAvatar, TermLabel, NodeIdentityInfo, MineDetailContainer },
  props: {
    rootId: { type: String, default: '' },
    nickname: { type: String, default: '' },
    avatar: { type: String, default: '' },
    /** 详情展示方式：column=第四栏（个人设置页），drawer=抽屉（设置页） */
    detailMode: { type: String as PropType<'column' | 'drawer'>, default: 'column' }
  },
  emits: ['profile-updated'],
  setup(props, { emit }) {
    // drawer 模式初始无选中（抽屉关闭，只显示列表栏）；column 模式保持默认选中头像
    const activeField = ref<FieldKey | null>(props.detailMode === 'drawer' ? null : 'avatar');
    const saving = ref(false);

    // 性别/地区/签名：本地 mock 扩展字段（见 stores/profile-extra）
    const extra = computed(() => getProfileExtra(props.rootId));

    /** 抽屉标题：当前选中字段名 */
    const activeFieldLabel = computed(() => {
      const labels: Record<FieldKey, string> = {
        avatar: '头像',
        nickname: '昵称',
        gender: '性别',
        region: '地区',
        signature: '签名',
        identity: '身份 ID'
      };
      return activeField.value ? labels[activeField.value] : '';
    });

    // ---------------- 第三栏字段列表 ----------------
    const fields = computed<Array<{ key: FieldKey; label: string; value: string; icon: Component }>>(() => [
      { key: 'avatar', label: '头像', value: '', icon: Avatar },
      { key: 'nickname', label: '昵称', value: props.nickname || '未设置', icon: User },
      { key: 'gender', label: '性别', value: extra.value.gender || '未设置', icon: UserFilled },
      { key: 'region', label: '地区', value: extra.value.region || '未设置', icon: Location },
      { key: 'signature', label: '签名', value: extra.value.signature || '未设置', icon: EditPen },
      { key: 'identity', label: '身份 ID', value: props.rootId ? shortenMiddle(props.rootId, 8, 4) : '未创建', icon: Key }
    ]);

    // ---------------- 字段编辑草稿（昵称 + mock 扩展字段共用一个保存链路） ----------------
    const draft = reactive<{ nickname: string } & ProfileExtra>({
      nickname: props.nickname,
      gender: extra.value.gender,
      region: extra.value.region,
      signature: extra.value.signature
    });

    const resetDraft = () => {
      draft.nickname = props.nickname;
      draft.gender = extra.value.gender;
      draft.region = extra.value.region;
      draft.signature = extra.value.signature;
    };

    // 外部资料变化（如保存成功后父级回写）时同步草稿
    watch(() => [props.nickname, extra.value], resetDraft);

    const dirty = computed(
      () =>
        draft.nickname.trim() !== props.nickname ||
        draft.gender !== extra.value.gender ||
        draft.region !== extra.value.region ||
        draft.signature !== extra.value.signature
    );

    const save = async () => {
      saving.value = true;
      try {
        // 昵称走真实接口（有变更才调用）
        if (draft.nickname.trim() !== props.nickname) {
          const result = await window.electronAPI.rootIdentity.updateProfile({ nickname: draft.nickname.trim() });
          emit('profile-updated', result);
        }
        // 性别/地区/签名：mock 存 localStorage（见 stores/profile-extra）
        setProfileExtra(props.rootId, {
          gender: draft.gender,
          region: draft.region.trim(),
          signature: draft.signature.trim()
        });
        ElMessage.success('已保存');
      } catch (error) {
        ElMessage.error(`保存失败：${errorMessage(error)}`);
      } finally {
        saving.value = false;
      }
    };

    // ---------------- 头像：选图上传（复用 AvatarPicker 同款压缩链路） ----------------
    const fileInput = ref<HTMLInputElement | null>(null);

    const triggerAvatarSelect = () => {
      fileInput.value?.click();
    };

    const onAvatarChange = async (event: Event) => {
      const input = event.target as HTMLInputElement;
      const file = input.files?.[0];
      input.value = '';
      if (!file) {
        return;
      }
      saving.value = true;
      try {
        const dataUrl = await fileToAvatarDataUrl(file);
        const result = await window.electronAPI.rootIdentity.updateProfile({ avatar: dataUrl });
        emit('profile-updated', result);
        ElMessage.success('头像已更新');
      } catch (error) {
        ElMessage.error(`头像更新失败：${errorMessage(error)}`);
      } finally {
        saving.value = false;
      }
    };

    // TODO(mock): Geolocation 只能拿到经纬度，缺少逆地理编码服务无法解析地级市；
    // 当前仅演示定位交互，成功/失败后仍需手动选择或输入城市，待接入逆地理编码后自动填充
    const locating = ref(false);
    const locate = () => {
      if (!navigator.geolocation) {
        ElMessage.warning('当前环境不支持定位，请手动选择城市');
        return;
      }
      locating.value = true;
      navigator.geolocation.getCurrentPosition(
        () => {
          locating.value = false;
          ElMessage.info('定位能力待接入逆地理编码服务，请先手动选择城市');
        },
        () => {
          locating.value = false;
          ElMessage.warning('定位失败，请手动选择城市');
        },
        { timeout: 5000 }
      );
    };

    // ---------------- 身份 ID（只读） ----------------
    const identityRows = computed<NodeIdentityRow[]>(() => [
      { label: '身份 ID', term: 'rootId', value: props.rootId, copyable: true, emptyText: '未创建' }
    ]);

    return {
      activeField,
      activeFieldLabel,
      fields,
      saving,
      draft,
      dirty,
      save,
      resetDraft,
      fileInput,
      triggerAvatarSelect,
      onAvatarChange,
      locating,
      locate,
      COMMON_CITIES,
      identityRows
    };
  }
});
</script>

<style scoped>
/* 字段名（第三栏）：常规字重，选中行加粗 */
.profile-field-label {
  font-size: 14px;
  font-weight: 400;
}

.mine-list-item.active .profile-field-label {
  font-weight: 600;
}

/* 头像字段值：与文本值一样靠右对齐 */
.profile-field-avatar {
  flex-shrink: 0;
  margin-left: auto;
}

/* 第四栏字段编辑：控件自上而下排列 */
.profile-field-form {
  display: flex;
  flex-direction: column;
  align-items: flex-start;
  gap: 16px;
}

.profile-avatar-control {
  display: flex;
  align-items: center;
  gap: 12px;
}

.profile-region-actions {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
  align-items: center;
}

.profile-region-city {
  margin: 0;
}

.profile-form-actions {
  display: flex;
  gap: 12px;
}

.profile-form-actions .el-button + .el-button {
  margin-left: 0;
}

.profile-identity-rows {
  margin-top: 16px;
}

.profile-file-input {
  display: none;
}
</style>

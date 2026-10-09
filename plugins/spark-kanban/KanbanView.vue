<!--
  任务看板插件（spark-kanban）· 主视图（app 视图）。

  设计依据：kanban.md v0.2 §4（界面与交互要点）+ 拍板口径：
  - 列横向排布、卡片纵向堆叠；列头显示卡片数与列语义（triage/stage/terminal
    徽标区分）；triage 列新卡片「未分诊」醒目提示；
  - 卡片 = 标题 + 类型徽标（bug/建议/任务/原生）+ 指派人 + 子事务决议徽标
    （开放/待确认/已生效/已否决，readResolution 原样呈现不美化）；
  - 拖动转列即时渲染 + 后台提交：原生卡片 → move 操作入 append-only 流；
    绑定卡片 → 向子事务提交签名状态操作（档三-1），失败（权限不足/规则拒绝）
    回滚并展示原因；拖入终态列弹确认（终态 = 子事务决议生效，内核机制，
    看板 MVP 只引导说明，不代为发起决议）；
  - 列内手动排序 = local 视图偏好（档三-22，不进同步流量）；
  - 降级纪律：sdk.affairs 缺席（独立使用/移动端只读形态）→ 绑定相关入口隐藏、
    看板降级为纯原生卡片模式，不报错不阻塞。
-->
<template>
  <section class="spark-kanban">
    <el-alert v-if="message" :title="message" :type="messageType" :closable="false" show-icon class="message" />

    <el-card shadow="never" class="header-card">
      <div class="header-row">
        <div>
          <p class="eyebrow">任务看板</p>
          <h2>开发任务与缺陷跟踪</h2>
          <p class="lede">卡片即事务子事务的视图与操作入口；看板只持有呈现与编排数据，权威状态在内核事务容器。</p>
        </div>
        <el-button @click="reloadAll" :loading="loading">刷新</el-button>
      </div>

      <el-form v-if="!isPersonal && orgOptions.length > 0" label-position="top" class="selectors">
        <el-form-item label="组织">
          <el-select v-model="selectedOrgId" @change="onOrgChange" placeholder="选择组织">
            <el-option
              v-for="org in orgOptions"
              :key="org.orgId"
              :label="`${org.name} (${org.orgId.slice(0, 8)}...)`"
              :value="org.orgId"
            />
          </el-select>
        </el-form-item>
      </el-form>

      <el-empty v-if="!isPersonal && orgOptions.length === 0" description="你还没有加入任何组织。" />

      <div v-if="spaceReady" class="meta-row">
        <el-tag type="info">{{ isPersonal ? '个人空间' : '组织空间' }}</el-tag>
        <el-tag v-if="!isPersonal" :type="isAdmin ? 'danger' : 'warning'">{{ isAdmin ? '组织管理员' : '组织成员' }}</el-tag>
        <el-tag :type="service?.affairsAvailable ? 'success' : 'info'">
          {{ service?.affairsAvailable ? '事务模块已连接' : '事务模块不可用（原生卡片模式）' }}
        </el-tag>
      </div>

      <div v-if="spaceReady" class="board-row">
        <el-select v-model="selectedBoardId" placeholder="选择看板" @change="onBoardChange">
          <el-option v-for="board in boards" :key="board.id" :label="board.name" :value="board.id" />
        </el-select>
        <el-button v-if="canManage" type="primary" size="small" @click="boardDialogVisible = true">新建看板</el-button>
      </div>
    </el-card>

    <template v-if="activeBoard">
      <el-alert
        v-if="activeBoard.contextAffairId && !service?.affairsAvailable"
        type="info"
        :closable="false"
        show-icon
        title="本看板绑定了项目议题，但事务模块当前不可用——子事务卡片暂不显示，原生卡片功能不受影响（降级运行）。"
      />

      <div class="board-toolbar">
        <div class="header-row-inner">
          <h3>{{ activeBoard.name }}</h3>
          <el-tag v-if="activeBoard.contextAffairId" size="small" type="success">
            议题上下文 {{ activeBoard.contextAffairId.slice(0, 12) }}…
          </el-tag>
        </div>
        <div class="header-row-inner">
          <el-button v-if="service?.affairsAvailable && canEdit" size="small" text @click="bindAffairDialogVisible = true">
            挂载子事务
          </el-button>
          <el-button v-if="canEdit" type="primary" size="small" @click="openCardDialog">新建卡片</el-button>
        </div>
      </div>

      <!-- 看板主视图：列横向排布、卡片纵向堆叠 -->
      <div class="board-columns">
        <div
          v-for="columnView in boardView"
          :key="columnView.column.id"
          class="board-column"
          :class="[`kind-${columnView.column.kind}`, { 'drop-active': dropColumnId === columnView.column.id }]"
          @dragover.prevent="onColumnDragOver(columnView.column.id)"
          @dragleave="onColumnDragLeave(columnView.column.id)"
          @drop.prevent="onDrop(columnView.column, null, $event)"
        >
          <div class="column-header">
            <el-tag size="small" :type="columnKindTagType(columnView.column.kind)">{{ columnKindLabel(columnView.column.kind) }}</el-tag>
            <strong class="column-title">{{ columnView.column.title }}</strong>
            <span class="column-count">{{ columnView.cards.length }}</span>
            <el-badge v-if="columnView.freshCount > 0" :value="columnView.freshCount" type="danger">
              <el-tag size="small" type="danger">未分诊</el-tag>
            </el-badge>
          </div>

          <div
            v-for="card in columnView.cards"
            :key="card.ref"
            class="kanban-card"
            :class="{ highlighted: highlightedCardRef === card.ref, 'affair-card': card.kind === 'affair' }"
            draggable="true"
            @dragstart="onCardDragStart(card, $event)"
            @dragover.prevent.stop
            @drop.prevent.stop="onDrop(columnView.column, card.ref, $event)"
            @click="openCardDetail(card)"
          >
            <div class="card-title-row">
              <el-tag size="small" :type="card.kind === 'native' ? 'info' : 'warning'">
                {{ card.kind === 'native' ? '原生' : affairTypeLabel(card.card.affairType) }}
              </el-tag>
              <el-tag
                v-if="card.kind === 'affair' && card.card.resolution !== 'open'"
                size="small"
                :type="card.card.resolution === 'effective' ? 'success' : card.card.resolution === 'pending' ? 'warning' : 'danger'"
              >
                {{ resolutionLabel(card.card.resolution) }}
              </el-tag>
              <span v-if="card.kind === 'native' && card.card.signed" class="signed-mark">已签名</span>
            </div>
            <p class="card-title">{{ card.kind === 'native' ? card.card.title : card.card.title }}</p>
            <div class="card-meta">
              <span v-if="card.kind === 'native' && card.card.assigneeRootId" class="assignee">
                → {{ card.card.assigneeRootId.slice(0, 12) }}…
              </span>
              <span v-if="card.kind === 'native' && card.card.commentCount > 0">评论 {{ card.card.commentCount }}</span>
              <span v-if="card.kind === 'affair'">{{ card.card.affairId.slice(0, 8) }}…</span>
            </div>
          </div>

          <el-empty v-if="columnView.cards.length === 0" description="" :image-size="40" />
        </div>
      </div>
    </template>

    <el-empty v-else-if="spaceReady && boards.length === 0" description="暂无看板，请先创建" />

    <!-- 新建看板对话框 -->
    <el-dialog v-model="boardDialogVisible" title="新建看板" width="440px">
      <el-form label-position="top">
        <el-form-item label="看板名称">
          <el-input v-model="boardDraft.name" :maxlength="40" show-word-limit placeholder="如：迭代 12 / 缺陷分诊" />
        </el-form-item>
        <el-form-item label="项目议题 affairId（可选；填入后自动聚合其子事务为卡片，新回流反馈落入待分诊列）">
          <el-input v-model="boardDraft.contextAffairId" placeholder="64 位小写 hex；留空 = 纯原生卡片看板" />
        </el-form-item>
      </el-form>
      <p class="hint">默认列模板：待分诊 → 待办 → 进行中 → 待验证 → 完成（列—状态映射随看板配置同步）。</p>
      <template #footer>
        <el-button @click="boardDialogVisible = false">取消</el-button>
        <el-button type="primary" :loading="boardSaving" @click="submitBoard">创建</el-button>
      </template>
    </el-dialog>

    <!-- 新建卡片对话框 -->
    <el-dialog v-model="cardDialogVisible" title="新建卡片" width="520px">
      <el-form label-position="top">
        <el-form-item label="标题">
          <el-input v-model="cardDraft.title" :maxlength="120" show-word-limit placeholder="一句话说清任务/缺陷" />
        </el-form-item>
        <el-form-item label="描述（可选）">
          <el-input v-model="cardDraft.description" type="textarea" :rows="5" :maxlength="5000" show-word-limit />
        </el-form-item>
        <el-form-item v-if="!isPersonal" label="指派人（可选，组织成员 RootID）">
          <el-select v-model="cardDraft.assigneeRootId" clearable placeholder="不指派">
            <el-option
              v-for="member in activeOrgMembers"
              :key="member.rootId"
              :label="`${member.rootId.slice(0, 16)}…（${member.role === 'admin' ? '管理员' : '成员'}）`"
              :value="member.rootId"
            />
          </el-select>
        </el-form-item>
      </el-form>
      <p class="hint">卡片数据仅追加、不覆盖：转列/指派/评论都是操作流留痕，其他成员可核验。创建将请求一次域身份签名（防抵赖），拒绝签名也会照建（少「已签名」徽标）。</p>
      <template #footer>
        <el-button @click="cardDialogVisible = false">取消</el-button>
        <el-button type="primary" :loading="cardSaving" @click="submitCard">创建</el-button>
      </template>
    </el-dialog>

    <!-- 挂载子事务对话框（手动绑定路径，档三-20：子事务 → 多看板允许） -->
    <el-dialog v-model="bindAffairDialogVisible" title="挂载子事务到本看板" width="480px">
      <el-form label-position="top">
        <el-form-item label="子事务 affairId（须本机已关注该事务）">
          <el-input v-model="bindAffairId" placeholder="64 位小写 hex" />
        </el-form-item>
      </el-form>
      <p class="hint">绑定记录 append-only 留痕；解绑 = 追加解绑记录，历史可考。同一子事务可挂到多个看板（绑定记录即视图数据）。</p>
      <template #footer>
        <el-button @click="bindAffairDialogVisible = false">取消</el-button>
        <el-button type="primary" :loading="bindSaving" @click="submitBindAffair">挂载</el-button>
      </template>
    </el-dialog>

    <!-- 拖入终态列确认（绑定卡片：终态 = 子事务决议生效，内核机制） -->
    <el-dialog v-model="terminalDialogVisible" title="终态列说明" width="480px">
      <el-alert
        type="warning"
        :closable="false"
        show-icon
        title="「完成」是终态列：卡片的终态严格来自子事务的采纳/关闭决议（经公示期生效，内核机制），不是看板自身能完成的动作。"
      />
      <p class="hint">
        看板 MVP 不代为发起决议流程——请到项目/议题客户端完成该子事务的采纳或关闭决议；决议生效后卡片会自动进入本列（公示期内显示「待确认」徽标）。
      </p>
      <template #footer>
        <el-button type="primary" @click="terminalDialogVisible = false">知道了</el-button>
      </template>
    </el-dialog>

    <!-- 卡片详情对话框 -->
    <el-dialog v-model="detailVisible" :title="detailTitle" width="560px">
      <template v-if="detailCard">
        <!-- 原生卡片详情：自身操作流 -->
        <template v-if="detailCard.kind === 'native'">
          <div class="detail-tags">
            <el-tag size="small" type="info">原生卡片</el-tag>
            <el-tag size="small">{{ columnTitle(detailCard.columnId) }}</el-tag>
            <el-tag v-if="detailCard.card.signed" size="small" type="success">已签名</el-tag>
          </div>
          <p class="detail-content">{{ detailCard.card.description || '（无描述）' }}</p>
          <div class="post-meta">
            <span>创建者 {{ detailCard.card.createdBy }}</span>
            <span>{{ formatDate(detailCard.card.createdAt) }}</span>
          </div>

          <div v-if="canEdit && !isPersonal" class="assign-row">
            <el-select v-model="assignDraft" clearable placeholder="指派给…" size="small">
              <el-option
                v-for="member in activeOrgMembers"
                :key="member.rootId"
                :label="`${member.rootId.slice(0, 16)}…（${member.role === 'admin' ? '管理员' : '成员'}）`"
                :value="member.rootId"
              />
            </el-select>
            <el-button size="small" :loading="assigning" @click="submitAssign(detailCard.card.cardId)">指派</el-button>
          </div>

          <div v-if="service?.affairsAvailable && canEdit" class="bind-row">
            <el-input v-model="bindNativeAffairId" size="small" placeholder="绑定到子事务 affairId（升级为事务卡片）" />
            <el-button size="small" :loading="bindingNative" @click="submitBindNative(detailCard.card.cardId)">绑定</el-button>
          </div>
          <p v-if="nativeBindingOf(detailCard.card.cardId)" class="hint">
            已绑定子事务：{{ nativeBindingOf(detailCard.card.cardId)?.affairId.slice(0, 20) }}…（原生卡片 → 单一子事务，档三-20）
          </p>

          <el-divider content-position="left">评论（{{ detailComments.length }}）</el-divider>
          <div v-for="comment in detailComments" :key="comment.opId" class="comment-item">
            <div class="post-meta">
              <span class="author">{{ comment.authorRootId }}</span>
              <span>{{ formatDate(comment.createdAt) }}</span>
            </div>
            <p class="comment-content">
              <el-tag v-if="comment.signature" type="success" size="small" class="sig-tag">已签名</el-tag>
              {{ comment.text }}
            </p>
          </div>
          <div v-if="canEdit" class="reply-editor">
            <el-input v-model="commentDraft" type="textarea" :rows="2" :maxlength="2000" placeholder="写下评论" />
            <el-button size="small" type="primary" :loading="commenting" @click="submitComment(detailCard.card.cardId)">评论</el-button>
          </div>
        </template>

        <!-- 绑定卡片详情：子事务投影（本体在事务容器，看板只读呈现） -->
        <template v-else>
          <div class="detail-tags">
            <el-tag size="small" type="warning">{{ affairTypeLabel(detailCard.card.affairType) }}</el-tag>
            <el-tag size="small">{{ columnTitle(detailCard.columnId) }}</el-tag>
            <el-tag
              size="small"
              :type="detailCard.card.resolution === 'effective' ? 'success' : detailCard.card.resolution === 'pending' ? 'warning' : detailCard.card.resolution === 'vetoed' ? 'danger' : 'info'"
            >
              {{ resolutionLabel(detailCard.card.resolution) }}
            </el-tag>
          </div>
          <p class="detail-content">{{ detailCard.card.summary || '（无摘要）' }}</p>
          <div class="post-meta">
            <span>子事务 {{ detailCard.card.affairId }}</span>
          </div>
          <p class="hint">
            本卡片是事务子事务的视图与操作入口：业务内容（缺陷描述、采纳决议）以子事务为准，
            列位置由子事务日志内的签名状态操作推导（档三-1），换一个客户端/重装看板可原样重推。
            操作日志时间线请在项目/议题客户端查看。
          </p>
          <p v-if="detailCard.card.resolution === 'pending'" class="hint pending-hint">
            决议公示期内：任何副本此刻看到的都应是「待确认」，生效后才进入「完成」列（时间语义诚实边界）。
          </p>
        </template>
      </template>
    </el-dialog>
  </section>
</template>

<script lang="ts">
import { computed, defineComponent, onMounted, onUnmounted, ref, watch } from 'vue';
import { ElMessage } from 'element-plus';
import { ensurePluginSDK } from '../../packages/plugin-sdk/src';
import type { PluginCardActionPayload, PluginSDK } from '../../packages/plugin-sdk/src';
import {
  buildBoardView,
  canEditCards,
  canManageBoard,
  deriveCardComments,
  deriveNativeCards,
  nativeCardBinding,
  RESOLUTION_BADGE_LABELS,
  type KanbanAffairCard,
  type KanbanBinding,
  type KanbanBoard,
  type KanbanCardOp,
  type KanbanCardView,
  type KanbanColumn,
  type KanbanColumnView,
  type KanbanResolutionBadge,
  type KanbanViewPrefs
} from './model';
import { KanbanService } from './service';

type OrganizationView = {
  orgId: string;
  name: string;
  members: Array<{ rootId: string; role: 'admin' | 'member' }>;
};

/** 卡片回调后高亮时长（ms） */
const HIGHLIGHT_DURATION_MS = 2500;

export default defineComponent({
  name: 'KanbanView',
  props: {
    /**
     * 运行上下文（桥握手 ctx.space 经入口注入；库包形态下由组合者注入看板上下文）。
     * 个人空间 = 固定 'personal' 数据域（自设备间同步）；组织空间 = 组织选择器。
     */
    pluginContext: {
      type: Object as () => { spaceType?: 'personal' | 'org'; orgId?: string } | undefined,
      required: false,
      default: undefined
    }
  },
  setup(props) {
    const sdk = ref<PluginSDK | null>(null);
    const service = ref<KanbanService | null>(null);
    const loading = ref(false);
    const message = ref('');
    const messageType = ref<'info' | 'success' | 'warning' | 'error'>('info');

    const currentRootId = ref<string | null>(null);
    const isPersonal = computed(() => props.pluginContext?.spaceType === 'personal');
    const orgOptions = ref<OrganizationView[]>([]);
    const selectedOrgId = ref('');

    const boards = ref<KanbanBoard[]>([]);
    const selectedBoardId = ref('');
    const cardOps = ref<KanbanCardOp[]>([]);
    const bindings = ref<KanbanBinding[]>([]);
    const affairCards = ref<KanbanAffairCard[]>([]);
    const viewPrefs = ref<KanbanViewPrefs | null>(null);

    // 对话框
    const boardDialogVisible = ref(false);
    const boardSaving = ref(false);
    const boardDraft = ref({ name: '', contextAffairId: '' });
    const cardDialogVisible = ref(false);
    const cardSaving = ref(false);
    const cardDraft = ref({ title: '', description: '', assigneeRootId: '' });
    const bindAffairDialogVisible = ref(false);
    const bindSaving = ref(false);
    const bindAffairId = ref('');
    const terminalDialogVisible = ref(false);
    const detailVisible = ref(false);
    const detailCard = ref<KanbanCardView | null>(null);
    const commentDraft = ref('');
    const commenting = ref(false);
    const assignDraft = ref('');
    const assigning = ref(false);
    const bindNativeAffairId = ref('');
    const bindingNative = ref(false);

    // 拖拽状态
    const dragCardRef = ref('');
    const dropColumnId = ref('');
    const highlightedCardRef = ref('');

    let highlightTimer: ReturnType<typeof setTimeout> | null = null;
    let offCardAction: (() => void) | null = null;

    /** 当前数据域 id（组织空间 = orgId；个人空间 = 'personal'） */
    const spaceId = computed(() => (isPersonal.value ? 'personal' : selectedOrgId.value));
    const spaceReady = computed(() => Boolean(spaceId.value));

    const activeOrg = computed(() => orgOptions.value.find((org) => org.orgId === selectedOrgId.value) ?? null);
    const activeOrgMembers = computed(() => activeOrg.value?.members ?? []);
    const currentOrgRole = computed<'admin' | 'member' | null>(() => {
      if (isPersonal.value) {
        // 个人空间：本人数据域，全权（service 层对 orgId='personal' 放行）
        return 'admin';
      }
      if (!activeOrg.value || !currentRootId.value) {
        return null;
      }
      return activeOrg.value.members.find((member) => member.rootId === currentRootId.value)?.role ?? null;
    });
    const canManage = computed(() => isPersonal.value || canManageBoard(currentOrgRole.value));
    const canEdit = computed(() => isPersonal.value || canEditCards(currentOrgRole.value));

    const activeBoard = computed(() => boards.value.find((board) => board.id === selectedBoardId.value) ?? null);

    const nativeCards = computed(() =>
      activeBoard.value ? deriveNativeCards(activeBoard.value.id, cardOps.value) : []
    );

    const boardView = computed<KanbanColumnView[]>(() => {
      if (!activeBoard.value) {
        return [];
      }
      return buildBoardView(activeBoard.value, nativeCards.value, affairCards.value, viewPrefs.value);
    });

    const detailTitle = computed(() => {
      if (!detailCard.value) {
        return '卡片详情';
      }
      return detailCard.value.kind === 'native' ? detailCard.value.card.title : detailCard.value.card.title;
    });

    const detailComments = computed(() => {
      if (!detailCard.value || detailCard.value.kind !== 'native' || !activeBoard.value) {
        return [];
      }
      return deriveCardComments(activeBoard.value.id, detailCard.value.card.cardId, cardOps.value);
    });

    const setMessage = (text: string, type: 'info' | 'success' | 'warning' | 'error' = 'info') => {
      message.value = text;
      messageType.value = type;
    };

    const ensureSdk = async () => {
      if (!sdk.value) {
        sdk.value = await ensurePluginSDK();
        service.value = new KanbanService(sdk.value);
      }
      return sdk.value;
    };

    const loadOrganizations = async () => {
      if (isPersonal.value) {
        orgOptions.value = [];
        return;
      }
      const plugin = await ensureSdk();
      const all = await plugin.runtime.listMineOrganizations();
      orgOptions.value = all as OrganizationView[];
      const preferredOrgId = props.pluginContext?.orgId;
      if (preferredOrgId && orgOptions.value.some((org) => org.orgId === preferredOrgId)) {
        selectedOrgId.value = preferredOrgId;
        return;
      }
      if (!orgOptions.value.some((org) => org.orgId === selectedOrgId.value)) {
        selectedOrgId.value = orgOptions.value[0]?.orgId ?? '';
      }
    };

    /** 看板数据加载（sdk.data 集合 + 事务聚合；事务模块缺席时降级为纯原生卡片） */
    const loadBoard = async () => {
      if (!service.value || !spaceId.value) {
        boards.value = [];
        cardOps.value = [];
        bindings.value = [];
        affairCards.value = [];
        return;
      }
      const orgId = spaceId.value;
      boards.value = await service.value.loadBoards(orgId);
      if (!boards.value.some((board) => board.id === selectedBoardId.value)) {
        selectedBoardId.value = boards.value[0]?.id ?? '';
      }
      const [ops, bindingList] = await Promise.all([
        service.value.loadCardOps(orgId),
        service.value.loadBindings(orgId)
      ]);
      cardOps.value = ops;
      bindings.value = bindingList;
      viewPrefs.value = selectedBoardId.value ? await service.value.loadViewPrefs(selectedBoardId.value) : null;

      // 绑定卡片聚合（自动绑定主路径 + 手动绑定补充路径）；模块缺席 = 降级
      if (activeBoard.value && service.value.affairsAvailable) {
        try {
          affairCards.value = await service.value.listAffairCards(activeBoard.value, bindingList);
        } catch (error) {
          console.warn('[spark-kanban] 子事务聚合失败（降级为原生卡片模式）：', error);
          affairCards.value = [];
        }
      } else {
        affairCards.value = [];
      }

      // 成员侧「本地生成」指派通知（档三-12 最少事件集；失败不影响加载）
      if (activeBoard.value && currentRootId.value) {
        try {
          await service.value.notifyAssignedToMe(orgId, currentRootId.value, ops, activeBoard.value.name);
        } catch (error) {
          console.warn('[spark-kanban] 指派通知生成失败（已降级）：', error);
        }
      }
    };

    const syncLatestFromPeers = async (): Promise<boolean> => {
      if (isPersonal.value || !selectedOrgId.value) {
        return false;
      }
      const plugin = await ensureSdk();
      try {
        await plugin.runtime.syncOrganizationData(selectedOrgId.value);
        return true;
      } catch (error) {
        setMessage(`成员数据同步失败：${error}`, 'warning');
        return false;
      }
    };

    const reloadAll = async () => {
      loading.value = true;
      try {
        const plugin = await ensureSdk();
        const identity = await plugin.runtime.currentRoot();
        currentRootId.value = identity.rootId;
        await loadOrganizations();
        await loadBoard();
        // 同步后台化：先渲染本地数据，peer 同步成功后重载收敛
        void syncLatestFromPeers().then(async (synced) => {
          if (synced) {
            await loadBoard().catch(() => undefined);
          }
        });
      } catch (error) {
        setMessage(`加载失败：${error}`, 'error');
      } finally {
        loading.value = false;
      }
    };

    const onOrgChange = () => {
      selectedBoardId.value = '';
      void loadBoard().catch((error) => setMessage(`加载失败：${error}`, 'error'));
    };

    const onBoardChange = () => {
      void loadBoard().catch((error) => setMessage(`加载失败：${error}`, 'error'));
    };

    // ---------------- 看板管理 ----------------

    const submitBoard = async () => {
      boardSaving.value = true;
      try {
        await ensureSdk();
        if (!service.value || !currentRootId.value) {
          throw new Error('Plugin service unavailable');
        }
        const board = await service.value.createBoard(
          spaceId.value,
          currentRootId.value,
          { name: boardDraft.value.name, contextAffairId: boardDraft.value.contextAffairId || undefined },
          currentOrgRole.value
        );
        boardDialogVisible.value = false;
        boardDraft.value = { name: '', contextAffairId: '' };
        await loadBoard();
        selectedBoardId.value = board.id;
        await loadBoard();
        setMessage('看板已创建（默认列模板：待分诊 → 待办 → 进行中 → 待验证 → 完成）', 'success');
      } catch (error) {
        setMessage(`看板创建失败：${error}`, 'error');
      } finally {
        boardSaving.value = false;
      }
    };

    // ---------------- 原生卡片 ----------------

    const openCardDialog = () => {
      cardDraft.value = { title: '', description: '', assigneeRootId: '' };
      cardDialogVisible.value = true;
    };

    const submitCard = async () => {
      cardSaving.value = true;
      try {
        await ensureSdk();
        if (!service.value || !currentRootId.value || !activeBoard.value) {
          throw new Error('Plugin service unavailable');
        }
        await service.value.createCard(spaceId.value, currentRootId.value, activeBoard.value, cardDraft.value, currentOrgRole.value);
        cardDialogVisible.value = false;
        await loadBoard();
        setMessage('卡片已创建（落入待分诊列，操作流留痕可核验）', 'success');
      } catch (error) {
        setMessage(`建卡失败：${error}`, 'error');
      } finally {
        cardSaving.value = false;
      }
    };

    const submitAssign = async (cardId: string) => {
      assigning.value = true;
      try {
        if (!service.value || !currentRootId.value || !activeBoard.value) {
          throw new Error('Plugin service unavailable');
        }
        await service.value.assignCard(
          spaceId.value,
          currentRootId.value,
          activeBoard.value.id,
          cardId,
          assignDraft.value,
          currentOrgRole.value
        );
        await loadBoard();
        setMessage(assignDraft.value ? '已指派（对方设备同步后本机生成通知）' : '已取消指派', 'success');
      } catch (error) {
        setMessage(`指派失败：${error}`, 'error');
      } finally {
        assigning.value = false;
      }
    };

    const submitComment = async (cardId: string) => {
      commenting.value = true;
      try {
        if (!service.value || !currentRootId.value || !activeBoard.value) {
          throw new Error('Plugin service unavailable');
        }
        await service.value.commentCard(
          spaceId.value,
          currentRootId.value,
          activeBoard.value.id,
          cardId,
          commentDraft.value,
          currentOrgRole.value
        );
        commentDraft.value = '';
        await loadBoard();
      } catch (error) {
        setMessage(`评论失败：${error}`, 'error');
      } finally {
        commenting.value = false;
      }
    };

    // ---------------- 绑定 ----------------

    const submitBindAffair = async () => {
      bindSaving.value = true;
      try {
        if (!service.value || !currentRootId.value || !activeBoard.value) {
          throw new Error('Plugin service unavailable');
        }
        await service.value.bindAffair(spaceId.value, currentRootId.value, activeBoard.value.id, bindAffairId.value, currentOrgRole.value);
        bindAffairDialogVisible.value = false;
        bindAffairId.value = '';
        await loadBoard();
        setMessage('子事务已挂载（本机已关注该事务时即出现在看板上）', 'success');
      } catch (error) {
        setMessage(`挂载失败：${error}`, 'error');
      } finally {
        bindSaving.value = false;
      }
    };

    const submitBindNative = async (cardId: string) => {
      bindingNative.value = true;
      try {
        if (!service.value || !currentRootId.value || !activeBoard.value) {
          throw new Error('Plugin service unavailable');
        }
        await service.value.bindNativeCard(
          spaceId.value,
          currentRootId.value,
          activeBoard.value.id,
          cardId,
          bindNativeAffairId.value,
          bindings.value,
          currentOrgRole.value
        );
        bindNativeAffairId.value = '';
        await loadBoard();
        setMessage('已绑定子事务（绑定记录 append-only 留痕）', 'success');
      } catch (error) {
        setMessage(`绑定失败：${error}`, 'error');
      } finally {
        bindingNative.value = false;
      }
    };

    const nativeBindingOf = (cardId: string) =>
      activeBoard.value ? nativeCardBinding(activeBoard.value.id, cardId, bindings.value) : null;

    // ---------------- 拖拽（§4：即时渲染 + 后台提交；失败回滚并展示原因） ----------------

    const onCardDragStart = (card: KanbanCardView, event: DragEvent) => {
      dragCardRef.value = card.ref;
      event.dataTransfer?.setData('text/plain', card.ref);
      if (event.dataTransfer) {
        event.dataTransfer.effectAllowed = 'move';
      }
    };

    const onColumnDragOver = (columnId: string) => {
      if (dragCardRef.value) {
        dropColumnId.value = columnId;
      }
    };

    const onColumnDragLeave = (columnId: string) => {
      if (dropColumnId.value === columnId) {
        dropColumnId.value = '';
      }
    };

    /** 列内排序偏好落盘（local 视图偏好，档三-22；插入到 targetRef 之前，缺省队尾） */
    const persistCardOrder = async (columnId: string, movedRef: string, targetRef: string | null) => {
      if (!service.value || !activeBoard.value) {
        return;
      }
      const columnCards = boardView.value.find((view) => view.column.id === columnId);
      const refs = (columnCards?.cards ?? []).map((card) => card.ref).filter((ref) => ref !== movedRef);
      const insertAt = targetRef ? refs.indexOf(targetRef) : -1;
      refs.splice(insertAt >= 0 ? insertAt : refs.length, 0, movedRef);
      viewPrefs.value = await service.value.saveCardOrder(activeBoard.value.id, columnId, refs);
    };

    const onDrop = async (column: KanbanColumn, targetRef: string | null, event: DragEvent) => {
      const ref = event.dataTransfer?.getData('text/plain') || dragCardRef.value;
      dropColumnId.value = '';
      dragCardRef.value = '';
      if (!ref || !service.value || !activeBoard.value) {
        return;
      }
      const dragged = boardView.value.flatMap((view) => view.cards).find((card) => card.ref === ref);
      if (!dragged) {
        return;
      }
      // 列内落点 = 手动排序（local 偏好）；跨列 = 转列语义 + 排序偏好
      const sameColumn = dragged.columnId === column.id;
      if (!sameColumn) {
        if (dragged.kind === 'native') {
          // 原生卡片：move 操作入 append-only 流（terminal 列 = 归档，视图语义）
          if (!canEdit.value) {
            ElMessage.warning('仅组织成员可以拖动卡片');
            return;
          }
          try {
            await service.value.moveCard(
              spaceId.value,
              currentRootId.value ?? '',
              activeBoard.value.id,
              dragged.card.cardId,
              column.id,
              currentOrgRole.value
            );
          } catch (error) {
            setMessage(`转列失败：${error}`, 'error');
            await loadBoard();
            return;
          }
        } else if (column.kind === 'terminal') {
          // 终态 = 子事务决议生效（内核机制），看板只引导说明，不代为发起决议
          terminalDialogVisible.value = true;
          return;
        } else if (column.kind === 'stage') {
          // 绑定卡片拖入中间态列：拖动转列 = 向子事务提交签名状态操作（档三-1）
          const statusKey = column.statusKey;
          if (!statusKey) {
            setMessage(`列「${column.title}」未声明列—状态映射（statusKey），无法提交状态操作`, 'warning');
            return;
          }
          try {
            const result = await service.value.submitStatusOp(dragged.card.affairId, statusKey);
            if (result.status !== 'accepted') {
              setMessage(`状态操作已提交但判定为 ${result.status}（未知指向暂存），列位置以事务日志收敛为准`, 'warning');
            }
          } catch (error) {
            // 权限不足/规则拒绝：回滚（重载）并如实展示原因
            setMessage(`转列被拒绝：${(error as Error).message}`, 'error');
            await loadBoard();
            return;
          }
        }
        // 绑定卡片拖回 triage 列：不合成状态操作（待分诊 = 无状态操作的默认落点，
        // 语义上「退回分诊」不产生新事实），仅记列内排序偏好（local 视图偏好）
      }
      try {
        await persistCardOrder(column.id, ref, targetRef !== ref ? targetRef : null);
      } catch (error) {
        console.warn('[spark-kanban] 列内排序偏好保存失败（local 降级）：', error);
      }
      await loadBoard();
    };

    // ---------------- 卡片详情 ----------------

    const openCardDetail = (card: KanbanCardView) => {
      detailCard.value = card;
      commentDraft.value = '';
      assignDraft.value = card.kind === 'native' ? card.card.assigneeRootId ?? '' : '';
      bindNativeAffairId.value = '';
      detailVisible.value = true;
    };

    // ---------------- 卡片回调（card-notify「查看卡片」路由） ----------------

    const handleCardAction = async (action: PluginCardActionPayload) => {
      if (action.actionId !== 'goto-card') {
        return;
      }
      const data = action.data as { cardId?: string; boardId?: string; orgId?: string } | undefined;
      if (!data?.cardId || !data?.boardId) {
        return;
      }
      if (!isPersonal.value && data.orgId && data.orgId !== selectedOrgId.value) {
        if (!orgOptions.value.some((org) => org.orgId === data.orgId)) {
          setMessage('目标卡片所属组织不在本机已加入的组织中。', 'warning');
          return;
        }
        selectedOrgId.value = data.orgId;
      }
      selectedBoardId.value = data.boardId;
      await loadBoard().catch(() => undefined);
      highlightedCardRef.value = `native:${data.cardId}`;
      if (highlightTimer) {
        clearTimeout(highlightTimer);
      }
      highlightTimer = setTimeout(() => {
        highlightedCardRef.value = '';
      }, HIGHLIGHT_DURATION_MS);
    };

    // ---------------- 展示辅助 ----------------

    const columnKindLabel = (kind: KanbanColumn['kind']): string =>
      kind === 'triage' ? '待分诊' : kind === 'stage' ? '阶段' : '终态';

    const columnKindTagType = (kind: KanbanColumn['kind']): 'warning' | 'primary' | 'success' =>
      kind === 'triage' ? 'warning' : kind === 'stage' ? 'primary' : 'success';

    const affairTypeLabel = (type: string): string => {
      // 档三-4：未预设类型显示为通用子事务
      const labels: Record<string, string> = { bug: 'bug', proposal: '建议', pr: 'PR', task: '任务' };
      return labels[type] ?? '子事务';
    };

    const resolutionLabel = (badge: KanbanResolutionBadge): string => RESOLUTION_BADGE_LABELS[badge];

    const columnTitle = (columnId: string): string =>
      activeBoard.value?.columns.find((column) => column.id === columnId)?.title ?? columnId;

    const formatDate = (timestamp: number) =>
      new Intl.DateTimeFormat('zh-CN', {
        year: 'numeric',
        month: '2-digit',
        day: '2-digit',
        hour: '2-digit',
        minute: '2-digit'
      }).format(new Date(timestamp));

    onMounted(() => {
      void (async () => {
        const plugin = await ensureSdk();
        offCardAction =
          plugin.messages?.onCardAction((action) => {
            void handleCardAction(action);
          }) ?? null;
        await reloadAll();
        // 远端合入本插件集合 / 事务副本变更 → 重载收敛（变更通知非可靠队列，重读为准）
        await service.value?.subscribeDataChanges(() => void loadBoard().catch(() => undefined));
        if (service.value?.affairsAvailable) {
          try {
            await service.value.subscribeAffairChanges(() => void loadBoard().catch(() => undefined));
          } catch (error) {
            console.warn('[spark-kanban] 事务变更订阅失败（降级为手动刷新）：', error);
          }
        }
      })();
    });

    onUnmounted(() => {
      offCardAction?.();
      if (highlightTimer) {
        clearTimeout(highlightTimer);
      }
    });

    watch(
      () => props.pluginContext?.orgId,
      (orgId) => {
        if (!orgId || isPersonal.value || selectedOrgId.value === orgId) {
          return;
        }
        if (orgOptions.value.some((org) => org.orgId === orgId)) {
          selectedOrgId.value = orgId;
          onOrgChange();
        }
      }
    );

    return {
      loading,
      message,
      messageType,
      service,
      isPersonal,
      orgOptions,
      selectedOrgId,
      spaceReady,
      activeOrgMembers,
      canManage,
      canEdit,
      isAdmin: computed(() => currentOrgRole.value === 'admin'),
      boards,
      selectedBoardId,
      activeBoard,
      boardView,
      dropColumnId,
      highlightedCardRef,
      boardDialogVisible,
      boardSaving,
      boardDraft,
      cardDialogVisible,
      cardSaving,
      cardDraft,
      bindAffairDialogVisible,
      bindSaving,
      bindAffairId,
      terminalDialogVisible,
      detailVisible,
      detailCard,
      detailTitle,
      detailComments,
      commentDraft,
      commenting,
      assignDraft,
      assigning,
      bindNativeAffairId,
      bindingNative,
      reloadAll,
      onOrgChange,
      onBoardChange,
      submitBoard,
      openCardDialog,
      submitCard,
      submitAssign,
      submitComment,
      submitBindAffair,
      submitBindNative,
      nativeBindingOf,
      onCardDragStart,
      onColumnDragOver,
      onColumnDragLeave,
      onDrop,
      openCardDetail,
      columnKindLabel,
      columnKindTagType,
      affairTypeLabel,
      resolutionLabel,
      columnTitle,
      formatDate
    };
  }
});
</script>

<style scoped>
.spark-kanban {
  display: grid;
  gap: 14px;
}

.header-card {
  border-radius: 12px;
}

.message {
  margin-bottom: 2px;
}

.header-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
}

.header-row-inner {
  display: flex;
  align-items: center;
  gap: 10px;
  min-width: 0;
}

.eyebrow {
  margin: 0 0 6px;
  color: #0f766e;
  font-size: 12px;
  font-weight: 700;
  letter-spacing: 0.08em;
  text-transform: uppercase;
}

h2,
h3 {
  margin: 0;
}

.lede {
  margin: 8px 0 0;
  color: #64748b;
}

.selectors {
  margin-top: 12px;
}

.meta-row {
  display: flex;
  flex-wrap: wrap;
  gap: 10px;
  margin-top: 10px;
}

.board-row {
  display: flex;
  gap: 10px;
  margin-top: 12px;
}

.board-row .el-select {
  min-width: 240px;
}

.board-toolbar {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
}

.board-columns {
  display: flex;
  gap: 12px;
  overflow-x: auto;
  align-items: flex-start;
  padding-bottom: 8px;
}

.board-column {
  flex: 0 0 260px;
  background: #f8fafc;
  border: 1px solid var(--el-border-color);
  border-radius: 10px;
  padding: 10px;
  min-height: 160px;
  transition: border-color 0.2s, box-shadow 0.2s;
}

.board-column.drop-active {
  border-color: #0f766e;
  box-shadow: 0 0 0 3px rgba(15, 118, 110, 0.18);
}

.board-column.kind-triage {
  background: #fffbeb;
}

.board-column.kind-terminal {
  background: #f0fdf4;
}

.column-header {
  display: flex;
  align-items: center;
  gap: 6px;
  margin-bottom: 10px;
  flex-wrap: wrap;
}

.column-title {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.column-count {
  color: #64748b;
  font-size: 12px;
}

.kanban-card {
  background: #fff;
  border: 1px solid var(--el-border-color);
  border-radius: 8px;
  padding: 10px;
  margin-bottom: 8px;
  cursor: grab;
  transition: border-color 0.2s, box-shadow 0.2s;
}

.kanban-card:hover {
  border-color: #0f766e;
}

.kanban-card.highlighted {
  border-color: #0f766e;
  box-shadow: 0 0 0 3px rgba(15, 118, 110, 0.18);
}

.kanban-card.affair-card {
  border-left: 3px solid #d97706;
}

.card-title-row {
  display: flex;
  align-items: center;
  gap: 6px;
  flex-wrap: wrap;
}

.signed-mark {
  color: #16a34a;
  font-size: 12px;
}

.card-title {
  margin: 6px 0;
  font-weight: 600;
  word-break: break-word;
}

.card-meta {
  display: flex;
  gap: 10px;
  color: #64748b;
  font-size: 12px;
}

.assignee {
  color: #0f766e;
}

.detail-tags {
  display: flex;
  gap: 8px;
  flex-wrap: wrap;
  margin-bottom: 8px;
}

.detail-content {
  white-space: pre-wrap;
  word-break: break-word;
  margin: 10px 0;
}

.post-meta {
  display: flex;
  justify-content: space-between;
  gap: 10px;
  color: #64748b;
  font-size: 12px;
  word-break: break-all;
}

.assign-row,
.bind-row {
  display: flex;
  gap: 8px;
  margin-top: 10px;
}

.comment-item {
  border-left: 2px solid #d1fae5;
  background: #f8fafc;
  padding: 8px;
  margin-bottom: 8px;
}

.comment-content {
  margin: 6px 0;
  white-space: pre-wrap;
  word-break: break-word;
}

.sig-tag {
  margin-right: 4px;
}

.reply-editor {
  display: flex;
  gap: 8px;
  align-items: flex-start;
  margin-top: 12px;
}

.hint {
  color: #64748b;
  margin: 8px 0 0;
  font-size: 12px;
}

.pending-hint {
  color: #b45309;
}
</style>

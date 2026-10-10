<!--
  论坛插件（spark-forum）· 主视图（app 视图）：板块列表 → 板块页 → 主题详情
  三层导航（桌面三栏语义收敛为单栏逐层递进，移动端同源可用）。

  沿用 spark-example ExampleView 的头部模式与机制：
  - runtime.currentRoot / listMineOrganizations / syncOrganizationData
    （org:read、org:sync）；
  - docs 读写全部经 service 层（storage:read/write）；
  - identity:sign 发帖防抵赖 + identity.verify 免权限验签（「已签名」徽标）；
  - messages.sendAppMessage 新主题通知（message:app，仅发帖者本机即时反馈）；
    成员侧通知在同步后由本机插件实例「本地生成」（§20.4.3，档三-12：回复
    不通知、仅新主题通知）；
  - messages.onCardAction 接收主题卡片「查看/去回复」回调 → 切组织、定位主题；
  - 删除诉求按档三-14：「关闭 + 隐藏」事件 + 提交时如实告知不可收回。
-->
<template>
  <section class="spark-forum">
    <el-alert
      v-if="message"
      :title="message"
      :type="messageType"
      :closable="false"
      show-icon
      class="message"
    />

    <el-card shadow="never" class="header-card">
      <div class="header-row">
        <div>
          <p class="eyebrow">论坛</p>
          <h2>组织主题讨论区</h2>
          <p class="lede">板块、主题帖与楼中楼回复；签名防抵赖、链式存证，面向开发讨论与方案沉淀。</p>
        </div>
        <el-button @click="reloadAll" :loading="loading">刷新</el-button>
      </div>

      <el-form label-position="top" class="selectors" v-if="orgOptions.length > 0">
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

      <el-empty v-if="orgOptions.length === 0" description="你还没有加入任何组织。" />

      <div v-if="activeOrg" class="meta-row">
        <el-tag type="info" class="root-id-tag">
          <span class="root-id-text">当前 RootID: {{ currentRootId || '-' }}</span>
        </el-tag>
        <el-tag :type="isAdmin ? 'danger' : 'warning'">
          {{ isAdmin ? '组织管理员' : '组织成员' }}
        </el-tag>
      </div>
    </el-card>

    <!-- 板块列表页 -->
    <el-card v-if="activeOrg && view === 'boards'" shadow="never">
      <template #header>
        <div class="header-row">
          <h3>板块</h3>
          <el-button v-if="isAdmin" type="primary" size="small" @click="openBoardDialog()">新建板块</el-button>
        </div>
      </template>

      <el-empty v-if="visibleBoards.length === 0" description="暂无板块" />

      <div
        v-for="board in visibleBoards"
        :key="board.id"
        class="board-item"
        :class="{ archived: board.archived }"
        @click="enterBoard(board.id)"
      >
        <div class="board-main">
          <strong>{{ board.name }}</strong>
          <el-tag v-if="board.archived" size="small" type="info">已归档</el-tag>
          <p class="board-intro">{{ board.intro }}</p>
        </div>
        <div class="board-stats">
          <span>主题 {{ topicCountByBoard(board.id) }}</span>
          <span>最近活跃 {{ lastActiveText(board.id) }}</span>
        </div>
        <div v-if="isAdmin" class="board-actions" @click.stop>
          <el-button size="small" text @click="openBoardDialog(board)">编辑</el-button>
          <el-button size="small" text :type="board.archived ? 'success' : 'warning'" @click="toggleArchiveBoard(board)">
            {{ board.archived ? '取消归档' : '归档' }}
          </el-button>
        </div>
      </div>
    </el-card>

    <!-- 板块页：主题列表 -->
    <el-card v-if="activeOrg && view === 'board' && activeBoard" shadow="never">
      <template #header>
        <div class="header-row">
          <div class="header-row-inner">
            <el-button size="small" text @click="view = 'boards'">← 板块</el-button>
            <h3>{{ activeBoard.name }}</h3>
            <el-tag v-if="activeBoard.archived" size="small" type="info">已归档</el-tag>
          </div>
          <el-button
            v-if="!activeBoard.archived"
            type="primary"
            size="small"
            :disabled="!canPost"
            @click="openTopicDialog()"
          >
            发主题
          </el-button>
        </div>
      </template>

      <el-empty v-if="topicList.length === 0" description="暂无主题，来发第一帖" />

      <p v-if="topicListTruncated" class="truncation-hint">
        仅显示最近 {{ FORUM_QUERY_LIMITS.topics }} 条主题（更早的主题仍在链上，后续版本将提供分页/检索）
      </p>

      <div
        v-for="item in topicList"
        :key="item.topic.id"
        :id="`topic-${item.topic.id}`"
        class="topic-item"
        :class="{ highlighted: highlightedTopicId === item.topic.id }"
        @click="enterTopic(item.topic.id)"
      >
        <div class="topic-title-row">
          <el-tag v-if="item.state.pinned" size="small" type="danger">置顶</el-tag>
          <el-tag v-if="item.state.featured" size="small" type="warning">精华</el-tag>
          <el-tag v-if="item.state.resolved" size="small" type="success">已解决</el-tag>
          <el-tag v-if="item.state.closed" size="small" type="info">已关闭</el-tag>
          <strong class="topic-title">{{ item.topic.title }}</strong>
        </div>
        <p class="topic-preview">{{ topicPreview(item.topic.content) }}</p>
        <div class="post-meta">
          <span class="author">{{ item.topic.authorRootId }}</span>
          <span>回复 {{ item.replyCount }} · {{ formatDate(item.lastActiveAt) }}</span>
        </div>
      </div>
    </el-card>

    <!-- 主题详情页 -->
    <el-card
      v-if="activeOrg && view === 'topic' && activeTopic"
      shadow="never"
      :id="`topic-detail-${activeTopic.id}`"
      :class="{ 'detail-highlighted': highlightedTopicId === activeTopic.id }"
    >
      <template #header>
        <div class="header-row">
          <div class="header-row-inner">
            <el-button size="small" text @click="view = 'board'">← {{ activeBoard?.name ?? '板块' }}</el-button>
            <h3 class="topic-detail-title">{{ activeTopic.title }}</h3>
          </div>
        </div>
      </template>

      <div class="topic-state-row">
        <el-tag v-if="activeTopicState.pinned" size="small" type="danger">置顶</el-tag>
        <el-tag v-if="activeTopicState.featured" size="small" type="warning">精华</el-tag>
        <el-tag v-if="activeTopicState.resolved" size="small" type="success">已解决</el-tag>
        <el-tag v-if="activeTopicState.closed" size="small" type="info">已关闭</el-tag>
        <el-tag v-if="activeTopic.signature" type="success" size="small">已签名</el-tag>
        <el-button
          v-if="activeTopic.signature"
          size="small"
          text
          :loading="verifying"
          @click="verifyActiveTopic"
        >
          验签
        </el-button>
        <span v-if="verifyResult" class="verify-result">{{ verifyResult }}</span>
      </div>

      <div class="post-meta">
        <span class="author">{{ activeTopic.authorRootId }}</span>
        <span>{{ formatDate(activeTopic.createdAt) }}<template v-if="activeTopic.supersedesId">（已编辑）</template></span>
      </div>

      <p class="topic-content">
        <template v-for="(segment, index) in activeTopicSegments" :key="index">
          <span v-if="segment.type === 'affair-ref'" class="affair-ref" title="议题引用（插件间契约未实现，展示级）">{{ segment.text }}</span>
          <template v-else>{{ segment.text }}</template>
        </template>
      </p>

      <div class="topic-ops">
        <el-button v-if="canEditActiveTopic" size="small" @click="openTopicDialog(activeTopic)">编辑（发新版本）</el-button>
        <el-button v-if="versionHistory.length > 1" size="small" text @click="historyVisible = !historyVisible">
          历史版本（{{ versionHistory.length }}）
        </el-button>
      </div>

      <template v-if="historyVisible">
        <div v-for="version in versionHistory" :key="version.id" class="history-item">
          <div class="post-meta">
            <span>{{ version.id === activeTopic.id ? '当前版本' : '历史版本' }}</span>
            <span>{{ formatDate(version.createdAt) }}</span>
          </div>
          <p class="topic-content small">{{ version.content }}</p>
        </div>
      </template>

      <!-- 管理员治理操作区：全部经事件流留痕，理由入事件 -->
      <div v-if="isAdmin" class="moderation">
        <el-divider content-position="left">治理操作（留痕可审计）</el-divider>
        <div class="moderation-actions">
          <el-button size="small" @click="openEventDialog(activeTopicState.pinned ? 'unpin' : 'pin')">
            {{ activeTopicState.pinned ? '取消置顶' : '置顶' }}
          </el-button>
          <el-button size="small" @click="openEventDialog(activeTopicState.featured ? 'unfeature' : 'feature')">
            {{ activeTopicState.featured ? '取消精华' : '加精' }}
          </el-button>
          <el-button v-if="!activeTopicState.resolved && !activeTopicState.closed" size="small" type="success" @click="openEventDialog('resolve')">
            标记已解决
          </el-button>
          <el-button v-if="!activeTopicState.closed" size="small" type="warning" @click="openEventDialog('close')">
            关闭主题
          </el-button>
          <el-button v-else size="small" @click="openEventDialog('reopen')">重新打开</el-button>
          <el-button v-if="!activeTopicState.hidden" size="small" type="danger" @click="openEventDialog('hide')">
            隐藏（删除诉求）
          </el-button>
          <el-button v-else size="small" text @click="openEventDialog('unhide')">取消隐藏</el-button>
        </div>
      </div>

      <!-- 楼层回复（两级楼中楼） -->
      <el-divider content-position="left">回复（{{ replyCountActive }}）</el-divider>

      <el-empty v-if="replyThread.length === 0" description="暂无回复" />

      <div class="comment-list">
        <div v-for="node in replyThread" :key="node.reply.id" class="comment-item">
          <div class="post-meta">
            <span class="author">{{ node.reply.authorRootId }}</span>
            <span>{{ formatDate(node.reply.createdAt) }}</span>
          </div>
          <p class="comment-content">
            <el-tag v-if="node.reply.signature" type="success" size="small" class="sig-tag">已签名</el-tag>
            {{ node.reply.content }}
          </p>
          <div v-if="!activeTopicState.closed" class="reply-editor small">
            <el-input
              v-model="replyDraftByReply[node.reply.id]"
              :maxlength="5000"
              placeholder="回复该楼层"
            />
            <el-button
              size="small"
              :loading="replyingTo === node.reply.id"
              @click="submitReply(node.reply.id)"
            >
              回复
            </el-button>
          </div>

          <div v-for="child in node.replies" :key="child.id" class="comment-item nested">
            <div class="post-meta">
              <span class="author">{{ child.authorRootId }}</span>
              <span>{{ formatDate(child.createdAt) }}</span>
            </div>
            <p class="comment-content">
              <el-tag v-if="child.signature" type="success" size="small" class="sig-tag">已签名</el-tag>
              <span class="reply-flag">回复：</span>{{ child.content }}
            </p>
          </div>
        </div>
      </div>

      <div v-if="!activeTopicState.closed" class="reply-editor">
        <el-input
          v-model="replyDraft"
          type="textarea"
          :rows="3"
          :maxlength="5000"
          show-word-limit
          placeholder="写下你的回复（最多 5000 字）"
        />
        <el-button type="primary" :loading="replying" :disabled="!canPost" @click="submitReply()">回复</el-button>
      </div>
      <p v-else class="hint">主题已关闭，不能再回复。</p>
    </el-card>

    <!-- 新建/编辑板块对话框 -->
    <el-dialog v-model="boardDialogVisible" :title="editingBoard ? '编辑板块' : '新建板块'" width="420px">
      <el-form label-position="top">
        <el-form-item label="板块名称">
          <el-input v-model="boardDraft.name" :maxlength="40" show-word-limit placeholder="如：内核 / SDK / 发布 / 产品" />
        </el-form-item>
        <el-form-item label="板块简介">
          <el-input v-model="boardDraft.intro" type="textarea" :rows="2" :maxlength="200" show-word-limit />
        </el-form-item>
        <el-form-item label="排序权重（越小越靠前）">
          <el-input-number v-model="boardDraft.sort" :min="0" :max="9999" />
        </el-form-item>
      </el-form>
      <template #footer>
        <el-button @click="boardDialogVisible = false">取消</el-button>
        <el-button type="primary" :loading="boardSaving" @click="submitBoard">保存</el-button>
      </template>
    </el-dialog>

    <!-- 发主题/编辑主题对话框 -->
    <el-dialog v-model="topicDialogVisible" :title="editingTopic ? '编辑主题（发新版本）' : '发主题'" width="560px">
      <el-form label-position="top">
        <el-form-item label="标题">
          <el-input v-model="topicDraft.title" :maxlength="120" show-word-limit placeholder="一句话说清讨论主题" />
        </el-form-item>
        <el-form-item label="正文（纯文本；可粘贴 affair:xxx 引用议题）">
          <el-input v-model="topicDraft.content" type="textarea" :rows="8" :maxlength="20000" show-word-limit />
        </el-form-item>
      </el-form>
      <p class="hint">
        论坛数据仅追加、不覆盖：{{ editingTopic ? '编辑会产生一个指向旧版的新版本，历史版本仍留链可审计。' : '发布后不可真正删除，只能经治理事件关闭/隐藏。' }}
        发布将请求一次域身份签名（防抵赖），拒绝签名也会照发（少「已签名」徽标）。
      </p>
      <template #footer>
        <el-button @click="topicDialogVisible = false">取消</el-button>
        <el-button type="primary" :loading="topicSaving" @click="submitTopic">
          {{ editingTopic ? '发布新版本' : '发布主题' }}
        </el-button>
      </template>
    </el-dialog>

    <!-- 治理事件对话框 -->
    <el-dialog v-model="eventDialogVisible" :title="`治理操作：${eventKindLabel(pendingEventKind)}`" width="420px">
      <el-alert
        v-if="pendingEventKind === 'hide' || pendingEventKind === 'close'"
        type="warning"
        :closable="false"
        show-icon
        title="该操作以事件形式永久留痕、不可收回：数据仅追加不删除，其他成员可通过事件流与存证核验。"
      />
      <el-form label-position="top" class="event-form">
        <el-form-item label="理由（可选，随事件留痕）">
          <el-input v-model="eventReason" type="textarea" :rows="2" :maxlength="200" show-word-limit />
        </el-form-item>
      </el-form>
      <template #footer>
        <el-button @click="eventDialogVisible = false">取消</el-button>
        <el-button type="primary" :loading="eventSaving" @click="submitEvent">确认提交</el-button>
      </template>
    </el-dialog>
  </section>
</template>

<script lang="ts">
import { computed, defineComponent, nextTick, onMounted, onUnmounted, ref, watch } from 'vue';
import { ElMessage } from 'element-plus';
import { ensurePluginSDK } from '../../packages/plugin-sdk/src';
import type { PluginCardActionPayload, PluginSDK } from '../../packages/plugin-sdk/src';
import {
  buildReplyThread,
  buildTopicList,
  canEditTopic,
  canManageBoards,
  canPostTopic,
  deriveTopicState,
  resolveLatestTopics,
  splitAffairRefs,
  topicVersionHistory,
  validateBoardInput,
  validateReplyContent,
  validateTopicContent,
  validateTopicTitle,
  DEFAULT_TOPIC_STATE,
  FORUM_QUERY_LIMITS,
  type ForumBoard,
  type ForumReply,
  type ForumReplyNode,
  type ForumTopic,
  type ForumTopicEvent,
  type ForumTopicEventKind,
  type ForumTopicListItem,
  type ForumTopicState
} from './model';
import { ForumService, type ForumOrgConfig } from './service';

type OrganizationView = {
  orgId: string;
  name: string;
  description: string;
  members: Array<{
    rootId: string;
    role: 'admin' | 'member';
    nodeInfo?: {
      peerId?: string;
      addresses: string[];
    };
  }>;
};

/** 卡片回调后高亮时长（ms）：足够用户注意到定位目标，又不永久占用视觉焦点 */
const HIGHLIGHT_DURATION_MS = 2500;

export default defineComponent({
  name: 'ForumView',
  props: {
    pluginContext: {
      type: Object as () => { orgId?: string } | undefined,
      required: false,
      default: undefined
    }
  },
  setup(props) {
    const sdk = ref<PluginSDK | null>(null);
    const service = ref<ForumService | null>(null);
    const loading = ref(false);
    const message = ref('');
    const messageType = ref<'info' | 'success' | 'warning' | 'error'>('info');

    const currentRootId = ref<string | null>(null);
    const orgOptions = ref<OrganizationView[]>([]);
    const selectedOrgId = ref('');
    const orgConfig = ref<ForumOrgConfig | null>(null);

    // 三层导航：boards（板块列表）→ board（主题列表）→ topic（主题详情）
    const view = ref<'boards' | 'board' | 'topic'>('boards');
    const selectedBoardId = ref('');
    const selectedTopicId = ref('');

    const boards = ref<ForumBoard[]>([]);
    const topics = ref<ForumTopic[]>([]);
    const replies = ref<ForumReply[]>([]);
    const topicEvents = ref<ForumTopicEvent[]>([]);

    // 板块对话框
    const boardDialogVisible = ref(false);
    const boardSaving = ref(false);
    const editingBoard = ref<ForumBoard | null>(null);
    const boardDraft = ref({ name: '', intro: '', sort: 100 });

    // 主题对话框
    const topicDialogVisible = ref(false);
    const topicSaving = ref(false);
    const editingTopic = ref<ForumTopic | null>(null);
    const topicDraft = ref({ title: '', content: '' });

    // 治理事件对话框
    const eventDialogVisible = ref(false);
    const eventSaving = ref(false);
    const pendingEventKind = ref<ForumTopicEventKind>('pin');
    const eventReason = ref('');

    // 回复
    const replyDraft = ref('');
    const replyDraftByReply = ref<Record<string, string>>({});
    const replying = ref(false);
    const replyingTo = ref('');

    // 验签
    const verifying = ref(false);
    const verifyResult = ref('');

    // 历史版本
    const historyVisible = ref(false);

    const highlightedTopicId = ref('');

    let highlightTimer: ReturnType<typeof setTimeout> | null = null;
    let offCardAction: (() => void) | null = null;

    const activeOrg = computed(() => orgOptions.value.find((org) => org.orgId === selectedOrgId.value) ?? null);
    const currentOrgRole = computed<'admin' | 'member' | null>(() => {
      if (!activeOrg.value || !currentRootId.value) {
        return null;
      }
      return activeOrg.value.members.find((member) => member.rootId === currentRootId.value)?.role ?? null;
    });
    const isAdmin = computed(() => canManageBoards(currentOrgRole.value));
    const canPost = computed(() => canPostTopic(currentOrgRole.value));

    /** 名册管理员集合：事件流状态派生的读侧鉴权依据（非管理员治理事件忽略） */
    const adminRootIds = computed<ReadonlySet<string>>(() => {
      const ids = (activeOrg.value?.members ?? [])
        .filter((member) => member.role === 'admin')
        .map((member) => member.rootId);
      return new Set(ids);
    });

    const visibleBoards = computed(() => {
      const visible = boards.value.filter((board) => !board.archived);
      // 已归档板块沉底展示（管理员可见，成员也可见但不可发帖）
      return [...visible, ...boards.value.filter((board) => board.archived)];
    });

    const activeBoard = computed(() => boards.value.find((board) => board.id === selectedBoardId.value) ?? null);

    const latestTopics = computed(() => resolveLatestTopics(topics.value));

    // 查询上限静默截断提示：topics 拉取达上限即视为「可能还有更多」，如实标注
    const topicListTruncated = computed(() => topics.value.length >= FORUM_QUERY_LIMITS.topics);

    const topicList = computed<ForumTopicListItem[]>(() => {
      if (!selectedBoardId.value) {
        return [];
      }
      return buildTopicList(selectedBoardId.value, topics.value, topicEvents.value, replies.value, adminRootIds.value);
    });

    const activeTopic = computed(() => latestTopics.value.find((topic) => topic.id === selectedTopicId.value) ?? null);

    const activeTopicState = computed<ForumTopicState>(() => {
      if (!activeTopic.value) {
        return { ...DEFAULT_TOPIC_STATE };
      }
      return deriveTopicState(activeTopic.value.id, topicEvents.value, adminRootIds.value);
    });

    const activeTopicSegments = computed(() => splitAffairRefs(activeTopic.value?.content ?? ''));

    const canEditActiveTopic = computed(() => {
      if (!activeTopic.value || activeTopicState.value.closed) {
        return false;
      }
      return canEditTopic(activeTopic.value, currentRootId.value);
    });

    const versionHistory = computed<ForumTopic[]>(() => {
      if (!activeTopic.value) {
        return [];
      }
      return topicVersionHistory(activeTopic.value, topics.value);
    });

    const replyThread = computed<ForumReplyNode[]>(() => {
      if (!activeTopic.value) {
        return [];
      }
      return buildReplyThread(activeTopic.value.id, replies.value);
    });

    const replyCountActive = computed(() => {
      if (!activeTopic.value) {
        return 0;
      }
      return replies.value.filter((reply) => reply.topicId === activeTopic.value!.id).length;
    });

    const topicCountByBoard = (boardId: string): number => {
      return latestTopics.value.filter((topic) => topic.boardId === boardId).length;
    };

    const lastActiveText = (boardId: string): string => {
      const boardTopics = latestTopics.value.filter((topic) => topic.boardId === boardId);
      const lastTopicAt = boardTopics.reduce((max, topic) => Math.max(max, topic.createdAt), 0);
      const topicIds = new Set(boardTopics.map((topic) => topic.id));
      const lastReplyAt = replies.value.reduce(
        (max, reply) => (topicIds.has(reply.topicId) ? Math.max(max, reply.createdAt) : max),
        0
      );
      const last = Math.max(lastTopicAt, lastReplyAt);
      return last > 0 ? formatDate(last) : '-';
    };

    const setMessage = (text: string, type: 'info' | 'success' | 'warning' | 'error' = 'info') => {
      message.value = text;
      messageType.value = type;
    };

    const ensureSdk = async () => {
      if (!sdk.value) {
        // SDK 由插件入口在桥握手完成时注入 window.__sparkPluginSDK，
        // 视图挂载可能先于握手完成，挂起等待注入
        sdk.value = await ensurePluginSDK();
        service.value = new ForumService(sdk.value);
      }
      return sdk.value;
    };

    const loadOrganizations = async () => {
      const plugin = await ensureSdk();
      const all = await plugin.runtime.listMineOrganizations();

      // 组织与插件无绑定：全部已加入组织皆可选
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

    const loadForum = async () => {
      await ensureSdk();
      if (!service.value) {
        throw new Error('Plugin service unavailable');
      }
      if (!selectedOrgId.value) {
        boards.value = [];
        topics.value = [];
        replies.value = [];
        topicEvents.value = [];
        orgConfig.value = null;
        return;
      }
      if (!currentRootId.value) {
        throw new Error('Root identity is locked');
      }

      const orgId = selectedOrgId.value;
      const config = await service.value.ensureOrgConfig(orgId, currentRootId.value);
      // 首次进入播种默认板块（幂等，固定 id 收敛并发播种）；播种后回写标记
      if (!config.boardsSeeded) {
        await service.value.seedDefaultBoard(orgId, config.superAdminRootId);
        orgConfig.value = await service.value.markBoardsSeeded(config);
      } else {
        orgConfig.value = config;
      }

      const [boardList, topicListRaw, replyList, eventList] = await Promise.all([
        service.value.loadBoards(orgId),
        service.value.loadTopics(orgId),
        service.value.loadReplies(orgId),
        service.value.loadTopicEvents(orgId)
      ]);
      boards.value = boardList;
      topics.value = topicListRaw;
      replies.value = replyList;
      topicEvents.value = eventList;

      // 成员侧「本地生成」通知（服务号模型 §20.4.3，档三-12：仅新主题通知）。
      // 通知失败不影响加载，仅降级少一条本机通知。
      try {
        const boardNameByBoardId = new Map(boardList.map((board) => [board.id, board.name]));
        await service.value.notifyNewTopics(orgId, topicListRaw, boardNameByBoardId);
      } catch (error) {
        console.warn('[spark-forum] 成员侧本地生成通知失败（已降级）：', error);
      }
    };

    const syncLatestFromPeers = async (): Promise<boolean> => {
      if (!selectedOrgId.value) {
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
        // 同步后台化，避免不可达 peer 阻塞首屏：先渲染本地数据，peer 同步
        // 在后台进行；同步成功后再走 loadForum 刷新（含本地通知链路）。
        await loadForum();
        void syncLatestFromPeers().then(async (synced) => {
          if (synced) {
            await loadForum().catch(() => undefined);
          }
        });
      } catch (error) {
        setMessage(`加载失败：${error}`, 'error');
      } finally {
        loading.value = false;
      }
    };

    const onOrgChange = () => {
      view.value = 'boards';
      selectedBoardId.value = '';
      selectedTopicId.value = '';
      void loadForum().catch((error) => setMessage(`加载失败：${error}`, 'error'));
    };

    const enterBoard = (boardId: string) => {
      selectedBoardId.value = boardId;
      view.value = 'board';
    };

    const enterTopic = (topicId: string) => {
      selectedTopicId.value = topicId;
      verifyResult.value = '';
      historyVisible.value = false;
      view.value = 'topic';
    };

    // ---------------- 板块管理 ----------------

    const openBoardDialog = (board?: ForumBoard) => {
      editingBoard.value = board ?? null;
      boardDraft.value = board
        ? { name: board.name, intro: board.intro, sort: board.sort }
        : { name: '', intro: '', sort: 100 };
      boardDialogVisible.value = true;
    };

    const submitBoard = async () => {
      const validation = validateBoardInput(boardDraft.value.name, boardDraft.value.intro);
      if (!validation.ok) {
        ElMessage.warning(validation.reason || '板块信息不合法');
        return;
      }
      boardSaving.value = true;
      try {
        await ensureSdk();
        if (!service.value || !selectedOrgId.value || !currentRootId.value) {
          throw new Error('Plugin service unavailable');
        }
        if (editingBoard.value) {
          await service.value.updateBoard(editingBoard.value, boardDraft.value, currentOrgRole.value);
        } else {
          await service.value.createBoard(selectedOrgId.value, currentRootId.value, boardDraft.value, currentOrgRole.value);
        }
        boardDialogVisible.value = false;
        await loadForum();
        setMessage(editingBoard.value ? '板块已更新' : '板块已创建', 'success');
      } catch (error) {
        setMessage(`板块保存失败：${error}`, 'error');
      } finally {
        boardSaving.value = false;
      }
    };

    const toggleArchiveBoard = async (board: ForumBoard) => {
      try {
        await ensureSdk();
        if (!service.value) {
          throw new Error('Plugin service unavailable');
        }
        await service.value.archiveBoard(board, !board.archived, currentOrgRole.value);
        await loadForum();
        setMessage(board.archived ? '板块已取消归档' : '板块已归档（列表沉底、不可发新主题）', 'success');
      } catch (error) {
        setMessage(`归档操作失败：${error}`, 'error');
      }
    };

    // ---------------- 主题发布/编辑 ----------------

    const openTopicDialog = (topic?: ForumTopic) => {
      editingTopic.value = topic ?? null;
      topicDraft.value = topic ? { title: topic.title, content: topic.content } : { title: '', content: '' };
      topicDialogVisible.value = true;
    };

    const submitTopic = async () => {
      const titleCheck = validateTopicTitle(topicDraft.value.title);
      if (!titleCheck.ok) {
        ElMessage.warning(titleCheck.reason || '标题不合法');
        return;
      }
      const contentCheck = validateTopicContent(topicDraft.value.content);
      if (!contentCheck.ok) {
        ElMessage.warning(contentCheck.reason || '正文不合法');
        return;
      }

      topicSaving.value = true;
      try {
        await ensureSdk();
        if (!service.value || !selectedOrgId.value || !currentRootId.value || !selectedBoardId.value) {
          throw new Error('Plugin service unavailable');
        }
        const topic = await service.value.createTopic(
          selectedOrgId.value,
          currentRootId.value,
          {
            boardId: selectedBoardId.value,
            title: topicDraft.value.title,
            content: topicDraft.value.content,
            supersedes: editingTopic.value ?? undefined
          },
          currentOrgRole.value
        );
        topicDialogVisible.value = false;

        // 仅首版主题发新主题通知（档三-12）；编辑新版本不通知
        let notified = false;
        if (!topic.supersedesId) {
          notified = await service.value.notifyNewTopic(topic, activeBoard.value?.name ?? '');
        }
        await loadForum();
        if (editingTopic.value) {
          // 编辑后视图切到新版本
          selectedTopicId.value = topic.id;
          setMessage('新版本已发布（旧版本仍留链可审计）', 'success');
        } else {
          setMessage(
            notified
              ? '主题发布成功（已触发 P2P 同步，本机应用会话已生成新主题通知）'
              : '主题发布成功（已触发 P2P 同步；应用消息被权限/限流降级，未生成本机通知）',
            'success'
          );
        }
      } catch (error) {
        setMessage(`发布失败：${error}`, 'error');
      } finally {
        topicSaving.value = false;
      }
    };

    // ---------------- 回复 ----------------

    const submitReply = async (parentReplyId?: string) => {
      const raw = parentReplyId ? replyDraftByReply.value[parentReplyId] || '' : replyDraft.value;
      const validation = validateReplyContent(raw);
      if (!validation.ok) {
        ElMessage.warning(validation.reason || '回复内容不合法');
        return;
      }

      if (parentReplyId) {
        replyingTo.value = parentReplyId;
      } else {
        replying.value = true;
      }
      try {
        await ensureSdk();
        if (!service.value || !selectedOrgId.value || !currentRootId.value || !selectedTopicId.value) {
          throw new Error('Plugin service unavailable');
        }
        await service.value.createReply(
          selectedOrgId.value,
          currentRootId.value,
          { topicId: selectedTopicId.value, content: raw, parentReplyId },
          currentOrgRole.value,
          adminRootIds.value
        );
        if (parentReplyId) {
          replyDraftByReply.value = { ...replyDraftByReply.value, [parentReplyId]: '' };
        } else {
          replyDraft.value = '';
        }
        await loadForum();
      } catch (error) {
        setMessage(`回复失败：${error}`, 'error');
      } finally {
        replying.value = false;
        replyingTo.value = '';
      }
    };

    // ---------------- 治理事件 ----------------

    const openEventDialog = (kind: ForumTopicEventKind) => {
      pendingEventKind.value = kind;
      eventReason.value = '';
      eventDialogVisible.value = true;
    };

    const eventKindLabel = (kind: ForumTopicEventKind): string => {
      const labels: Record<ForumTopicEventKind, string> = {
        pin: '置顶',
        unpin: '取消置顶',
        feature: '加精',
        unfeature: '取消精华',
        resolve: '标记已解决',
        close: '关闭主题',
        reopen: '重新打开',
        hide: '隐藏（删除诉求）',
        unhide: '取消隐藏'
      };
      return labels[kind];
    };

    const submitEvent = async () => {
      eventSaving.value = true;
      try {
        await ensureSdk();
        if (!service.value || !selectedOrgId.value || !currentRootId.value || !selectedTopicId.value) {
          throw new Error('Plugin service unavailable');
        }
        await service.value.createTopicEvent(
          selectedOrgId.value,
          currentRootId.value,
          { topicId: selectedTopicId.value, kind: pendingEventKind.value, reason: eventReason.value || undefined },
          currentOrgRole.value
        );
        eventDialogVisible.value = false;
        await loadForum();
        setMessage(`治理操作「${eventKindLabel(pendingEventKind.value)}」已留痕`, 'success');
      } catch (error) {
        setMessage(`治理操作失败：${error}`, 'error');
      } finally {
        eventSaving.value = false;
      }
    };

    // ---------------- 验签 ----------------

    const verifyActiveTopic = async () => {
      if (!activeTopic.value) {
        return;
      }
      await ensureSdk();
      if (!service.value) {
        return;
      }
      verifying.value = true;
      try {
        const valid = await service.value.verifyTopicSignature(activeTopic.value);
        verifyResult.value = valid ? '验签通过：确为作者域身份签发' : '验签失败：签名与内容不符';
      } catch (error) {
        verifyResult.value = `验签出错：${error}`;
      } finally {
        verifying.value = false;
      }
    };

    /**
     * 卡片回调（messages.onCardAction）：主题卡片「查看/去回复」经壳层归属
     * 校验后路由到这里。card.data 捎带 orgId——卡片可能属于非当前选中组织
     * 的应用会话，定位前先切换组织，再定位主题（未同步则先重载）。
     */
    const handleCardAction = async (action: PluginCardActionPayload) => {
      if (action.actionId !== 'goto-topic') {
        return;
      }
      const data = action.data as { topicId?: string; orgId?: string } | undefined;
      const topicId = data?.topicId;
      if (!topicId) {
        return;
      }
      if (data?.orgId && data.orgId !== selectedOrgId.value) {
        if (!orgOptions.value.some((org) => org.orgId === data.orgId)) {
          setMessage('目标主题所属组织不在本机已加入的组织中。', 'warning');
          return;
        }
        selectedOrgId.value = data.orgId;
        await loadForum().catch(() => undefined);
      }
      let target = latestTopics.value.find((topic) => topic.id === topicId);
      if (!target) {
        // 目标主题不在当前视图（可能尚未同步）：重载一次再定位
        await loadForum().catch(() => undefined);
        target = latestTopics.value.find((topic) => topic.id === topicId);
      }
      if (!target) {
        setMessage('目标主题尚未同步到本机，请稍后重试。', 'warning');
        return;
      }
      selectedBoardId.value = target.boardId;
      enterTopic(target.id);
      await nextTick();
      document
        .getElementById(`topic-detail-${target.id}`)
        ?.scrollIntoView({ behavior: 'smooth', block: 'start' });
      highlightedTopicId.value = target.id;
      if (highlightTimer) {
        clearTimeout(highlightTimer);
      }
      highlightTimer = setTimeout(() => {
        highlightedTopicId.value = '';
      }, HIGHLIGHT_DURATION_MS);
    };

    const topicPreview = (content: string): string => {
      const normalized = content.replace(/\s+/g, ' ').trim();
      return normalized.length > 80 ? `${normalized.slice(0, 80)}…` : normalized;
    };

    const formatDate = (timestamp: number) => {
      return new Intl.DateTimeFormat('zh-CN', {
        year: 'numeric',
        month: '2-digit',
        day: '2-digit',
        hour: '2-digit',
        minute: '2-digit'
      }).format(new Date(timestamp));
    };

    onMounted(() => {
      void (async () => {
        const plugin = await ensureSdk();
        // 注册卡片回调（返回注销函数；仅 app 主视图注册，卡片视图收不到）
        offCardAction = plugin.messages?.onCardAction((action) => {
          void handleCardAction(action);
        }) ?? null;
        await reloadAll();
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
        if (!orgId || selectedOrgId.value === orgId) {
          return;
        }
        if (!orgOptions.value.some((org) => org.orgId === orgId)) {
          return;
        }
        selectedOrgId.value = orgId;
        onOrgChange();
      }
    );

    return {
      loading,
      message,
      messageType,
      currentRootId,
      orgOptions,
      selectedOrgId,
      activeOrg,
      isAdmin,
      canPost,
      view,
      boards,
      visibleBoards,
      activeBoard,
      topicList,
      topicListTruncated,
      FORUM_QUERY_LIMITS,
      activeTopic,
      activeTopicState,
      activeTopicSegments,
      canEditActiveTopic,
      versionHistory,
      historyVisible,
      replyThread,
      replyCountActive,
      replyDraft,
      replyDraftByReply,
      replying,
      replyingTo,
      verifying,
      verifyResult,
      highlightedTopicId,
      boardDialogVisible,
      boardSaving,
      editingBoard,
      boardDraft,
      topicDialogVisible,
      topicSaving,
      editingTopic,
      topicDraft,
      eventDialogVisible,
      eventSaving,
      pendingEventKind,
      eventReason,
      topicCountByBoard,
      lastActiveText,
      reloadAll,
      onOrgChange,
      enterBoard,
      enterTopic,
      openBoardDialog,
      submitBoard,
      toggleArchiveBoard,
      openTopicDialog,
      submitTopic,
      submitReply,
      openEventDialog,
      eventKindLabel,
      submitEvent,
      verifyActiveTopic,
      topicPreview,
      formatDate
    };
  }
});
</script>

<style scoped>
.spark-forum {
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
}

.root-id-tag {
  max-width: 100%;
}

.root-id-text {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.board-item {
  display: flex;
  align-items: center;
  gap: 12px;
  border: 1px solid var(--el-border-color);
  border-radius: 10px;
  padding: 12px;
  margin-bottom: 10px;
  cursor: pointer;
  transition: border-color 0.2s;
}

.board-item:hover {
  border-color: #0f766e;
}

.board-item.archived {
  opacity: 0.6;
}

.board-main {
  flex: 1;
  min-width: 0;
}

.board-intro {
  margin: 4px 0 0;
  color: #64748b;
  font-size: 12px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.board-stats {
  display: flex;
  flex-direction: column;
  align-items: flex-end;
  gap: 2px;
  color: #64748b;
  font-size: 12px;
  flex-shrink: 0;
}

.board-actions {
  flex-shrink: 0;
}

.topic-item {
  border: 1px solid var(--el-border-color);
  border-radius: 10px;
  padding: 12px;
  margin-bottom: 10px;
  cursor: pointer;
  transition: border-color 0.3s, box-shadow 0.3s;
}

.topic-item:hover {
  border-color: #0f766e;
}

.topic-item.highlighted {
  border-color: #0f766e;
  box-shadow: 0 0 0 3px rgba(15, 118, 110, 0.18);
}

/* 卡片回调定位后主题详情短暂高亮（见 HIGHLIGHT_DURATION_MS） */
.detail-highlighted {
  border-color: #0f766e;
  box-shadow: 0 0 0 3px rgba(15, 118, 110, 0.18);
  transition: box-shadow 0.3s;
}

.topic-title-row {
  display: flex;
  align-items: center;
  gap: 6px;
  flex-wrap: wrap;
}

.topic-title {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.topic-preview {
  margin: 6px 0;
  color: #64748b;
  font-size: 13px;
}

.truncation-hint {
  margin: 4px 0 10px;
  color: #b45309;
  font-size: 12px;
}

.topic-detail-title {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.topic-state-row {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 8px;
  margin-bottom: 8px;
}

.post-meta {
  display: flex;
  justify-content: space-between;
  gap: 10px;
  color: #64748b;
  font-size: 12px;
}

.post-meta .author {
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.topic-content {
  margin: 10px 0;
  white-space: pre-wrap;
  word-break: break-word;
}

.topic-content.small {
  font-size: 13px;
  color: #475569;
}

.affair-ref {
  display: inline-block;
  background: #eef2ff;
  color: #4338ca;
  border: 1px solid #c7d2fe;
  border-radius: 4px;
  padding: 0 6px;
  font-size: 12px;
}

.topic-ops {
  display: flex;
  gap: 8px;
  margin-bottom: 8px;
}

.history-item {
  border: 1px dashed var(--el-border-color);
  border-radius: 8px;
  padding: 8px;
  margin-bottom: 8px;
}

.moderation-actions {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
}

.verify-result {
  color: #64748b;
  font-size: 12px;
}

.comment-list {
  margin-top: 10px;
  display: grid;
  gap: 8px;
}

.comment-item {
  border-left: 2px solid #d1fae5;
  background: #f8fafc;
  padding: 8px;
}

.comment-item.nested {
  margin-left: 16px;
  border-left-color: #bae6fd;
}

.comment-content {
  margin: 8px 0;
  white-space: pre-wrap;
  word-break: break-word;
}

.sig-tag {
  margin-right: 4px;
}

.reply-flag {
  color: #0f766e;
  font-weight: 600;
}

.reply-editor {
  display: flex;
  gap: 8px;
  align-items: flex-start;
  margin-top: 12px;
}

.reply-editor .el-input,
.reply-editor .el-textarea {
  flex: 1;
  min-width: 0;
}

.reply-editor .el-button {
  flex-shrink: 0;
}

.reply-editor.small {
  margin-top: 6px;
  align-items: center;
}

.hint {
  color: #64748b;
  margin: 8px 0 0;
  font-size: 12px;
}

.event-form {
  margin-top: 10px;
}
</style>

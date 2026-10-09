<template>
  <!-- 已知缺口（S4 如实标注）：本地草稿的恢复入口未接——服务层
       saveDraft/listDrafts/deleteDraft 就绪，「从草稿继续编辑」排后续迭代。 -->
  <section class="project-root">
    <el-alert
      v-if="message"
      :type="messageType"
      :title="message"
      :closable="true"
      class="message-bar"
      @close="message = ''"
    />

    <template v-if="!affairsReady">
      <el-empty :description="AFFAIRS_MISSING_TEXT" />
    </template>

    <template v-else>
      <section class="layout">
        <!-- 左栏：议题列表 + 创建/关注入口 -->
        <aside class="sidebar">
          <div class="sidebar-actions">
            <el-button type="primary" size="small" :disabled="readonlySpace" @click="openCreateDialog">
              创建项目
            </el-button>
            <el-button size="small" :disabled="readonlySpace" @click="followDialogVisible = true">关注议题</el-button>
          </div>
          <p v-if="readonlySpace" class="readonly-hint">移动端只读形态（档三-2）：写操作已禁用</p>
          <el-scrollbar class="project-list">
            <div
              v-for="project in projects"
              :key="project.affairId"
              class="project-item"
              :class="{ active: project.affairId === selectedAffairId }"
              @click="selectProject(project.affairId)"
            >
              <div class="project-item-title">
                <el-tag v-if="project.isPublic" size="small" type="success" effect="plain">公开</el-tag>
                <span>{{ project.title }}</span>
              </div>
              <div class="project-item-sub">{{ project.affairId.slice(0, 12) }}… · {{ project.operationCount }} 条操作</div>
            </div>
            <el-empty v-if="projects.length === 0" description="尚无关注的项目议题" :image-size="60" />
          </el-scrollbar>
        </aside>

        <!-- 主区：项目工作区 -->
        <main class="workspace">
          <template v-if="currentProject">
            <header class="workspace-header">
              <h2 class="workspace-title">
                {{ currentProject.title }}
                <el-tag v-if="currentProject.isPublic" size="small" type="success">已公开发布</el-tag>
                <el-tag v-else size="small" type="info" effect="plain">未公开</el-tag>
              </h2>
              <p class="workspace-summary">{{ currentProject.summary }}</p>
              <div class="workspace-meta">
                <el-tag v-for="tag in currentProject.tags" :key="tag" size="small" effect="plain">{{ tag }}</el-tag>
                <span class="meta-text">维护者 {{ maintainers.length }} 人 · 未决子事务 {{ openChildCount }} 个</span>
                <el-button size="small" text type="danger" :disabled="readonlySpace" @click="onUnfollow">
                  取关（保留已复制数据）
                </el-button>
              </div>
            </header>

            <el-tabs v-model="activeTab" class="workspace-tabs">
              <!-- 讨论流 -->
              <el-tab-pane label="讨论" name="discussion">
                <el-scrollbar class="tab-scroll">
                  <div v-for="entry in timeline" :key="entry.opHash" class="timeline-item">
                    <span class="timeline-kind" :class="`kind-${entry.kind}`">{{ timelineKindLabel(entry.kind) }}</span>
                    <span class="timeline-text">{{ entry.text }}</span>
                    <span class="timeline-actor">{{ (entry.actorIdentity ?? '').slice(0, 8) || '未知' }}</span>
                  </div>
                  <el-empty v-if="timeline.length === 0" description="暂无操作日志" :image-size="60" />
                </el-scrollbar>
                <div class="comment-box">
                  <el-input
                    v-model="commentDraft"
                    type="textarea"
                    :rows="2"
                    maxlength="5000"
                    placeholder="发言将签名后写入议题操作日志（append-only，不可收回）"
                    :disabled="readonlySpace"
                  />
                  <el-button type="primary" size="small" :disabled="readonlySpace || commenting" @click="onSubmitComment">
                    发言
                  </el-button>
                </div>
              </el-tab-pane>

              <!-- 子事务 -->
              <el-tab-pane label="子事务" name="children">
                <div class="tab-toolbar">
                  <el-radio-group v-model="childTypeFilter" size="small">
                    <el-radio-button value="">全部</el-radio-button>
                    <el-radio-button value="bug">缺陷</el-radio-button>
                    <el-radio-button value="proposal">建议</el-radio-button>
                    <el-radio-button value="pr">PR</el-radio-button>
                  </el-radio-group>
                  <el-button type="primary" size="small" :disabled="readonlySpace" @click="openChildDialog">
                    发起子事务
                  </el-button>
                </div>
                <el-table :data="filteredChildren" size="small" class="children-table" @row-click="openChildDetail">
                  <el-table-column label="类型" width="90">
                    <template #default="{ row }">
                      <el-tag size="small" effect="plain">{{ row.typeLabel }}</el-tag>
                    </template>
                  </el-table-column>
                  <el-table-column prop="title" label="标题" min-width="220" show-overflow-tooltip />
                  <el-table-column label="处置" width="90">
                    <template #default="{ row }">
                      <el-tag size="small" :type="row.disposition.state === 'open' ? 'info' : 'success'" effect="plain">
                        {{ dispositionLabel(row.disposition.state) }}
                      </el-tag>
                    </template>
                  </el-table-column>
                  <el-table-column label="决议" width="90">
                    <template #default="{ row }">
                      <el-tag size="small" :type="row.resolutionBadge === 'pending' ? 'warning' : 'info'" effect="plain">
                        {{ RESOLUTION_BADGE_LABELS[row.resolutionBadge] }}
                      </el-tag>
                    </template>
                  </el-table-column>
                </el-table>
                <el-empty v-if="filteredChildren.length === 0" description="暂无子事务（反馈回流会自动聚合到这里）" :image-size="60" />
              </el-tab-pane>

              <!-- 看板（库件组合，只读 MVP） -->
              <el-tab-pane label="看板" name="board">
                <template v-if="boardView">
                  <p class="tab-note">只读形态（MVP）：子事务按状态聚合分列；拖动转列排后续迭代。数据写本插件命名空间。</p>
                  <div class="board-columns">
                    <div v-for="column in boardView.columns" :key="column.column.id" class="board-column">
                      <div class="board-column-title">
                        {{ column.column.title }}
                        <el-badge v-if="column.freshCount > 0" :value="column.freshCount" type="warning" />
                        <span v-else class="board-count">{{ column.cards.length }}</span>
                      </div>
                      <div v-for="card in column.cards" :key="card.ref" class="board-card">
                        <template v-if="card.kind === 'affair'">
                          <el-tag size="small" effect="plain">{{ card.card.affairType }}</el-tag>
                          <span>{{ card.card.title }}</span>
                        </template>
                      </div>
                      <el-empty v-if="column.cards.length === 0" description="" :image-size="30" />
                    </div>
                  </div>
                </template>
                <div v-else class="board-empty">
                  <el-empty description="本项目尚未初始化看板" :image-size="60" />
                  <el-button size="small" type="primary" :disabled="readonlySpace || boardInitializing" @click="onInitBoard">
                    初始化看板
                  </el-button>
                  <p v-if="boardInitError" class="tab-note warn">{{ boardInitError }}</p>
                </div>
              </el-tab-pane>

              <!-- 发布（库件组合；仅组织空间） -->
              <el-tab-pane label="发布" name="release">
                <template v-if="isPersonal">
                  <el-empty description="发布管理件仅组织空间可用——本区未启用（诚实降级，不阻塞其余功能）" :image-size="60" />
                </template>
                <template v-else>
                  <div class="tab-toolbar">
                    <el-button size="small" type="primary" :disabled="readonlySpace" @click="releaseDialogVisible = true">
                      登记发布单
                    </el-button>
                    <el-button v-if="!releaseConfig && isOrgAdmin" size="small" :disabled="readonlySpace" @click="onInitReleaseConfig">
                      初始化发布权配置
                    </el-button>
                    <span class="tab-note" v-if="evidence.headHash">存证链头 {{ evidence.headHash.slice(0, 12) }}…</span>
                  </div>
                  <p v-if="!releaseMarketAvailable" class="tab-note">
                    市场模块不可用或移动端：降级只做登记，核验委托桌面端成员（档二-8）。
                  </p>
                  <el-table :data="releaseRows" size="small">
                    <el-table-column label="目标" min-width="160">
                      <template #default="{ row }">{{ row.release.pluginId }}</template>
                    </el-table-column>
                    <el-table-column prop="release.version" label="版本" width="100" />
                    <el-table-column label="状态" width="100">
                      <template #default="{ row }">
                        <el-tag size="small" effect="plain">{{ row.stateLabel }}</el-tag>
                      </template>
                    </el-table-column>
                    <el-table-column label="包哈希" min-width="140">
                      <template #default="{ row }">
                        <span class="mono">{{ (row.release.artifacts[0]?.sha256 ?? '').slice(0, 16) || '—' }}…</span>
                      </template>
                    </el-table-column>
                    <el-table-column label="操作" width="180">
                      <template #default="{ row }">
                        <el-button
                          v-if="row.state === 'registered' || row.state === 'verify-failed'"
                          size="small"
                          :disabled="readonlySpace"
                          @click="openVerifyDialog(row.release.id)"
                        >
                          本机核验
                        </el-button>
                        <el-button
                          v-if="row.state === 'verified'"
                          size="small"
                          type="primary"
                          :disabled="readonlySpace"
                          @click="onPublishRelease(row.release.id)"
                        >
                          推进发布
                        </el-button>
                      </template>
                    </el-table-column>
                  </el-table>
                  <el-empty v-if="releaseRows.length === 0" description="暂无发布单" :image-size="60" />
                </template>
              </el-tab-pane>

              <!-- 文档 -->
              <el-tab-pane label="文档" name="docs">
                <div class="tab-toolbar">
                  <el-button size="small" type="primary" :disabled="readonlySpace" @click="openDocDialog()">
                    新建文档
                  </el-button>
                </div>
                <div v-for="doc in docs" :key="doc.docId" class="doc-item">
                  <span class="doc-title">{{ doc.title }}</span>
                  <span class="doc-sub">v{{ doc.latestSeq }} · {{ doc.versionCount }} 个版本</span>
                  <el-button size="small" text @click="openDocHistory(doc.docId)">历史</el-button>
                  <el-button size="small" text type="primary" :disabled="readonlySpace" @click="openDocDialog(doc.docId)">
                    编辑（新版本）
                  </el-button>
                </div>
                <el-empty v-if="docs.length === 0" description="暂无项目文档" :image-size="60" />
              </el-tab-pane>

              <!-- 成员 -->
              <el-tab-pane label="成员" name="members">
                <p class="tab-note">阶梯名册由内核从事务日志与存证链确定性推导，原样呈现不美化；履历为本地副本所见。</p>
                <el-table :data="members" size="small">
                  <el-table-column label="身份（插件域）" min-width="160">
                    <template #default="{ row }">
                      <span class="mono">{{ row.identity.slice(0, 16) }}…</span>
                      <el-tag v-if="row.isMaintainer" size="small" type="warning" effect="plain">维护者</el-tag>
                    </template>
                  </el-table-column>
                  <el-table-column prop="tier" label="阶梯" width="110" />
                  <el-table-column prop="accepts" label="累计采纳" width="90" />
                  <el-table-column label="账龄" width="120">
                    <template #default="{ row }">{{ formatAge(row.accountAgeMs) }}</template>
                  </el-table-column>
                  <el-table-column label="履历" width="90">
                    <template #default="{ row }">
                      <el-button size="small" text @click="onShowProfile(row.identity)">查看</el-button>
                    </template>
                  </el-table-column>
                </el-table>
                <el-empty v-if="members.length === 0" description="名册为空（尚无链上活跃）" :image-size="60" />
              </el-tab-pane>

              <!-- 代码（git-repo 缺库包形态，诚实降级） -->
              <el-tab-pane label="代码" name="code">
                <el-empty :image-size="80" description="">
                  <template #description>
                    <p class="code-gap-title">代码视图未启用</p>
                    <p class="tab-note">
                      代码仓库件（spark-git-repo）尚缺库包形态（无 lib.ts 库入口、服务层未做命名空间参数化），
                      本插件按组合纪律不直接内嵌其数据域——该缺口已记录上报，待库包形态补齐后接入。
                    </p>
                    <p class="tab-note">
                      降级替代：安装独立的「代码仓库」插件浏览镜像与发起 PR；桌面端可本地 git clone 只读镜像，
                      导出 bundle 后在「子事务」页以 PR 类型提交（附件 cid 手工填入）。
                    </p>
                  </template>
                </el-empty>
              </el-tab-pane>
            </el-tabs>
          </template>
          <el-empty v-else description="选择左侧项目议题，或创建 / 关注一个新项目" :image-size="100" />
        </main>
      </section>
    </template>

    <!-- 创建项目对话框（含公开发布显式确认，档二-2 补录） -->
    <el-dialog v-model="createDialogVisible" title="创建项目议题" width="560px">
      <el-form label-width="90px">
        <el-form-item label="标题" required>
          <el-input v-model="createDraft.title" maxlength="120" show-word-limit />
        </el-form-item>
        <el-form-item label="简介" required>
          <el-input v-model="createDraft.summary" type="textarea" :rows="3" maxlength="2000" show-word-limit />
        </el-form-item>
        <el-form-item label="标签">
          <el-input v-model="createDraft.tagsText" placeholder="逗号分隔，至多 8 个" />
        </el-form-item>
        <el-form-item label="公开发布">
          <el-checkbox v-model="createDraft.publish">进入全网公共目录（标题/简介/标签可被任何 indexer 收录检索）</el-checkbox>
        </el-form-item>
        <el-alert
          v-if="createDraft.publish"
          type="warning"
          :closable="false"
          title="公开发布不可撤回：元数据将洪泛进公共元数据面，任何人无需许可即可发现该项目。"
        />
        <el-form-item v-if="createDraft.publish" label="确认">
          <el-checkbox v-model="createDraft.confirmedPublish">我已知晓公开后果，确认公开发布</el-checkbox>
        </el-form-item>
        <el-form-item label="规则模板">
          <span class="tab-note">维护者制（MVP 唯一模板）：维护者集合初始 = 我的插件域身份；PR 采纳 = 单维护者合并回执 + 禁非快进。</span>
        </el-form-item>
      </el-form>
      <template #footer>
        <el-button @click="createDialogVisible = false">取消</el-button>
        <el-button
          type="primary"
          :disabled="creating || (createDraft.publish && !createDraft.confirmedPublish)"
          @click="onCreateProject"
        >
          签名并创建
        </el-button>
      </template>
    </el-dialog>

    <!-- 关注议题对话框 -->
    <el-dialog v-model="followDialogVisible" title="关注已有议题" width="560px">
      <el-input
        v-model="followGenesisJson"
        type="textarea"
        :rows="6"
        placeholder="粘贴创世记录 JSON 原文（邀请链接/转发路径；内核全链校验 + affairId 自认证复算）"
      />
      <p class="tab-note">关注即副本：关注后议题数据将复制到本机并参与做种。</p>
      <template #footer>
        <el-button @click="followDialogVisible = false">取消</el-button>
        <el-button type="primary" :disabled="following" @click="onFollow">校验并关注</el-button>
      </template>
    </el-dialog>

    <!-- 发起子事务对话框 -->
    <el-dialog v-model="childDialogVisible" title="发起子事务" width="560px">
      <el-form label-width="90px">
        <el-form-item label="类型" required>
          <el-radio-group v-model="childDraft.type">
            <el-radio-button value="bug">缺陷</el-radio-button>
            <el-radio-button value="proposal">建议</el-radio-button>
            <el-radio-button value="pr">PR</el-radio-button>
          </el-radio-group>
        </el-form-item>
        <el-form-item label="标题" required>
          <el-input v-model="childDraft.title" maxlength="120" show-word-limit />
        </el-form-item>
        <el-form-item label="说明" required>
          <el-input v-model="childDraft.summary" type="textarea" :rows="4" maxlength="2000" show-word-limit />
        </el-form-item>
        <el-form-item v-if="childDraft.type === 'pr'" label="bundle cid">
          <el-input v-model="childDraft.bundleCid" placeholder="可选：git bundle 内容面 cid（64 位 hex）" />
        </el-form-item>
      </el-form>
      <template #footer>
        <el-button @click="childDialogVisible = false">取消</el-button>
        <el-button type="primary" :disabled="childSaving" @click="onCreateChild">签名并提交</el-button>
      </template>
    </el-dialog>

    <!-- 子事务详情抽屉 -->
    <el-drawer v-model="childDetailVisible" :title="activeChild?.title ?? '子事务详情'" size="480px">
      <template v-if="activeChild">
        <p>
          <el-tag size="small" effect="plain">{{ activeChild.typeLabel }}</el-tag>
          <el-tag size="small" :type="activeChild.disposition.state === 'open' ? 'info' : 'success'" effect="plain">
            {{ dispositionLabel(activeChild.disposition.state) }}
          </el-tag>
        </p>
        <p class="detail-summary">{{ activeChild.summary }}</p>
        <div v-if="activeChild.disposition.note" class="tab-note">处置附言：{{ activeChild.disposition.note }}</div>
        <el-divider />
        <div v-for="entry in childTimeline" :key="entry.opHash" class="timeline-item">
          <span class="timeline-kind" :class="`kind-${entry.kind}`">{{ timelineKindLabel(entry.kind) }}</span>
          <span class="timeline-text">{{ entry.text }}</span>
        </div>
        <el-empty v-if="childTimeline.length === 0" description="暂无操作日志" :image-size="50" />
        <div class="comment-box">
          <el-input v-model="childCommentDraft" type="textarea" :rows="2" maxlength="5000" :disabled="readonlySpace" />
          <el-button size="small" :disabled="readonlySpace" @click="onSubmitChildComment">发言</el-button>
        </div>
        <el-divider />
        <div class="disposition-actions">
          <el-input v-model="dispositionNote" placeholder="处置附言（可选）" size="small" :disabled="readonlySpace" />
          <el-button size="small" type="success" :disabled="readonlySpace" @click="onDisposition('adopted')">
            采纳（维护者）
          </el-button>
          <el-button size="small" type="danger" :disabled="readonlySpace" @click="onDisposition('closed')">
            关闭（维护者）
          </el-button>
        </div>
        <p class="tab-note">
          {{ activeChild.type === 'pr'
            ? 'PR 处置 = 单维护者合并回执即生效（档一-3），不走公示期。'
            : '缺陷/建议处置 = 决议操作入内核公示期（24h）：期间决议徽标显示「待确认」，期满无阈值异议转「已生效」（R1）。' }}
        </p>
      </template>
    </el-drawer>

    <!-- 登记发布单对话框 -->
    <el-dialog v-model="releaseDialogVisible" title="登记发布单" width="560px">
      <el-form label-width="110px">
        <el-form-item label="目标插件" required>
          <el-input v-model="releaseDraft.pluginId" placeholder="如 spark-project" />
        </el-form-item>
        <el-form-item label="版本号" required>
          <el-input v-model="releaseDraft.version" placeholder="semver，如 0.1.0" />
        </el-form-item>
        <el-form-item label="update-manifest">
          <el-input
            v-model="releaseDraft.updateManifestJson"
            type="textarea"
            :rows="4"
            placeholder="粘贴 CI 产出的 update-manifest.json 原文（解析资产清单与包哈希）"
          />
        </el-form-item>
        <el-form-item label="变更说明">
          <el-input v-model="releaseDraft.changelog" type="textarea" :rows="3" maxlength="20000" />
        </el-form-item>
        <el-form-item label="决议引用">
          <el-input v-model="releaseDraft.decisionRef" placeholder="可选：发布决议 opHash / 事务引用" />
        </el-form-item>
      </el-form>
      <template #footer>
        <el-button @click="releaseDialogVisible = false">取消</el-button>
        <el-button type="primary" :disabled="releaseSaving" @click="onRegisterRelease">签名并登记</el-button>
      </template>
    </el-dialog>

    <!-- 本机核验对话框（档一-6 本机导入复算；核验通过方可推进发布） -->
    <el-dialog v-model="verifyDialogVisible" title="本机核验（导入复算）" width="560px">
      <el-form label-width="110px">
        <el-form-item label=".spkg 包路径" required>
          <el-input v-model="verifySpkgPath" placeholder="本机 .spkg 包文件路径（内核复算整包哈希与登记值三方比对）" />
        </el-form-item>
      </el-form>
      <p class="tab-note">验签在内核市场通路执行，本插件只验不签；比对不一致会登记「核验失败」事件并原样记录原因。</p>
      <template #footer>
        <el-button @click="verifyDialogVisible = false">取消</el-button>
        <el-button type="primary" :disabled="verifySaving || !verifySpkgPath.trim()" @click="onVerifyRelease">执行核验</el-button>
      </template>
    </el-dialog>

    <!-- 文档编辑对话框 -->
    <el-dialog v-model="docDialogVisible" :title="docDraft.docId ? '编辑文档（追加新版本）' : '新建文档'" width="640px">
      <el-input v-model="docDraft.title" maxlength="120" placeholder="文档标题" class="doc-title-input" />
      <el-input v-model="docDraft.body" type="textarea" :rows="12" maxlength="50000" placeholder="正文（纯文本；append-only 版本链，历史可溯）" />
      <template #footer>
        <el-button @click="docDialogVisible = false">取消</el-button>
        <el-button type="primary" :disabled="docSaving" @click="onSaveDoc">保存新版本</el-button>
      </template>
    </el-dialog>

    <!-- 文档历史抽屉 -->
    <el-drawer v-model="docHistoryVisible" title="文档历史" size="480px">
      <div v-for="version in docHistoryList" :key="`${version.docId}:${version.seq}`" class="doc-version">
        <div class="doc-version-head">
          <strong>v{{ version.seq }}</strong>
          <span class="tab-note">{{ new Date(version.createdAt).toLocaleString() }} · {{ version.authorIdentity.slice(0, 8) }}…</span>
        </div>
        <pre class="doc-body">{{ version.body }}</pre>
      </div>
    </el-drawer>

    <!-- 公开履历抽屉 -->
    <el-drawer v-model="profileVisible" title="公开履历（本地副本所见）" size="420px">
      <template v-if="profile">
        <p>参与事务数：{{ profile.affairsParticipated }}</p>
        <p>提议 / 采纳：{{ profile.proposals }} / {{ profile.adoptions }}</p>
        <p>表决票：{{ profile.votes }}（赞成 {{ profile.votesYes }} / 反对 {{ profile.votesNo }}）</p>
        <p class="tab-note">账龄：{{ formatAge(profile.accountAgeMs) }}；未关注/未复制到的事务不参与聚合（诚实边界）。</p>
      </template>
      <el-empty v-else description="履历读取失败或该身份无链上活动" :image-size="60" />
    </el-drawer>
  </section>
</template>

<script lang="ts">
import { computed, defineComponent, onMounted, onUnmounted, ref, shallowRef, watch } from 'vue';
import { ElMessage } from 'element-plus';
import { ensurePluginSDK } from '../../packages/plugin-sdk/src';
import type { PluginSDK } from '../../packages/plugin-sdk/src';
import {
  AFFAIRS_MODULE_MISSING,
  ProjectService,
  type ReleaseRow
} from './service';
import {
  DISPOSITION_LABELS,
  RESOLUTION_BADGE_LABELS,
  type ChildAffairType,
  type ChildAffairView,
  type DispositionAction,
  type ProjectDocSummary,
  type ProjectDocVersion,
  type ProjectMemberView,
  type ProjectMeta,
  type TimelineEntry
} from './model';
import type { KanbanColumnView } from './vendor/github.com/welyin/spark/plugins/spark-kanban/model';
import type { ReleaseManagerConfig } from './vendor/github.com/welyin/spark/plugins/spark-release-manager/model';
import type { AffairPublicProfile } from '../../packages/plugin-sdk/src';

type OrganizationView = {
  orgId: string;
  name: string;
  members: Array<{ rootId: string; role: 'admin' | 'member' }>;
};

export default defineComponent({
  name: 'ProjectView',
  props: {
    /** 运行上下文（桥握手 ctx 经入口注入；库包形态下由组合者注入） */
    pluginContext: {
      type: Object as () => { spaceType?: 'personal' | 'org'; orgId?: string; platform?: string } | undefined,
      required: false,
      default: undefined
    }
  },
  setup(props) {
    const sdk = ref<PluginSDK | null>(null);
    // shallowRef：ref() 的 UnwrapRef 会把类实例映射成公共结构类型（剥掉私有
    // 字段），导致与 ProjectService 声明类型不兼容（TS2740）
    const service = shallowRef<ProjectService | null>(null);
    const affairsReady = ref(true);
    const message = ref('');
    const messageType = ref<'info' | 'success' | 'warning' | 'error'>('info');

    const currentRootId = ref<string | null>(null);
    const isPersonal = computed(() => props.pluginContext?.spaceType === 'personal');
    const readonlySpace = computed(
      () => props.pluginContext?.platform === 'android' || props.pluginContext?.platform === 'ios'
    );
    const orgOptions = ref<OrganizationView[]>([]);
    const selectedOrgId = ref('');

    // 议题列表与当前工作区
    const projects = ref<ProjectMeta[]>([]);
    const selectedAffairId = ref('');
    const currentProject = ref<ProjectMeta | null>(null);
    const maintainers = ref<string[]>([]);
    const activeTab = ref<'discussion' | 'children' | 'board' | 'release' | 'docs' | 'members' | 'code'>('discussion');

    // 讨论流
    const timeline = ref<TimelineEntry[]>([]);
    const commentDraft = ref('');
    const commenting = ref(false);

    // 子事务
    const children = ref<ChildAffairView[]>([]);
    const childTypeFilter = ref('');
    const childDialogVisible = ref(false);
    const childSaving = ref(false);
    const childDraft = ref<{ type: ChildAffairType; title: string; summary: string; bundleCid: string }>({
      type: 'bug',
      title: '',
      summary: '',
      bundleCid: ''
    });
    const childDetailVisible = ref(false);
    const activeChild = ref<ChildAffairView | null>(null);
    const childTimeline = ref<TimelineEntry[]>([]);
    const childCommentDraft = ref('');
    const dispositionNote = ref('');

    // 看板（库件组合，只读 MVP）
    const boardView = ref<{ columns: KanbanColumnView[] } | null>(null);
    const boardInitializing = ref(false);
    const boardInitError = ref('');

    // 发布（库件组合，仅组织空间）
    const releaseRows = ref<ReleaseRow[]>([]);
    const releaseConfig = ref<ReleaseManagerConfig | null>(null);
    const releaseMarketAvailable = ref(false);
    const evidence = ref<{ headHash: string | null; chainValid: boolean | null; chainHeight: number | null }>({
      headHash: null,
      chainValid: null,
      chainHeight: null
    });
    const releaseDialogVisible = ref(false);
    const releaseSaving = ref(false);
    const releaseDraft = ref({ pluginId: '', version: '', updateManifestJson: '', changelog: '', decisionRef: '' });
    const verifyDialogVisible = ref(false);
    const verifySaving = ref(false);
    const verifyTargetId = ref('');
    const verifySpkgPath = ref('');

    // 文档
    const docs = ref<ProjectDocSummary[]>([]);
    const docDialogVisible = ref(false);
    const docSaving = ref(false);
    const docDraft = ref<{ docId?: string; title: string; body: string }>({ title: '', body: '' });
    const docHistoryVisible = ref(false);
    const docHistoryList = ref<ProjectDocVersion[]>([]);

    // 成员
    const members = ref<ProjectMemberView[]>([]);
    const profileVisible = ref(false);
    const profile = ref<AffairPublicProfile | null>(null);

    // 创建 / 关注
    const createDialogVisible = ref(false);
    const creating = ref(false);
    const createDraft = ref({ title: '', summary: '', tagsText: '', publish: false, confirmedPublish: false });
    const followDialogVisible = ref(false);
    const followGenesisJson = ref('');
    const following = ref(false);

    const AFFAIRS_MISSING_TEXT = AFFAIRS_MODULE_MISSING;

    /** 当前数据域 id（组织空间 = orgId；个人空间 = 'personal'） */
    const spaceId = computed(() => (isPersonal.value ? 'personal' : selectedOrgId.value));

    const activeOrg = computed(() => orgOptions.value.find((org) => org.orgId === selectedOrgId.value) ?? null);
    const isOrgAdmin = computed(
      () => Boolean(currentRootId.value) &&
        activeOrg.value?.members.some((member) => member.rootId === currentRootId.value && member.role === 'admin') === true
    );
    const currentOrgRole = computed<'admin' | 'member' | null>(() => {
      if (!activeOrg.value || !currentRootId.value) {
        return null;
      }
      return activeOrg.value.members.find((member) => member.rootId === currentRootId.value)?.role ?? null;
    });
    const orgAdminRootIds = computed(() =>
      (activeOrg.value?.members ?? []).filter((member) => member.role === 'admin').map((member) => member.rootId)
    );

    const filteredChildren = computed(() =>
      childTypeFilter.value ? children.value.filter((child) => child.type === childTypeFilter.value) : children.value
    );
    const openChildCount = computed(() => children.value.filter((child) => child.disposition.state === 'open').length);

    const setMessage = (text: string, type: 'info' | 'success' | 'warning' | 'error' = 'info') => {
      message.value = text;
      messageType.value = type;
    };

    const ensureService = async (): Promise<ProjectService> => {
      const existing = service.value;
      if (existing) {
        return existing;
      }
      sdk.value = await ensurePluginSDK();
      const created = new ProjectService(sdk.value);
      service.value = created;
      affairsReady.value = created.affairsAvailable;
      return created;
    };

    // ------------------------------------------------------------------
    // 加载
    // ------------------------------------------------------------------

    const loadProjects = async () => {
      const svc = await ensureService();
      projects.value = await svc.listProjects();
      if (selectedAffairId.value && !projects.value.some((project) => project.affairId === selectedAffairId.value)) {
        selectedAffairId.value = '';
        currentProject.value = null;
      }
    };

    const loadWorkspace = async () => {
      if (!selectedAffairId.value) {
        return;
      }
      const svc = await ensureService();
      const detail = await svc.getProject(selectedAffairId.value);
      if (!detail) {
        setMessage('创世记录尚未同步到本机（等待复制收敛）', 'warning');
        return;
      }
      currentProject.value = detail.meta;
      maintainers.value = detail.maintainers;
      const [tl, ch] = await Promise.all([
        svc.listTimeline(selectedAffairId.value),
        svc.listChildren(selectedAffairId.value)
      ]);
      timeline.value = tl;
      children.value = ch;
      docs.value = await svc.listDocs(selectedAffairId.value);
      if (activeTab.value === 'board') {
        await loadBoard();
      }
      if (activeTab.value === 'members') {
        await loadMembers();
      }
    };

    const loadBoard = async () => {
      if (!selectedAffairId.value || !spaceId.value) {
        return;
      }
      const svc = await ensureService();
      boardInitError.value = '';
      boardView.value = await svc.projectBoardView(selectedAffairId.value, spaceId.value);
    };

    const loadReleases = async () => {
      if (isPersonal.value || !selectedOrgId.value) {
        return;
      }
      const svc = await ensureService();
      const { rows, config } = await svc.listReleaseRows(selectedOrgId.value, orgAdminRootIds.value);
      releaseRows.value = rows;
      releaseConfig.value = config;
      releaseMarketAvailable.value = svc.releaseMarketAvailable;
      evidence.value = await svc.getReleaseEvidence().catch(() => evidence.value);
    };

    const loadMembers = async () => {
      if (!selectedAffairId.value) {
        return;
      }
      const svc = await ensureService();
      const { members: roster } = await svc.listMembers(selectedAffairId.value);
      members.value = roster;
    };

    const selectProject = async (affairId: string) => {
      selectedAffairId.value = affairId;
      try {
        await loadWorkspace();
      } catch (error) {
        setMessage(`加载失败：${(error as Error).message}`, 'error');
      }
    };

    const loadOrganizations = async () => {
      if (isPersonal.value) {
        orgOptions.value = [];
        return;
      }
      const plugin = sdk.value ?? (await ensurePluginSDK());
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

    const reloadAll = async () => {
      try {
        const plugin = await ensurePluginSDK();
        if (!sdk.value) {
          return;
        }
        const identity = await plugin.runtime.currentRoot();
        currentRootId.value = identity.rootId;
        await loadOrganizations();
        await loadProjects();
        if (selectedAffairId.value) {
          await loadWorkspace();
        }
        if (!isPersonal.value && selectedOrgId.value) {
          await loadReleases();
        }
      } catch (error) {
        setMessage(`加载失败：${(error as Error).message}`, 'error');
      }
    };

    // ------------------------------------------------------------------
    // 项目创建 / 关注 / 取关
    // ------------------------------------------------------------------

    const openCreateDialog = () => {
      createDraft.value = { title: '', summary: '', tagsText: '', publish: false, confirmedPublish: false };
      createDialogVisible.value = true;
    };

    const onCreateProject = async () => {
      creating.value = true;
      try {
        const svc = await ensureService();
        const tags = createDraft.value.tagsText
          .split(/[,，]/)
          .map((tag) => tag.trim())
          .filter(Boolean);
        const { affairId, commentPosted, cardSent } = await svc.createProject(
          {
            title: createDraft.value.title,
            summary: createDraft.value.summary,
            tags,
            publish: createDraft.value.publish
          },
          { confirmedPublish: createDraft.value.confirmedPublish }
        );
        createDialogVisible.value = false;
        setMessage(
          `项目议题已创建（${affairId.slice(0, 12)}…）${createDraft.value.publish ? '，公开发布声明已入创世记录' : ''}${commentPosted ? '' : '；首条议题说明未写入（可稍后发言补充）'}${cardSent ? '' : '；回执卡片未送达（权限/限流降级）'}`,
          'success'
        );
        await loadProjects();
        await selectProject(affairId);
      } catch (error) {
        setMessage((error as Error).message, 'error');
      } finally {
        creating.value = false;
      }
    };

    const onFollow = async () => {
      following.value = true;
      try {
        const svc = await ensureService();
        const affairId = await svc.followGenesis(JSON.parse(followGenesisJson.value));
        followDialogVisible.value = false;
        followGenesisJson.value = '';
        setMessage(`已关注（${affairId.slice(0, 12)}…），等待复制收敛`, 'success');
        await loadProjects();
        await selectProject(affairId);
      } catch (error) {
        setMessage(`关注失败：${(error as Error).message}`, 'error');
      } finally {
        following.value = false;
      }
    };

    const onUnfollow = async () => {
      if (!selectedAffairId.value) {
        return;
      }
      try {
        const svc = await ensureService();
        await svc.unfollow(selectedAffairId.value);
        selectedAffairId.value = '';
        currentProject.value = null;
        await loadProjects();
      } catch (error) {
        setMessage(`取关失败：${(error as Error).message}`, 'error');
      }
    };

    // ------------------------------------------------------------------
    // 讨论 / 子事务
    // ------------------------------------------------------------------

    const onSubmitComment = async () => {
      if (!selectedAffairId.value) {
        return;
      }
      commenting.value = true;
      try {
        const svc = await ensureService();
        await svc.submitComment(selectedAffairId.value, commentDraft.value);
        commentDraft.value = '';
        await loadWorkspace();
      } catch (error) {
        setMessage((error as Error).message, 'error');
      } finally {
        commenting.value = false;
      }
    };

    const openChildDialog = () => {
      childDraft.value = { type: 'bug', title: '', summary: '', bundleCid: '' };
      childDialogVisible.value = true;
    };

    const onCreateChild = async () => {
      if (!selectedAffairId.value) {
        return;
      }
      childSaving.value = true;
      try {
        const svc = await ensureService();
        const { affairId, noticePosted } = await svc.createChild(selectedAffairId.value, childDraft.value);
        childDialogVisible.value = false;
        setMessage(
          `子事务已创建（${affairId.slice(0, 12)}…）${noticePosted ? '' : '；议题通告写入失败（不影响子事务本体）'}`,
          'success'
        );
        await loadWorkspace();
      } catch (error) {
        setMessage((error as Error).message, 'error');
      } finally {
        childSaving.value = false;
      }
    };

    const openChildDetail = async (child: ChildAffairView) => {
      activeChild.value = child;
      childDetailVisible.value = true;
      dispositionNote.value = '';
      try {
        const svc = await ensureService();
        childTimeline.value = await svc.listTimeline(child.affairId);
      } catch (error) {
        setMessage(`子事务日志读取失败：${(error as Error).message}`, 'warning');
      }
    };

    const onSubmitChildComment = async () => {
      if (!activeChild.value) {
        return;
      }
      try {
        const svc = await ensureService();
        await svc.submitComment(activeChild.value.affairId, childCommentDraft.value);
        childCommentDraft.value = '';
        childTimeline.value = await svc.listTimeline(activeChild.value.affairId);
      } catch (error) {
        setMessage((error as Error).message, 'error');
      }
    };

    const onDisposition = async (action: DispositionAction) => {
      if (!activeChild.value || !selectedAffairId.value) {
        return;
      }
      try {
        const svc = await ensureService();
        const result = await svc.submitDisposition(selectedAffairId.value, activeChild.value.affairId, action, dispositionNote.value);
        ElMessage.success(
          result.resolutionOpHash
            ? `处置决议已签名入日志并进入公示期（${DISPOSITION_LABELS[action]}；期间徽标显示「待确认」）`
            : `处置操作已签名入日志（${DISPOSITION_LABELS[action]}${activeChild.value.type === 'pr' ? '，回执即生效' : ''}）`
        );
        childDetailVisible.value = false;
        await loadWorkspace();
      } catch (error) {
        setMessage((error as Error).message, 'error');
      }
    };

    // ------------------------------------------------------------------
    // 看板 / 发布 / 文档 / 成员
    // ------------------------------------------------------------------

    const onInitBoard = async () => {
      if (!selectedAffairId.value || !currentProject.value) {
        return;
      }
      boardInitializing.value = true;
      try {
        const svc = await ensureService();
        const result = await svc.initProjectBoard(
          selectedAffairId.value,
          spaceId.value,
          currentRootId.value ?? '',
          isPersonal.value ? 'admin' : currentOrgRole.value,
          currentProject.value.title
        );
        if (!result.ok) {
          boardInitError.value = `初始化被拒：${result.reason}`;
          return;
        }
        await loadBoard();
      } catch (error) {
        boardInitError.value = (error as Error).message;
      } finally {
        boardInitializing.value = false;
      }
    };

    const onInitReleaseConfig = async () => {
      if (!selectedOrgId.value || !currentRootId.value) {
        return;
      }
      try {
        const svc = await ensureService();
        await svc.initReleaseConfig(selectedOrgId.value, currentRootId.value, currentOrgRole.value);
        setMessage('发布权配置已初始化（发布者 = 当前身份）', 'success');
        await loadReleases();
      } catch (error) {
        setMessage((error as Error).message, 'error');
      }
    };

    const onRegisterRelease = async () => {
      if (!selectedOrgId.value || !currentRootId.value) {
        return;
      }
      releaseSaving.value = true;
      try {
        const svc = await ensureService();
        await svc.registerRelease(selectedOrgId.value, currentRootId.value, {
          pluginId: releaseDraft.value.pluginId,
          version: releaseDraft.value.version,
          ...(releaseDraft.value.updateManifestJson.trim()
            ? { updateManifestJson: releaseDraft.value.updateManifestJson }
            : {}),
          ...(releaseDraft.value.changelog.trim() ? { changelog: releaseDraft.value.changelog } : {}),
          ...(releaseDraft.value.decisionRef.trim() ? { decisionRef: releaseDraft.value.decisionRef } : {})
        });
        releaseDialogVisible.value = false;
        setMessage('发布单已登记（包哈希随发布单入存证链；核验通过后方可推进发布）', 'success');
        await loadReleases();
      } catch (error) {
        setMessage((error as Error).message, 'error');
      } finally {
        releaseSaving.value = false;
      }
    };

    const openVerifyDialog = (releaseId: string) => {
      verifyTargetId.value = releaseId;
      verifySpkgPath.value = '';
      verifyDialogVisible.value = true;
    };

    const onVerifyRelease = async () => {
      if (!selectedOrgId.value || !currentRootId.value || !verifyTargetId.value) {
        return;
      }
      verifySaving.value = true;
      try {
        const svc = await ensureService();
        const event = await svc.verifyRelease(
          selectedOrgId.value,
          currentRootId.value,
          verifyTargetId.value,
          verifySpkgPath.value
        );
        verifyDialogVisible.value = false;
        setMessage(
          event.type === 'verified' ? '核验通过（证据已入存证链），可推进发布' : `核验未通过：${event.reason ?? '详见核验失败事件'}`,
          event.type === 'verified' ? 'success' : 'warning'
        );
        await loadReleases();
      } catch (error) {
        setMessage((error as Error).message, 'error');
      } finally {
        verifySaving.value = false;
      }
    };

    const onPublishRelease = async (releaseId: string) => {
      if (!selectedOrgId.value || !currentRootId.value) {
        return;
      }
      try {
        const svc = await ensureService();
        await svc.publishRelease(selectedOrgId.value, currentRootId.value, releaseId);
        setMessage('已推进发布（版本卡片由发布管理件推送）', 'success');
        await loadReleases();
      } catch (error) {
        setMessage((error as Error).message, 'error');
      }
    };

    const openDocDialog = (docId?: string) => {
      docDraft.value = { ...(docId ? { docId } : {}), title: '', body: '' };
      docDialogVisible.value = true;
    };

    const onSaveDoc = async () => {
      if (!selectedAffairId.value) {
        return;
      }
      docSaving.value = true;
      try {
        const svc = await ensureService();
        await svc.saveDoc(selectedAffairId.value, docDraft.value);
        docDialogVisible.value = false;
        docs.value = await svc.listDocs(selectedAffairId.value);
      } catch (error) {
        setMessage((error as Error).message, 'error');
      } finally {
        docSaving.value = false;
      }
    };

    const openDocHistory = async (docId: string) => {
      if (!selectedAffairId.value) {
        return;
      }
      const svc = await ensureService();
      docHistoryList.value = await svc.getDocHistory(selectedAffairId.value, docId);
      docHistoryVisible.value = true;
    };

    const onShowProfile = async (identity: string) => {
      profile.value = null;
      profileVisible.value = true;
      try {
        const svc = await ensureService();
        profile.value = await svc.getPublicProfile(identity);
      } catch {
        profile.value = null;
      }
    };

    // ------------------------------------------------------------------
    // 呈现助手
    // ------------------------------------------------------------------

    const timelineKindLabel = (kind: TimelineEntry['kind']): string =>
      ({ comment: '发言', 'child-notice': '通告', disposition: '处置', resolution: '决议', other: '操作' })[kind];

    const dispositionLabel = (state: ChildAffairView['disposition']['state']): string =>
      state === 'open' ? '开放' : DISPOSITION_LABELS[state];

    const formatAge = (ageMs: number | null): string => {
      if (ageMs === null) {
        return '无链上记录';
      }
      const days = Math.floor(ageMs / 86400000);
      return days > 0 ? `${days} 天` : '不足 1 天';
    };

    // ------------------------------------------------------------------
    // 生命周期与订阅（档二-4 降级：加载时补发 + 节流）
    // ------------------------------------------------------------------

    let subscribed = false;

    onMounted(async () => {
      await reloadAll();
      if (!affairsReady.value) {
        return;
      }
      const svc = await ensureService();
      if (!subscribed) {
        subscribed = true;
        await svc.subscribeAffairChanges(() => {
          // 变更通知非可靠队列：重读收敛而非增量套用
          void loadProjects()
            .then(() => (selectedAffairId.value ? loadWorkspace() : undefined))
            .catch(() => undefined);
        });
        await svc.subscribeDataChanges(() => {
          void loadWorkspace().catch(() => undefined);
        });
      }
      // 加载时补发：子事务处置卡片（本机）+ 组织空间版本卡片（发布管理件推送源）
      void svc.backfillDispositionCards().catch(() => undefined);
      if (!isPersonal.value && selectedOrgId.value) {
        void svc.backfillReleaseCards(selectedOrgId.value, orgAdminRootIds.value).catch(() => undefined);
      }
    });

    watch(activeTab, (tab) => {
      if (tab === 'board') {
        void loadBoard().catch((error) => setMessage(`看板加载失败：${(error as Error).message}`, 'warning'));
      }
      if (tab === 'members') {
        void loadMembers().catch((error) => setMessage(`成员名册加载失败：${(error as Error).message}`, 'warning'));
      }
      if (tab === 'release') {
        void loadReleases().catch((error) => setMessage(`发布区加载失败：${(error as Error).message}`, 'warning'));
      }
    });

    watch(selectedOrgId, () => {
      boardView.value = null;
      void loadReleases().catch(() => undefined);
      if (activeTab.value === 'board') {
        void loadBoard().catch(() => undefined);
      }
    });

    onUnmounted(() => {
      subscribed = false;
    });

    return {
      AFFAIRS_MISSING_TEXT,
      RESOLUTION_BADGE_LABELS,
      message,
      messageType,
      affairsReady,
      readonlySpace,
      isPersonal,
      projects,
      selectedAffairId,
      currentProject,
      maintainers,
      activeTab,
      timeline,
      commentDraft,
      commenting,
      children,
      childTypeFilter,
      filteredChildren,
      openChildCount,
      childDialogVisible,
      childSaving,
      childDraft,
      childDetailVisible,
      activeChild,
      childTimeline,
      childCommentDraft,
      dispositionNote,
      boardView,
      boardInitializing,
      boardInitError,
      releaseRows,
      releaseConfig,
      releaseMarketAvailable,
      evidence,
      releaseDialogVisible,
      releaseSaving,
      releaseDraft,
      verifyDialogVisible,
      verifySaving,
      verifySpkgPath,
      isOrgAdmin,
      docs,
      docDialogVisible,
      docSaving,
      docDraft,
      docHistoryVisible,
      docHistoryList,
      members,
      profileVisible,
      profile,
      createDialogVisible,
      creating,
      createDraft,
      followDialogVisible,
      followGenesisJson,
      following,
      selectProject,
      openCreateDialog,
      onCreateProject,
      onFollow,
      onUnfollow,
      onSubmitComment,
      openChildDialog,
      onCreateChild,
      openChildDetail,
      onSubmitChildComment,
      onDisposition,
      onInitBoard,
      onInitReleaseConfig,
      onRegisterRelease,
      openVerifyDialog,
      onVerifyRelease,
      onPublishRelease,
      openDocDialog,
      onSaveDoc,
      openDocHistory,
      onShowProfile,
      timelineKindLabel,
      dispositionLabel,
      formatAge
    };
  }
});
</script>

<style scoped>
.project-root {
  height: 100%;
  display: flex;
  flex-direction: column;
  padding: 12px;
  box-sizing: border-box;
}
.message-bar {
  margin-bottom: 8px;
}
.layout {
  flex: 1;
  display: flex;
  gap: 12px;
  min-height: 0;
}
.sidebar {
  width: 240px;
  flex: none;
  display: flex;
  flex-direction: column;
  border-right: 1px solid var(--el-border-color-lighter, #e4e7ed);
  padding-right: 12px;
}
.sidebar-actions {
  display: flex;
  gap: 8px;
  margin-bottom: 8px;
}
.readonly-hint {
  font-size: 12px;
  color: var(--el-color-warning, #e6a23c);
  margin: 0 0 8px;
}
.project-list {
  flex: 1;
}
.project-item {
  padding: 8px;
  border-radius: 6px;
  cursor: pointer;
  margin-bottom: 4px;
}
.project-item:hover {
  background: var(--el-fill-color-light, #f5f7fa);
}
.project-item.active {
  background: var(--el-color-primary-light-9, #ecf5ff);
}
.project-item-title {
  display: flex;
  align-items: center;
  gap: 6px;
  font-size: 13px;
  font-weight: 600;
}
.project-item-sub {
  font-size: 12px;
  color: var(--el-text-color-secondary, #909399);
  margin-top: 2px;
}
.workspace {
  flex: 1;
  min-width: 0;
  display: flex;
  flex-direction: column;
}
.workspace-title {
  margin: 0;
  font-size: 18px;
  display: flex;
  align-items: center;
  gap: 8px;
}
.workspace-summary {
  margin: 6px 0;
  color: var(--el-text-color-regular, #606266);
  font-size: 13px;
}
.workspace-meta {
  display: flex;
  align-items: center;
  gap: 6px;
  flex-wrap: wrap;
}
.meta-text {
  font-size: 12px;
  color: var(--el-text-color-secondary, #909399);
}
.workspace-tabs {
  flex: 1;
  min-height: 0;
}
.tab-scroll {
  max-height: calc(100vh - 320px);
}
.tab-toolbar {
  display: flex;
  align-items: center;
  gap: 10px;
  margin-bottom: 10px;
}
.tab-note {
  font-size: 12px;
  color: var(--el-text-color-secondary, #909399);
}
.tab-note.warn {
  color: var(--el-color-warning, #e6a23c);
}
.timeline-item {
  display: flex;
  align-items: baseline;
  gap: 8px;
  padding: 6px 0;
  border-bottom: 1px dashed var(--el-border-color-lighter, #e4e7ed);
  font-size: 13px;
}
.timeline-kind {
  flex: none;
  font-size: 12px;
  color: var(--el-color-primary, #409eff);
}
.timeline-kind.kind-disposition {
  color: var(--el-color-success, #67c23a);
}
.timeline-kind.kind-resolution {
  color: var(--el-color-warning, #e6a23c);
}
.timeline-text {
  flex: 1;
  white-space: pre-wrap;
  word-break: break-word;
}
.timeline-actor {
  flex: none;
  font-size: 12px;
  color: var(--el-text-color-secondary, #909399);
}
.comment-box {
  display: flex;
  gap: 8px;
  margin-top: 10px;
  align-items: flex-end;
}
.children-table {
  width: 100%;
}
.board-columns {
  display: flex;
  gap: 10px;
  overflow-x: auto;
}
.board-column {
  flex: 1;
  min-width: 150px;
  background: var(--el-fill-color-lighter, #fafafa);
  border-radius: 6px;
  padding: 8px;
}
.board-column-title {
  font-size: 13px;
  font-weight: 600;
  margin-bottom: 8px;
  display: flex;
  align-items: center;
  gap: 6px;
}
.board-count {
  font-size: 12px;
  color: var(--el-text-color-secondary, #909399);
}
.board-card {
  background: var(--el-bg-color, #fff);
  border: 1px solid var(--el-border-color-lighter, #e4e7ed);
  border-radius: 4px;
  padding: 6px 8px;
  margin-bottom: 6px;
  font-size: 12px;
  display: flex;
  gap: 6px;
  align-items: center;
}
.board-empty {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 8px;
}
.mono {
  font-family: monospace;
  font-size: 12px;
}
.doc-item {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 8px 0;
  border-bottom: 1px dashed var(--el-border-color-lighter, #e4e7ed);
}
.doc-title {
  font-weight: 600;
  font-size: 13px;
}
.doc-sub {
  font-size: 12px;
  color: var(--el-text-color-secondary, #909399);
}
.doc-title-input {
  margin-bottom: 10px;
}
.doc-version {
  margin-bottom: 16px;
}
.doc-version-head {
  display: flex;
  gap: 8px;
  align-items: baseline;
}
.doc-body {
  white-space: pre-wrap;
  word-break: break-word;
  font-size: 12px;
  background: var(--el-fill-color-lighter, #fafafa);
  padding: 8px;
  border-radius: 4px;
  max-height: 300px;
  overflow: auto;
}
.detail-summary {
  white-space: pre-wrap;
  word-break: break-word;
  font-size: 13px;
}
.disposition-actions {
  display: flex;
  gap: 8px;
  align-items: center;
}
.code-gap-title {
  font-size: 14px;
  font-weight: 600;
  margin: 0 0 8px;
}
</style>

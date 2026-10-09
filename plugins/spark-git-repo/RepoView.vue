<!--
  代码仓库应用（spark-git-repo）· 主视图。

  分区（git-repo.md §4 + §7 MVP）：
  - 仓库：镜像版本/同步状态（持有即做种副本健康如实呈现）、分支切换、提交历史、
    文件树与内容、提交 diff——纯 JS 解析镜像对象，桌面/移动同码（移动端只读可用）；
  - PR：已知 PR 列表（本机关注的子事务）、详情时间线、评审/评论、发起/修订/
    合并/关闭（桌面写路径；移动端隐藏入口 +「在桌面端继续」引导）；
  - 发布：维护者发布镜像新版本（桌面；逐对象散 blob + 清单操作）；
  - 设置：项目议题绑定（关注列表选择 / 创世记录粘贴）、身份与能力声明。

  诚实口径：
  - 操作者身份 = 本插件域身份 id（平台暂无个人身份签名面，同 spark-affairs）；
  - PR 发现 = 本机关注集扫描（SDK 无子事务发现面）——他机 PR 经卡片/创世转发
    关注后出现；附件全部 provider 不可达时如实标注「暂不可拉取」；
  - git CLI 缺失：浏览不受影响，写路径给安装指引，不静默失败。
-->
<template>
  <section class="spark-git-repo">
    <el-alert v-if="unavailable" type="warning" :closable="false" show-icon class="message" :title="unavailable" />

    <template v-else>
      <el-alert v-if="message" :title="message" :type="messageType" :closable="false" show-icon class="message" />

      <el-card shadow="never" class="header-card">
        <div class="header-row">
          <div>
            <p class="eyebrow">代码仓库</p>
            <h2>{{ binding?.repoName || mirror?.manifest.repo || '未绑定项目议题' }}</h2>
            <p class="lede">
              只读镜像浏览 + PR 子事务协作流。镜像清单 = 项目议题内签名事务操作；
              合并 = 单维护者回执即生效、禁止非快进写回。
              <template v-if="caps.mobileReadonly">（移动端只读形态）</template>
            </p>
          </div>
          <div class="header-side">
            <el-tag v-if="caps.desktopWrite" :type="gitCli.available ? 'success' : 'warning'" size="small">
              git CLI {{ gitCli.available ? gitCli.version ?? '可用' : '未检测到' }}
            </el-tag>
            <el-tag v-else type="info" size="small">只读</el-tag>
            <el-button size="small" @click="reloadAll" :loading="loading">刷新</el-button>
          </div>
        </div>
      </el-card>

      <el-alert v-if="caps.desktopWrite && !gitCli.available" type="info" :closable="false" show-icon class="message"
        title="未检测到本地 git CLI：浏览不受影响；发起 PR / 合并 / 发布 / 物化需要先安装 git 并重启应用。"
      />

      <el-empty v-if="!binding" description="尚未绑定项目议题——到「设置」选择或关注项目议题">
        <el-button type="primary" @click="tab = 'settings'">去绑定</el-button>
      </el-empty>

      <el-tabs v-else v-model="tab" class="main-tabs">
        <!-- ============ 仓库浏览 ============ -->
        <el-tab-pane label="仓库" name="repo">
          <el-empty v-if="!mirror" description="项目议题还没有镜像清单——等待维护者发布首个镜像版本" />
          <template v-else>
            <el-card shadow="never" class="block">
              <div class="mirror-line">
                <span>镜像 <strong>v{{ mirror.manifest.version }}</strong></span>
                <span>默认分支 <el-tag size="small">{{ mirror.manifest.defaultBranch }}</el-tag></span>
                <span>对象 {{ mirror.status.local }}/{{ mirror.status.total }} 在本地</span>
                <el-tag v-if="storeMissing.length > 0" type="warning" size="small">
                  {{ storeMissing.length }} 个对象暂不可拉取
                </el-tag>
                <el-tag v-if="mirror.conflicts > 0" type="danger" size="small">
                  发现 {{ mirror.conflicts }} 个同版本冲突清单（串行合并约定被违背的客观证据）
                </el-tag>
                <span v-if="mirror.manifest.importHead" class="muted">
                  初始导入 {{ mirror.manifest.importHead.slice(0, 12) }}…
                </span>
              </div>
              <div class="mirror-line">
                <span>分支：</span>
                <el-select v-model="currentBranch" size="small" style="width: 200px" @change="loadHistory">
                  <el-option v-for="branch in mirror.manifest.branches" :key="branch.name"
                    :label="`${branch.name} (${branch.head.slice(0, 8)})`" :value="branch.name" />
                </el-select>
                <el-button v-if="caps.desktopWrite && gitCli.available" size="small" @click="doMaterialize" :loading="busy">
                  物化工作区（clone）
                </el-button>
              </div>
              <el-progress v-if="progress" :percentage="progress?.percent ?? 0" :format="() => progress?.text ?? ''" />
            </el-card>

            <el-tabs v-model="repoTab" class="sub-tabs">
              <el-tab-pane label="提交历史" name="commits">
                <el-table :data="commits" size="small" class="commit-table" @row-click="selectCommit">
                  <el-table-column label="commit" width="110">
                    <template #default="{ row }"><code>{{ row.sha.slice(0, 8) }}</code></template>
                  </el-table-column>
                  <el-table-column prop="subject" label="提交说明" show-overflow-tooltip />
                  <el-table-column prop="author" label="作者" width="160" show-overflow-tooltip />
                  <el-table-column label="时间" width="170">
                    <template #default="{ row }">{{ formatTime(row.committerTimeMs) }}</template>
                  </el-table-column>
                </el-table>
                <el-empty v-if="commits.length === 0" description="该分支历史暂不可读（镜像对象未同步）" />
              </el-tab-pane>

              <el-tab-pane label="文件" name="files">
                <div class="breadcrumb">
                  <el-breadcrumb separator="/">
                    <el-breadcrumb-item @click="navigateTree('')">根目录</el-breadcrumb-item>
                    <el-breadcrumb-item v-for="(seg, i) in treePathSegs" :key="i"
                      @click="navigateTree(treePathSegs.slice(0, i + 1).join('/'))">{{ seg }}</el-breadcrumb-item>
                  </el-breadcrumb>
                </div>
                <el-table v-if="!currentFile" :data="treeEntries" size="small" @row-click="openTreeEntry">
                  <el-table-column width="50">
                    <template #default="{ row }">{{ row.type === 'tree' ? '📁' : '📄' }}</template>
                  </el-table-column>
                  <el-table-column prop="name" label="名称" />
                  <el-table-column label="mode" width="100">
                    <template #default="{ row }"><code>{{ row.mode }}</code></template>
                  </el-table-column>
                </el-table>
                <div v-else class="file-view">
                  <div class="file-head">
                    <el-button size="small" text @click="currentFile = null">← 返回目录</el-button>
                    <code>{{ treePath }}</code>
                    <span class="muted">{{ currentFile.size }} 字节</span>
                  </div>
                  <el-alert v-if="currentFile.binary" type="info" :closable="false" title="二进制文件，不提供在线预览" />
                  <pre v-else class="file-content"><code><span v-for="(line, i) in currentFileLines" :key="i" class="code-line"><span class="line-no">{{ i + 1 }}</span>{{ line }}
</span></code></pre>
                </div>
              </el-tab-pane>

              <el-tab-pane label="提交 diff" name="diff">
                <el-empty v-if="!selectedCommit" description="在「提交历史」中点击一个提交查看 diff" />
                <template v-else>
                  <p class="muted">
                    <code>{{ selectedCommit.sha.slice(0, 12) }}</code> {{ selectedCommit.subject }}
                    <template v-if="selectedCommit.parents.length > 1">（合并提交，对首个父提交 diff）</template>
                  </p>
                  <div v-for="change in commitChanges" :key="change.path" class="diff-file">
                    <div class="diff-head">
                      <el-tag size="small" :type="change.change === 'added' ? 'success' : change.change === 'removed' ? 'danger' : 'warning'">
                        {{ change.change === 'added' ? '新增' : change.change === 'removed' ? '删除' : '修改' }}
                      </el-tag>
                      <code>{{ change.path }}</code>
                    </div>
                    <el-tag v-if="isDiffOversized(change)" type="info" size="small">diff 过大，不展开（文本超过 256KB 护栏）</el-tag>
                    <pre v-else-if="diffOf(change)" class="diff-content"><code><span v-for="(row, i) in diffOf(change)" :key="i" class="code-line" :data-kind="row.type">{{ row.type === 'gap' ? `… 折叠 ${row.count} 行 …` : row.text }}
</span></code></pre>
                  </div>
                  <el-empty v-if="commitChanges.length === 0" description="无文件变更（或对象未同步）" />
                </template>
              </el-tab-pane>
            </el-tabs>
          </template>
        </el-tab-pane>

        <!-- ============ PR ============ -->
        <el-tab-pane label="PR" name="prs">
          <div class="block pr-actions">
            <el-button v-if="caps.desktopWrite && gitCli.available" type="primary" size="small" @click="prFormOpen = true">
              发起 PR
            </el-button>
            <span v-else-if="caps.mobileReadonly || !caps.desktopWrite" class="muted">
              移动端只读：发起 PR 请在桌面端继续
            </span>
            <el-button size="small" @click="loadPrs" :loading="loading">刷新列表</el-button>
          </div>

          <el-table :data="prs" size="small" @row-click="selectPr">
            <el-table-column label="状态" width="90">
              <template #default="{ row }">
                <el-tag size="small" :type="row.state.status === 'open' ? 'primary' : row.state.status === 'merged' ? 'success' : 'danger'">
                  {{ row.state.status === 'open' ? '开放' : row.state.status === 'merged' ? '已合并' : '已关闭' }}
                </el-tag>
              </template>
            </el-table-column>
            <el-table-column label="标题">
              <template #default="{ row }">{{ row.state.open?.title ?? '(元数据未同步)' }}</template>
            </el-table-column>
            <el-table-column label="base" width="110">
              <template #default="{ row }">{{ row.state.open?.base ?? '?' }}</template>
            </el-table-column>
            <el-table-column label="评审/评论" width="110">
              <template #default="{ row }">{{ row.state.reviews.length }}/{{ row.state.comments.length }}</template>
            </el-table-column>
          </el-table>
          <el-empty v-if="prs.length === 0"
            description="本机没有已关注的 PR 子事务——他机发起的 PR 经卡片/创世转发关注后出现在这里" />

          <el-drawer v-model="prDetailOpen" :size="drawerSize" :title="prDetail?.open?.title ?? 'PR 详情'">
            <template v-if="prDetail">
              <el-descriptions :column="1" size="small" border>
                <el-descriptions-item label="状态">
                  {{ prDetail.status === 'open' ? '开放' : prDetail.status === 'merged' ? '已合并' : '已关闭' }}
                </el-descriptions-item>
                <el-descriptions-item label="base / head">
                  {{ prDetail.open?.base }} / <code>{{ prDetail.currentHead?.slice(0, 12) }}…</code>
                </el-descriptions-item>
                <el-descriptions-item label="描述">{{ prDetail.open?.description || '（无）' }}</el-descriptions-item>
                <el-descriptions-item v-if="prDetail.merged" label="合并回执">
                  结果 <code>{{ prDetail.merged.resultCommit.slice(0, 12) }}…</code> → 镜像 v{{ prDetail.merged.mirrorVersion }}
                  <div v-if="receiptCheck">
                    <el-tag :type="receiptCheck.ok ? 'success' : 'danger'" size="small">
                      {{ receiptCheck.ok ? `已核验：commit 在分支 ${receiptCheck.checkedBranch} 历史中` : receiptCheck.reason }}
                    </el-tag>
                  </div>
                  <div v-else class="muted">回执所指镜像版本未同步到本机，暂不能独立核验</div>
                </el-descriptions-item>
                <el-descriptions-item v-if="prDetail.closed" label="关闭理由">{{ prDetail.closed.reason }}</el-descriptions-item>
              </el-descriptions>

              <h4>附件与本地检出</h4>
              <div v-for="(att, i) in prDetail.currentAttachments" :key="i" class="attach-row">
                <el-tag size="small">{{ att.kind }}</el-tag>
                <code>{{ att.cid.slice(0, 16) }}…</code>
                <span class="muted">{{ formatSize(att.size) }}</span>
                <el-tag v-if="att.size > ATTACH_WARN" type="warning" size="small">超过 10MB 提示阈值，建议拆分</el-tag>
                <el-button size="small" text @click="copyFetchCmd">复制 git fetch 指引</el-button>
              </div>
              <p v-if="attachmentNote" class="muted">{{ attachmentNote }}</p>

              <h4>评审与讨论</h4>
              <el-timeline class="pr-timeline">
                <el-timeline-item v-for="item in prDetail.timeline" :key="item.opHash" :timestamp="formatTime(item.declaredAt)">
                  <strong>{{ kindLabel(item.kind) }}</strong>
                  <span class="muted"> · {{ item.actor.slice(0, 12) }}…</span>
                  <el-tag v-if="item.rejectedReason" type="danger" size="small">{{ item.rejectedReason }}</el-tag>
                  <div class="timeline-body">{{ timelineText(item) }}</div>
                </el-timeline-item>
              </el-timeline>

              <template v-if="prDetail.status === 'open'">
                <el-form label-position="top" class="review-form">
                  <el-form-item label="评论 / 评审意见">
                    <el-input v-model="reviewText" type="textarea" :rows="3" maxlength="4000" />
                  </el-form-item>
                  <div class="actions">
                    <el-button size="small" @click="doComment" :loading="busy">发表评论</el-button>
                    <el-button size="small" type="success" @click="doReview('approve')" :loading="busy">通过</el-button>
                    <el-button size="small" type="warning" @click="doReview('request-changes')" :loading="busy">要求修改</el-button>
                  </div>
                </el-form>
                <div v-if="caps.desktopWrite && gitCli.available" class="actions maintainer-actions">
                  <el-button size="small" @click="updateFormOpen = true">修订 PR（追加新附件）</el-button>
                  <el-button size="small" type="primary" @click="mergeFormOpen = true">维护者合并</el-button>
                  <el-button size="small" type="danger" @click="closeFormOpen = true">关闭 PR</el-button>
                </div>
                <p v-else class="muted">修订 / 合并 / 关闭为桌面写路径——请在桌面端继续。</p>
                <p class="hint">
                  合并纪律（档一-3）：同一时刻只有一名维护者执行合并写回（串行靠社区纪律 + 本提示）；
                  合并前请确认权威仓库已同步到最新镜像 head；非快进写回一律被拒绝。
                </p>
              </template>
            </template>
          </el-drawer>
        </el-tab-pane>

        <!-- ============ 发布镜像（维护者，桌面） ============ -->
        <el-tab-pane v-if="caps.desktopWrite" label="发布镜像" name="publish">
          <el-card shadow="never" class="block">
            <template #header><h3>发布镜像新版本（维护者）</h3></template>
            <el-form label-position="top">
              <el-form-item label="权威仓库本地目录（唯一写点）">
                <div class="dir-row">
                  <el-input v-model="publishForm.repoDir" placeholder="选择本地 git 仓库目录" />
                  <el-button size="small" @click="pickDir('publish')">选择目录</el-button>
                </div>
              </el-form-item>
              <el-form-item label="仓库名">
                <el-input v-model="publishForm.repoName" maxlength="120" />
              </el-form-item>
              <el-form-item label="版本说明（可选）">
                <el-input v-model="publishForm.note" maxlength="500" />
              </el-form-item>
              <p class="hint">
                发布 = 枚举发布分支可达的 git 对象（不可达对象 / stash / 未发布私有 ref 不进入分发面）
                → 逐对象 saveBlob 入内容面（保存即声明 provider；上一版已有对象增量跳过）
                → 项目议题提交 git.mirror.manifest 操作（版本 {{ (mirror?.manifest.version ?? 0) + 1 }}）。
                首版将把当前默认分支 head 作为一次性导入哈希记入清单（档一-4）。
              </p>
              <el-button type="primary" @click="doPublish" :loading="busy" :disabled="!gitCli.available">发布</el-button>
              <el-progress v-if="progress" :percentage="progress?.percent ?? 0" :format="() => progress?.text ?? ''" />
            </el-form>
          </el-card>
        </el-tab-pane>

        <!-- ============ 设置 ============ -->
        <el-tab-pane label="设置" name="settings">
          <el-card shadow="never" class="block">
            <template #header><h3>项目议题绑定（一议题一仓库，档三-8）</h3></template>
            <el-form label-position="top">
              <el-form-item label="从本机关注的议题中选择">
                <el-select v-model="settingsAffairId" filterable style="width: 100%" placeholder="选择项目议题">
                  <el-option v-for="topic in followedTopics" :key="topic.affairId"
                    :label="`${topic.title} (${topic.affairId.slice(0, 8)}…)`" :value="topic.affairId" />
                </el-select>
              </el-form-item>
              <el-form-item label="仓库名（展示用）">
                <el-input v-model="settingsRepoName" maxlength="120" placeholder="如 spark" />
              </el-form-item>
              <el-button type="primary" size="small" @click="saveBinding" :disabled="!settingsAffairId">保存绑定</el-button>
            </el-form>
            <el-divider />
            <el-form label-position="top">
              <el-form-item label="按创世记录关注议题（粘贴 JSON 原文，affairId 自认证复算）">
                <el-input v-model="genesisText" type="textarea" :rows="4" placeholder='{"affairV":1,...}' />
              </el-form-item>
              <el-button size="small" @click="doFollow" :loading="busy">关注</el-button>
            </el-form>
          </el-card>
          <el-card shadow="never" class="block">
            <template #header><h3>身份与能力</h3></template>
            <p class="muted">
              本机操作者身份（插件域身份 id，平台暂无个人身份签名面）：
              <code>{{ service?.viewerIdentity ?? '（首次写操作时生成）' }}</code>
            </p>
            <p class="muted">
              能力：affairs {{ caps.affairs ? '✓' : '✗' }} · content {{ caps.content ? '✓' : '✗' }} ·
              桌面写路径 {{ caps.desktopWrite ? '✓' : '✗' }} · 消息 {{ caps.messages ? '✓' : '✗' }}
            </p>
          </el-card>
        </el-tab-pane>
      </el-tabs>
    </template>

    <!-- 发起 PR -->
    <el-dialog v-model="prFormOpen" title="发起 PR" width="560px">
      <el-form label-position="top">
        <el-form-item label="本地仓库目录（你的 clone）">
          <div class="dir-row">
            <el-input v-model="prForm.repoDir" placeholder="选择本地 git 仓库目录" />
            <el-button size="small" @click="pickDir('pr')">选择目录</el-button>
          </div>
        </el-form-item>
        <el-form-item label="改动分支">
          <el-select v-model="prForm.branch" style="width: 100%" :loading="branchesLoading" placeholder="选择目录后加载本地分支">
            <el-option v-for="b in localBranches" :key="b.name" :label="`${b.name} (${b.head.slice(0, 8)})`" :value="b.name" />
          </el-select>
        </el-form-item>
        <el-form-item label="base 分支（镜像清单）">
          <el-select v-model="prForm.base" style="width: 100%">
            <el-option v-for="b in mirror?.manifest.branches ?? []" :key="b.name" :label="b.name" :value="b.name" />
          </el-select>
        </el-form-item>
        <el-form-item label="标题"><el-input v-model="prForm.title" maxlength="120" show-word-limit /></el-form-item>
        <el-form-item label="描述"><el-input v-model="prForm.description" type="textarea" :rows="4" maxlength="8000" /></el-form-item>
        <p class="hint">提交 = 生成 git bundle（base..分支）→ saveBlob 入内容面 → 创建 PR 子事务（parent → 项目议题）→ pr.open 签名操作。过程状态可见。</p>
      </el-form>
      <template #footer>
        <el-button @click="prFormOpen = false">取消</el-button>
        <el-button type="primary" @click="doOpenPr" :loading="busy">签名并提交</el-button>
      </template>
    </el-dialog>

    <!-- 修订 PR -->
    <el-dialog v-model="updateFormOpen" title="修订 PR（追加新附件）" width="520px">
      <el-form label-position="top">
        <el-form-item label="本地仓库目录">
          <div class="dir-row">
            <el-input v-model="updateForm.repoDir" />
            <el-button size="small" @click="pickDir('update')">选择目录</el-button>
          </div>
        </el-form-item>
        <el-form-item label="改动分支"><el-input v-model="updateForm.branch" /></el-form-item>
        <el-form-item label="修订说明"><el-input v-model="updateForm.note" type="textarea" :rows="3" maxlength="2000" /></el-form-item>
      </el-form>
      <template #footer>
        <el-button @click="updateFormOpen = false">取消</el-button>
        <el-button type="primary" @click="doUpdatePr" :loading="busy">提交修订</el-button>
      </template>
    </el-dialog>

    <!-- 维护者合并 -->
    <el-dialog v-model="mergeFormOpen" title="维护者合并" width="520px">
      <el-form label-position="top">
        <el-form-item label="权威仓库本地目录（须已同步到最新镜像 head）">
          <div class="dir-row">
            <el-input v-model="mergeForm.repoDir" />
            <el-button size="small" @click="pickDir('merge')">选择目录</el-button>
          </div>
        </el-form-item>
        <el-form-item label="回执说明（可选）"><el-input v-model="mergeForm.note" maxlength="500" /></el-form-item>
        <p class="hint">
          流程：拉回 bundle 附件 → 本地 fetch + 合并（先 --ff-only，退化三方合并；冲突将如实报错中止）
          → 快进校验 → 发布新镜像版本 → pr.merged 回执。任一步失败即中止，不产生半成品状态。
        </p>
      </el-form>
      <template #footer>
        <el-button @click="mergeFormOpen = false">取消</el-button>
        <el-button type="primary" @click="doMergePr" :loading="busy">执行合并</el-button>
      </template>
    </el-dialog>

    <!-- 关闭 PR -->
    <el-dialog v-model="closeFormOpen" title="关闭 PR" width="480px">
      <el-form-item label="关闭理由（入操作日志，不可收回）">
        <el-input v-model="closeReason" type="textarea" :rows="3" maxlength="2000" />
      </el-form-item>
      <template #footer>
        <el-button @click="closeFormOpen = false">取消</el-button>
        <el-button type="danger" @click="doClosePr" :loading="busy">确认关闭</el-button>
      </template>
    </el-dialog>
  </section>
</template>

<script setup lang="ts">
import { computed, onMounted, reactive, ref } from 'vue';
import { listLocalBranches, type ExecFn } from './git';
import {
  ATTACHMENT_WARN_BYTES,
  collapseContext,
  diffLines,
  diffOversized,
  type DisplayDiffRow,
  type FileChange,
  type GitObjectStore,
  type ParsedCommit,
  type PrReviewVerdict,
  type PrState,
  type PrTimelineItem,
  type TreeEntry
} from './model';
import {
  AFFAIRS_MODULE_MISSING,
  CONTENT_MODULE_MISSING,
  GitRepoService,
  probeCapabilities,
  type MirrorView,
  type PluginCapabilities,
  type PrSummaryView,
  type ProjectBinding
} from './service';

const ATTACH_WARN = ATTACHMENT_WARN_BYTES;

const unavailable = ref('');
const message = ref('');
const messageType = ref<'success' | 'warning' | 'error' | 'info'>('info');
const loading = ref(false);
const busy = ref(false);
const tab = ref('repo');
const repoTab = ref('commits');
const progress = ref<{ percent: number; text: string } | null>(null);

const caps = ref<PluginCapabilities>({ affairs: false, content: false, desktopWrite: false, mobileReadonly: false, messages: false });
const gitCli = ref<{ available: boolean; version: string | null }>({ available: false, version: null });

const service = ref<GitRepoService | null>(null);
const binding = ref<ProjectBinding | null>(null);
const mirror = ref<MirrorView | null>(null);
const objectStore = ref<GitObjectStore | null>(null);
const storeMissing = ref<string[]>([]);

const currentBranch = ref('');
const commits = ref<ParsedCommit[]>([]);
const selectedCommit = ref<ParsedCommit | null>(null);
const commitChanges = ref<FileChange[]>([]);
const treePath = ref('');
const treeEntries = ref<TreeEntry[]>([]);
const currentFile = ref<{ text: string | null; binary: boolean; size: number } | null>(null);

const prs = ref<PrSummaryView[]>([]);
const prDetail = ref<PrState | null>(null);
const prDetailOpen = ref(false);
const selectedPrId = ref('');
const receiptCheck = ref<{ ok: boolean; checkedBranch: string | null; reason: string | null } | null>(null);
const attachmentNote = ref('');
const reviewText = ref('');

const prFormOpen = ref(false);
const updateFormOpen = ref(false);
const mergeFormOpen = ref(false);
const closeFormOpen = ref(false);
const closeReason = ref('');
const prForm = reactive({ repoDir: '', branch: '', base: '', title: '', description: '' });
const updateForm = reactive({ repoDir: '', branch: '', note: '' });
const mergeForm = reactive({ repoDir: '', note: '' });
const publishForm = reactive({ repoDir: '', repoName: '', note: '' });
const localBranches = ref<Array<{ name: string; head: string }>>([]);
const branchesLoading = ref(false);

const followedTopics = ref<Array<{ affairId: string; title: string; type: string }>>([]);
const settingsAffairId = ref('');
const settingsRepoName = ref('');
const genesisText = ref('');

const drawerSize = computed(() => (window.innerWidth < 640 ? '100%' : '70%'));
const treePathSegs = computed(() => treePath.value.split('/').filter(Boolean));
const currentFileLines = computed(() => (currentFile.value?.text ?? '').split('\n'));

function sdk() {
  return window.__sparkPluginSDK ?? null;
}

function notify(text: string, type: typeof messageType.value = 'info'): void {
  message.value = text;
  messageType.value = type;
}

function formatTime(ms: number | null): string {
  if (!ms) {
    return '—';
  }
  return new Date(ms).toLocaleString();
}

function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}

function kindLabel(kind: string): string {
  const labels: Record<string, string> = {
    'pr.open': '发起 PR',
    'pr.update': '修订',
    'pr.comment': '评论',
    'pr.review': '评审',
    'pr.merged': '合并回执',
    'pr.closed': '关闭'
  };
  return labels[kind] ?? kind;
}

function timelineText(item: PrTimelineItem): string {
  const p = item.payload as Record<string, unknown>;
  if (item.kind === 'pr.open') return String(p.description ?? '');
  if (item.kind === 'pr.update') return `新 head ${String(p.head ?? '').slice(0, 12)}… ${p.note ? `· ${String(p.note)}` : ''}`;
  if (item.kind === 'pr.comment') return String(p.text ?? '');
  if (item.kind === 'pr.review') {
    const verdict = p.verdict === 'approve' ? '通过' : p.verdict === 'request-changes' ? '要求修改' : '评论';
    return `【${verdict}】${String(p.text ?? '')}`;
  }
  if (item.kind === 'pr.merged') return `结果 ${String(p.resultCommit ?? '').slice(0, 12)}… → 镜像 v${String(p.mirrorVersion ?? '?')}${p.note ? ` · ${String(p.note)}` : ''}`;
  if (item.kind === 'pr.closed') return String(p.reason ?? '');
  return '';
}

function blobTexts(change: FileChange): { oldText: string; newText: string } | null {
  const store = objectStore.value;
  const svc = service.value;
  if (!store || !svc) return null;
  const oldText = change.oldSha ? svc.readBlobText(store, change.oldSha) : null;
  const newText = change.newSha ? svc.readBlobText(store, change.newSha) : null;
  if ((oldText && oldText.binary) || (newText && newText.binary)) return null;
  return { oldText: oldText?.text ?? '', newText: newText?.text ?? '' };
}

function isDiffOversized(change: FileChange): boolean {
  const texts = blobTexts(change);
  return texts !== null && diffOversized(texts.oldText, texts.newText);
}

function diffOf(change: FileChange): DisplayDiffRow[] | null {
  if (change.change === 'added' && !change.newSha) return null;
  const texts = blobTexts(change);
  if (!texts || diffOversized(texts.oldText, texts.newText)) return null;
  const rows = diffLines(texts.oldText, texts.newText);
  return collapseContext(rows, 3);
}

// ------------------------------------------------------------------
// 装载
// ------------------------------------------------------------------

onMounted(async () => {
  const instance = sdk();
  if (!instance) {
    unavailable.value = '插件 SDK 未注入（非沙箱上下文）';
    return;
  }
  caps.value = probeCapabilities(instance);
  if (!caps.value.affairs) {
    unavailable.value = AFFAIRS_MODULE_MISSING;
    return;
  }
  if (!caps.value.content) {
    unavailable.value = CONTENT_MODULE_MISSING;
    return;
  }
  try {
    service.value = new GitRepoService(instance);
  } catch (error) {
    unavailable.value = (error as Error).message;
    return;
  }
  if (caps.value.desktopWrite) {
    gitCli.value = await service.value.gitCliStatus();
  }
  await service.value.subscribeChanges(() => {
    void reloadAll();
  }).catch(() => undefined);
  await reloadAll();
});

async function reloadAll(): Promise<void> {
  const svc = service.value;
  if (!svc) return;
  loading.value = true;
  try {
    followedTopics.value = await svc.listFollowedTopics();
    binding.value = await svc.getBinding();
    if (binding.value) {
      await loadMirror();
      await loadPrs();
    }
  } catch (error) {
    notify((error as Error).message, 'error');
  } finally {
    loading.value = false;
  }
}

async function loadMirror(): Promise<void> {
  const svc = service.value;
  const bound = binding.value;
  if (!svc || !bound) return;
  mirror.value = await svc.getMirrorView(bound.projectAffairId);
  if (!mirror.value) {
    objectStore.value = null;
    commits.value = [];
    return;
  }
  const { store, missing } = await svc.loadObjectStore(mirror.value.manifest, { fetch: true });
  objectStore.value = store;
  storeMissing.value = missing;
  if (missing.length > 0) {
    notify(`有 ${missing.length} 个镜像对象暂不可拉取（provider 不可达）——浏览内容可能不完整`, 'warning');
  }
  if (!currentBranch.value || !mirror.value.manifest.branches.some((b) => b.name === currentBranch.value)) {
    currentBranch.value = mirror.value.manifest.defaultBranch;
  }
  await loadHistory();
  await navigateTree('');
}

function branchHead(name: string): string | null {
  return mirror.value?.manifest.branches.find((b) => b.name === name)?.head ?? null;
}

async function loadHistory(): Promise<void> {
  const store = objectStore.value;
  const head = branchHead(currentBranch.value);
  if (!store || !head) {
    commits.value = [];
    return;
  }
  commits.value = service.value?.listHistory(store, head) ?? [];
}

function selectCommit(commit: ParsedCommit): void {
  selectedCommit.value = commit;
  commitChanges.value = objectStore.value && service.value ? service.value.commitChanges(objectStore.value, commit.sha) : [];
  repoTab.value = 'diff';
}

async function navigateTree(path: string): Promise<void> {
  const store = objectStore.value;
  const head = branchHead(currentBranch.value);
  if (!store || !head || !service.value) return;
  treePath.value = path;
  currentFile.value = null;
  treeEntries.value = service.value.listTree(store, head, path) ?? [];
}

function openTreeEntry(entry: TreeEntry): void {
  const path = treePath.value ? `${treePath.value}/${entry.name}` : entry.name;
  if (entry.type === 'tree') {
    void navigateTree(path);
    return;
  }
  const store = objectStore.value;
  const head = branchHead(currentBranch.value);
  if (!store || !head || !service.value) return;
  const file = service.value.readFile(store, head, path);
  currentFile.value = file ? { text: file.text, binary: file.binary, size: file.size } : { text: null, binary: false, size: 0 };
  treePath.value = path;
}

// ------------------------------------------------------------------
// PR
// ------------------------------------------------------------------

async function loadPrs(): Promise<void> {
  const svc = service.value;
  const bound = binding.value;
  if (!svc || !bound) return;
  prs.value = await svc.listKnownPrs(bound.projectAffairId);
}

async function selectPr(row: PrSummaryView): Promise<void> {
  const svc = service.value;
  const bound = binding.value;
  if (!svc || !bound) return;
  selectedPrId.value = row.affairId;
  prDetail.value = await svc.getPrDetail(row.affairId);
  receiptCheck.value = null;
  prDetailOpen.value = true;
  if (prDetail.value.status === 'merged') {
    receiptCheck.value = await svc.verifyMergedPr(bound.projectAffairId, prDetail.value);
  }
  attachmentNote.value = '';
}

async function refreshPrDetail(): Promise<void> {
  if (!selectedPrId.value || !service.value) return;
  prDetail.value = await service.value.getPrDetail(selectedPrId.value);
  await loadPrs();
}

async function copyFetchCmd(): Promise<void> {
  const att = prDetail.value?.currentAttachments[0];
  const head = prDetail.value?.currentHead;
  if (!att || !head) return;
  const text = `# 在权威仓库目录中：\n# 1) 从内容面取回附件（cid ${att.cid}）后保存为 pr.bundle\n# 2) git fetch ./pr.bundle ${head}:refs/pr/${selectedPrId.value.slice(0, 8)}\n# 3) git log ${prDetail.value?.open?.base}..refs/pr/${selectedPrId.value.slice(0, 8)}`;
  try {
    await navigator.clipboard.writeText(text);
    notify('检出指引已复制', 'success');
  } catch {
    attachmentNote.value = text;
  }
}

async function doComment(): Promise<void> {
  if (!service.value || !selectedPrId.value) return;
  busy.value = true;
  try {
    await service.value.commentPr(selectedPrId.value, reviewText.value);
    reviewText.value = '';
    notify('评论已提交', 'success');
    await refreshPrDetail();
  } catch (error) {
    notify((error as Error).message, 'error');
  } finally {
    busy.value = false;
  }
}

async function doReview(verdict: PrReviewVerdict): Promise<void> {
  if (!service.value || !selectedPrId.value) return;
  busy.value = true;
  try {
    await service.value.reviewPr(selectedPrId.value, verdict, reviewText.value || verdict);
    reviewText.value = '';
    notify('评审已提交', 'success');
    await refreshPrDetail();
  } catch (error) {
    notify((error as Error).message, 'error');
  } finally {
    busy.value = false;
  }
}

async function doOpenPr(): Promise<void> {
  const svc = service.value;
  const bound = binding.value;
  if (!svc || !bound) return;
  busy.value = true;
  try {
    const result = await svc.openPr({
      projectAffairId: bound.projectAffairId,
      repoDir: prForm.repoDir,
      branch: prForm.branch,
      base: prForm.base,
      title: prForm.title,
      description: prForm.description
    });
    prFormOpen.value = false;
    await svc.notifyPrCard({ affairId: result.prAffairId, title: prForm.title, status: '开放', base: prForm.base });
    notify(`PR 已创建（附件 ${formatSize(result.size)}${result.size > ATTACH_WARN ? '，超过 10MB 提示阈值，建议下次拆分' : ''}）`, 'success');
    await loadPrs();
  } catch (error) {
    notify((error as Error).message, 'error');
  } finally {
    busy.value = false;
  }
}

async function doUpdatePr(): Promise<void> {
  const svc = service.value;
  const bound = binding.value;
  if (!svc || !bound || !selectedPrId.value) return;
  busy.value = true;
  try {
    await svc.updatePr({
      prAffairId: selectedPrId.value,
      projectAffairId: bound.projectAffairId,
      repoDir: updateForm.repoDir,
      branch: updateForm.branch,
      note: updateForm.note
    });
    updateFormOpen.value = false;
    notify('修订已提交（append-only 入日志）', 'success');
    await refreshPrDetail();
  } catch (error) {
    notify((error as Error).message, 'error');
  } finally {
    busy.value = false;
  }
}

async function doMergePr(): Promise<void> {
  const svc = service.value;
  const bound = binding.value;
  if (!svc || !bound || !selectedPrId.value) return;
  busy.value = true;
  progress.value = { percent: 0, text: '准备合并' };
  try {
    const result = await svc.mergePr({
      prAffairId: selectedPrId.value,
      projectAffairId: bound.projectAffairId,
      repoDir: mergeForm.repoDir,
      note: mergeForm.note,
      onProgress: (stage) => {
        progress.value = { percent: progress.value?.percent ?? 0, text: stage };
      }
    });
    mergeFormOpen.value = false;
    await svc.notifyPrCard({
      affairId: selectedPrId.value,
      title: prDetail.value?.open?.title ?? '',
      status: '已合并',
      base: prDetail.value?.open?.base ?? ''
    });
    notify(`已合并：${result.resultCommit.slice(0, 12)}… → 镜像 v${result.mirrorVersion}`, 'success');
    await refreshPrDetail();
    await loadMirror();
  } catch (error) {
    notify((error as Error).message, 'error');
  } finally {
    busy.value = false;
    progress.value = null;
  }
}

async function doClosePr(): Promise<void> {
  if (!service.value || !selectedPrId.value) return;
  busy.value = true;
  try {
    await service.value.closePr(selectedPrId.value, closeReason.value);
    closeFormOpen.value = false;
    closeReason.value = '';
    notify('PR 已关闭', 'success');
    await refreshPrDetail();
  } catch (error) {
    notify((error as Error).message, 'error');
  } finally {
    busy.value = false;
  }
}

// ------------------------------------------------------------------
// 发布 / 物化 / 目录选择 / 绑定
// ------------------------------------------------------------------

async function doPublish(): Promise<void> {
  const svc = service.value;
  const bound = binding.value;
  if (!svc || !bound) return;
  busy.value = true;
  progress.value = { percent: 0, text: '枚举对象' };
  try {
    const result = await svc.publishMirror(bound.projectAffairId, {
      repoDir: publishForm.repoDir,
      repoName: publishForm.repoName || bound.repoName || 'repo',
      note: publishForm.note,
      onProgress: (done, total) => {
        progress.value = { percent: Math.round((done / Math.max(1, total)) * 100), text: `${done}/${total} 对象` };
      }
    });
    notify(`镜像 v${result.version} 已发布（${result.objectCount} 个对象）`, 'success');
    await loadMirror();
  } catch (error) {
    notify((error as Error).message, 'error');
  } finally {
    busy.value = false;
    progress.value = null;
  }
}

async function doMaterialize(): Promise<void> {
  const svc = service.value;
  const bound = binding.value;
  const instance = sdk();
  if (!svc || !bound || !instance?.sys) return;
  const target = await instance.sys.pickFolder('选择物化目标目录（须为已存在的空目录，将执行 git init + checkout）');
  if (!target) return;
  busy.value = true;
  progress.value = { percent: 0, text: '准备物化' };
  try {
    const result = await svc.materialize(bound.projectAffairId, target, (done, total) => {
      progress.value = { percent: Math.round((done / Math.max(1, total)) * 100), text: `${done}/${total} 对象` };
    });
    notify(`工作区已物化到 ${target}（${result.objectCount} 个对象，分支 ${result.defaultBranch}）`, 'success');
  } catch (error) {
    notify((error as Error).message, 'error');
  } finally {
    busy.value = false;
    progress.value = null;
  }
}

async function pickDir(target: 'pr' | 'update' | 'merge' | 'publish'): Promise<void> {
  const instance = sdk();
  if (!instance?.sys) return;
  const dir = await instance.sys.pickFolder('选择本地 git 仓库目录');
  if (!dir) return;
  if (target === 'pr') {
    prForm.repoDir = dir;
    await loadLocalBranches(dir);
  } else if (target === 'update') {
    updateForm.repoDir = dir;
  } else if (target === 'merge') {
    mergeForm.repoDir = dir;
  } else {
    publishForm.repoDir = dir;
  }
}

async function loadLocalBranches(dir: string): Promise<void> {
  const instance = sdk();
  if (!instance?.sys) return;
  branchesLoading.value = true;
  try {
    const exec: ExecFn = (program, args, workdir) => instance.sys!.exec(program, args, workdir);
    localBranches.value = await listLocalBranches(exec, dir);
  } catch {
    localBranches.value = [];
  } finally {
    branchesLoading.value = false;
  }
}

async function saveBinding(): Promise<void> {
  if (!service.value) return;
  busy.value = true;
  try {
    await service.value.bindProject(settingsAffairId.value, settingsRepoName.value.trim());
    notify('绑定已保存', 'success');
    await reloadAll();
    tab.value = 'repo';
  } catch (error) {
    notify((error as Error).message, 'error');
  } finally {
    busy.value = false;
  }
}

async function doFollow(): Promise<void> {
  if (!service.value) return;
  busy.value = true;
  try {
    const genesis = JSON.parse(genesisText.value) as unknown;
    const affairId = await service.value.followGenesis(genesis);
    genesisText.value = '';
    notify(`已关注议题 ${affairId.slice(0, 12)}…`, 'success');
    followedTopics.value = await service.value.listFollowedTopics();
  } catch (error) {
    notify((error as Error).message, 'error');
  } finally {
    busy.value = false;
  }
}
</script>

<style scoped>
.spark-git-repo {
  padding: 12px 16px 32px;
  font-size: 14px;
}
.message {
  margin-bottom: 12px;
}
.header-card,
.block {
  margin-bottom: 12px;
}
.header-row {
  display: flex;
  justify-content: space-between;
  align-items: flex-start;
  gap: 12px;
  flex-wrap: wrap;
}
.header-side {
  display: flex;
  gap: 8px;
  align-items: center;
}
.eyebrow {
  margin: 0;
  font-size: 12px;
  color: #999;
  letter-spacing: 0.1em;
}
h2 {
  margin: 2px 0 4px;
}
.lede {
  margin: 0;
  color: #666;
  font-size: 13px;
}
.mirror-line {
  display: flex;
  gap: 12px;
  align-items: center;
  flex-wrap: wrap;
  margin: 4px 0;
}
.muted {
  color: #999;
  font-size: 12px;
}
.hint {
  color: #999;
  font-size: 12px;
}
.sub-tabs {
  margin-top: 4px;
}
.commit-table :deep(tbody tr) {
  cursor: pointer;
}
.breadcrumb {
  margin-bottom: 8px;
}
.file-head {
  display: flex;
  gap: 8px;
  align-items: center;
  margin-bottom: 8px;
}
.file-content,
.diff-content {
  margin: 0;
  padding: 8px 0;
  background: #fafafa;
  border: 1px solid #eee;
  border-radius: 6px;
  overflow-x: auto;
  font-size: 12px;
  line-height: 1.5;
}
.code-line {
  display: block;
  white-space: pre;
  padding: 0 8px;
}
.code-line .line-no {
  display: inline-block;
  width: 3.5em;
  color: #bbb;
  user-select: none;
}
.code-line[data-kind='add'] {
  background: #e6ffed;
}
.code-line[data-kind='del'] {
  background: #ffeef0;
}
.code-line[data-kind='gap'] {
  color: #999;
  background: #f5f5f5;
}
.diff-file {
  margin-bottom: 12px;
}
.diff-head {
  display: flex;
  gap: 8px;
  align-items: center;
  margin: 6px 0;
}
.pr-actions {
  display: flex;
  gap: 12px;
  align-items: center;
}
.pr-timeline {
  margin-top: 8px;
}
.timeline-body {
  white-space: pre-wrap;
  word-break: break-word;
  color: #444;
  font-size: 13px;
}
.attach-row {
  display: flex;
  gap: 8px;
  align-items: center;
  flex-wrap: wrap;
  margin: 4px 0;
}
.actions {
  display: flex;
  gap: 8px;
  flex-wrap: wrap;
  margin: 8px 0;
}
.dir-row {
  display: flex;
  gap: 8px;
  width: 100%;
}
.review-form {
  margin-top: 12px;
}
.maintainer-actions {
  border-top: 1px dashed #ddd;
  padding-top: 12px;
}
</style>

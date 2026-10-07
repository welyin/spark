/**
 * 朋友圈插件（spark-moments）· 业务服务层。
 *
 * 对齐：wiki/product/moments.md（§五 可见性、§六 投递与互动、§八 权限）、
 *       wiki/architecture/plugins/social-feed.md（§9 sdk.feed 契约）、
 *       wiki/architecture/plugins/plugin-data-api.md（declareCollection personal scope）。
 *
 * 职责约定（对齐 spark-example service.ts 教学要点）：
 * - 所有 SDK 调用集中在服务层，视图组件不直接碰 sdk.data / sdk.feed / sdk.contacts，
 *   便于单测（tests/ 用 mock SDK 驱动本层）与权限审计（本文件即插件能力面清单）；
 * - 存储走**声明式数据 API**（sdk.data：declareCollection/save/get/query/saveBlob/readBlob），
 *   区别于 spark-example 的旧 sdk.docs——spark-moments 是 personal 空间插件，
 *   用新 API 声明 personal scope 同步集合（social-feed §10 偏差表第 1 条：scope "sync"）；
 * - 投递走 sdk.feed（feed:write 高级权限 + 内核限流 10 次/60s；feed:read 收件拉取）；
 * - 验签走 sdk.identity.verify（免权限）；发动态/删除/互动签名 identity:sign（使用时询问
 *   高危权限）。动态签名拒绝时降级为不签名（本地照发，接收方会拒收）；**删除通知与互动
 *   投递的签名是投递链路的一部分——签名失败则不投递**（防伪造硬约束优于可达性）；
 * - 通讯录走 sdk.contacts（contact:read 高级 + 使用时询问）：仅用于发动态时展开可见名单，
 *   不用于可见性裁决（裁决在发送方过滤 + 内核 DM 过滤）。
 *
 * 权限降级原则：签名、应用消息、通讯录、投递均是「增强能力」，授权被拒或限流时
 * 不阻断主流程（动态照发，仅少签名/少通知/少名单），插件对每个高级权限调用 try/catch 降级。
 * 例外：**收件验签是硬约束**（防伪造，authorRootId 与签名公钥一致性校验），验签失败即拒收不落库。
 *
 * ── UI 接线契约（本服务面向 UI 消费；后续 UI 任务按此消费，不直接碰 sdk.data/feed/contacts）──
 * 视图层经 composables/useMoments.ts 收敛访问本服务（代码规范 §四 分层收敛），主要接线：
 * - 发动态   ：publishPost({text, images, scope, selection, myRootId, myProfile})——图片已由
 *   composer/imageUtil.processImage 逐张 saveBlob 并传入 {hash, thumbHash,...}；scope 为四选一，
 *   selection 为「谁可以看」勾选项（联系人 rootId / 分组 groupId / 标签 tagId）。
 * - 收动态   ：receivePost(payload)（sdk.feed.onReceive("spark-moments:post") + pull 补读，验签后落库）。
 * - 互动     ：interact({post, type:'like'|'comment', action:'add'|'remove', text?, myRootId})；
 *   作者自己的互动直接广播名单，非作者投递作者（两跳）。
 * - 删动态   ：deletePost(post)（作者权限校验在调用方，本地标记 deletedAt + 投递删除通知）。
 * - 删评论   ：deleteComment(post, commentRootId, myRootId)。
 * - 时间线   ：loadTimeline() / loadMyPosts() / loadPostsByAuthor() / getPost() / loadInteractions()。
 * - 互动通知 ：notifyInteraction({...}) 作者收到互动后写应用会话（message:app，限流降级）。
 * - 通讯录   ：resolveContactsSelection({contactRootIds, groupIds, tagIds}) 展开名单；
 *   displayNameOf(friends, rootId, snapshot) 展示名解析。
 * feed topic（MOMENTS_TOPICS）：post / interaction / delete 三通道；interaction 通道用
 * payload.broadcast 区分「作者收件（第一跳）」与「名单广播（共同好友可见）」。
 */

import type { PluginSDK, PluginFriendSummary } from '../../packages/plugin-sdk/src';
import {
  buildDeleteSignPayload,
  buildInteractionForwardSignPayload,
  buildInteractionSignPayload,
  buildPostSignPayload,
  buildCommentExcerpt,
  buildInteractionSummary,
  buildPostExcerpt,
  computeDeleteBroadcast,
  computeInteractionBroadcast,
  dedupe,
  expandRecipients,
  interactionKey,
  newId,
  sortTimeline,
  MOMENTS_COLLECTIONS,
  type MomentsImage,
  type MomentsInteraction,
  type MomentsPost,
  type MomentsProfile,
  type MomentsSignature,
  type MomentsVisibleScope
} from './model';

/** feed topic（social-feed §9：`{pluginId}:{sub}`，前缀须 == 本插件 id） */
export const MOMENTS_TOPICS = {
  post: 'spark-moments:post',
  interaction: 'spark-moments:interaction',
  delete: 'spark-moments:delete'
} as const;

/** 集合声明（plugin-data-api §2：personal 空间 scope 缺省 sync，lww-record 默认） */
const MOMENTS_DECLARATIONS = [
  { name: MOMENTS_COLLECTIONS.posts },
  { name: MOMENTS_COLLECTIONS.interactions },
  { name: MOMENTS_COLLECTIONS.profile }
] as const;

/** 我的 profile 集合键（恒 "self"） */
const PROFILE_KEY = 'self';

/** 公开可见：需要全部联系人名单（contact:read 展开） */
export class MomentsService {
  private collectionsReady: Promise<void> | null = null;

  constructor(private readonly sdk: PluginSDK) {}

  // ---------------------------------------------------------------------------
  // 集合声明（幂等，重复声明与首次一致即可）
  // ---------------------------------------------------------------------------

  private ensureCollectionsDeclared(): Promise<void> {
    this.collectionsReady ??= (async () => {
      for (const decl of MOMENTS_DECLARATIONS) {
        await this.sdk.data.declareCollection(decl);
      }
    })();
    return this.collectionsReady;
  }

  // ---------------------------------------------------------------------------
  // 通讯录：名单展开（contact:read，仅发动态用）
  // ---------------------------------------------------------------------------

  /**
   * 从通讯录展开「联系人/分组/标签」勾选为 rootId 集合。
   * 勾选来源：具体联系人 rootId + 分组下成员 + 标签下成员（去重）。
   * 返回全部联系人 rootId 与选中展开 rootId——供 expandRecipients 做四选一裁决。
   */
  async resolveContactsSelection(
    selected: { contactRootIds: string[]; groupIds: string[]; tagIds: string[] }
  ): Promise<{ allRootIds: string[]; selectedRootIds: string[] }> {
    const contacts = this.sdk.contacts ? await this.sdk.contacts.listFriends() : [];
    const allRootIds = contacts.map((f) => f.rootId);

    const selectedGroups = new Set(selected.groupIds);
    const selectedTags = new Set(selected.tagIds);
    const selectedRootIds: string[] = [];

    for (const friend of contacts) {
      const inGroup = selectedGroups.has(friend.groupId);
      const inTag = friend.tagIds.some((tagId) => selectedTags.has(tagId));
      if (selected.contactRootIds.includes(friend.rootId) || inGroup || inTag) {
        selectedRootIds.push(friend.rootId);
      }
    }
    return { allRootIds, selectedRootIds: dedupe(selectedRootIds) };
  }

  /** 展示名解析：备注 > 昵称（联系人列表内查找） */
  displayNameOf(friends: PluginFriendSummary[] | undefined, rootId: string, snapshot?: MomentsPost['authorSnapshot']): string {
    const match = friends?.find((f) => f.rootId === rootId);
    if (match) return match.nickname;
    return snapshot?.nickname || rootId.slice(0, 8);
  }

  // ---------------------------------------------------------------------------
  // 我的 profile（E6 降级：本地存储，pdsync 自设备同步）
  // ---------------------------------------------------------------------------

  async getSelfProfile(): Promise<MomentsProfile | null> {
    await this.ensureCollectionsDeclared();
    return this.sdk.data.get<MomentsProfile>(MOMENTS_COLLECTIONS.profile, PROFILE_KEY);
  }

  async saveSelfProfile(profile: MomentsProfile): Promise<void> {
    await this.ensureCollectionsDeclared();
    await this.sdk.data.save(MOMENTS_COLLECTIONS.profile, PROFILE_KEY, { ...profile, updatedAt: Date.now() });
  }

  // ---------------------------------------------------------------------------
  // 发动态（产品 §6.1 + UI composer.md §5.2 发布管线）
  // ---------------------------------------------------------------------------

  /**
   * 发布动态：
   * 1. 按可见性展开 recipients；
   * 2. 域身份签名（identity:sign，拒绝则降级为不签名）；
   * 3. 落 spark-moments:posts；
   * 4. 非私密则 sdk.feed.deliver 定向投递（离线由内核补投）。
   *
   * 图片：调用方已把每张图 saveBlob（原图 + 缩略图）并传入 {hash, thumbHash,...}，
   * 本方法只组装记录与投递。
   */
  async publishPost(input: {
    text: string;
    images: MomentsImage[];
    scope: MomentsVisibleScope;
    selection: { contactRootIds: string[]; groupIds: string[]; tagIds: string[] };
    myRootId: string;
    myProfile: MomentsProfile;
  }): Promise<MomentsPost> {
    await this.ensureCollectionsDeclared();

    // ① 可见性展开：public/partial/exclude 依赖通讯录；private 不投递
    const { allRootIds, selectedRootIds } =
      input.scope === 'private' ? { allRootIds: [], selectedRootIds: [] } : await this.resolveContactsSelection(input.selection);
    const { recipients, visibleList } = expandRecipients(input.scope, allRootIds, selectedRootIds);

    const post: MomentsPost = {
      id: newId('post'),
      authorRootId: input.myRootId,
      text: input.text,
      images: input.images,
      createdAt: Date.now(),
      visibleScope: input.scope,
      visibleList,
      recipients,
      authorSnapshot: { nickname: input.myProfile.nickname, avatar: input.myProfile.avatar }
    };

    // ② 签名（增强能力：拒绝降级为不签名，不阻断发帖）
    post.signature = await this.signPost(post);

    // ③ 落库
    await this.sdk.data.save(MOMENTS_COLLECTIONS.posts, post.id, post);

    // ④ 投递（private 不投递；feed 尽力而为 + 离线补投，无逐人回执）
    if (recipients.length > 0 && this.sdk.feed) {
      await this.deliverPost(post);
    }
    return post;
  }

  /** 动态签名（identity:sign 使用时询问；拒绝降级返回 undefined） */
  private async signPost(post: MomentsPost): Promise<MomentsSignature | undefined> {
    try {
      const payload = buildPostSignPayload(post);
      const result = await this.sdk.identity.sign(payload);
      return { payload, signature: result.signature, publicKey: result.publicKey };
    } catch (error) {
      console.warn('[spark-moments] 签名被拒或不可用，动态不带签名：', error);
      return undefined;
    }
  }

  /** 投递动态（topic spark-moments:post，payload 携带签名与作者快照） */
  private async deliverPost(post: MomentsPost): Promise<void> {
    // feed payload 上限 32 KiB：authorSnapshot.avatar 是 data URL，可达数十 KB，
    // 携带会超限导致投递失败（`invalid feed body`）。投递载荷剥离大头像——
    // 验签不依赖它（buildPostSignPayload 不含 authorSnapshot），接收方头像优先
    // 走通讯录 friends，故此处剥离不影响接收侧展示。
    const payloadPost = post.authorSnapshot?.avatar
      ? { ...post, authorSnapshot: { ...post.authorSnapshot, avatar: undefined } }
      : post;
    await this.sdk.feed!.deliver({
      topic: MOMENTS_TOPICS.post,
      payload: { post: payloadPost },
      recipients: post.recipients
    });
  }

  // ---------------------------------------------------------------------------
  // 收动态（产品 §6.1 收动态：验签 → 落库；防伪造硬约束）
  // ---------------------------------------------------------------------------

  /**
   * 收到动态（sdk.feed.onReceive / pull 补读路径），先验签后落库。
   * 投递线形是信封 `{post}`（见 deliverPost），此处必须拆封；兼容裸 post（旧路径/测试直调）。
   * 验签失败（签名无效 / payload 与当前内容不符 / authorRootId 与签名公钥不一致）→ 拒收。
   * @returns 是否接受落库
   */
  async receivePost(payload: unknown): Promise<boolean> {
    await this.ensureCollectionsDeclared();
    const envelope = (payload ?? {}) as { post?: MomentsPost };
    const post = envelope.post ?? (payload as MomentsPost);
    if (!post || !post.id || !post.authorRootId) {
      return false;
    }
    if (!(await this.verifyPostSignature(post))) {
      console.warn('[spark-moments] 拒收验签失败的动态', post.id);
      return false;
    }
    // 幂等兜底：spark-moments:posts 以 postId 为键，重复投递天然覆盖（social-feed §4.3）
    await this.sdk.data.save(MOMENTS_COLLECTIONS.posts, post.id, post);
    return true;
  }

  /** 验签：从帖子当前字段重算载荷比对 + 密码学验签 + 公钥绑定校验 */
  async verifyPostSignature(post: MomentsPost): Promise<boolean> {
    if (!post.signature) {
      return false;
    }
    const expected = buildPostSignPayload(post);
    if (post.signature.payload !== expected) {
      // 随帖 payload 与当前内容/作者/归属不符：拒绝
      return false;
    }
    const result = await this.sdk.identity.verify(expected, post.signature.signature, post.signature.publicKey);
    return result.valid;
  }

  // ---------------------------------------------------------------------------
  // 互动（产品 §6.2：点赞/评论 + 两跳投递）
  // ---------------------------------------------------------------------------

  /**
   * 本地对动态发起互动（点赞/评论/取消/删除）。
   * 1. 落 spark-moments:interactions（key 复合）；
   * 2. 若我是该动态作者：直接广播给投递名单（作者自己的互动，无需给自己发）；
   * 3. 否则：投递作者（authorRootId），作者收到后再广播名单（两跳）。
   */
  async interact(input: {
    post: MomentsPost;
    type: 'like' | 'comment';
    action: 'add' | 'remove';
    text?: string;
    myRootId: string;
  }): Promise<MomentsInteraction> {
    await this.ensureCollectionsDeclared();
    const interaction: MomentsInteraction = {
      type: input.type,
      action: input.action,
      text: input.text,
      ts: Date.now()
    };

    // 落本地（key 复合：postId:type:rootId）
    await this.sdk.data.save(MOMENTS_COLLECTIONS.interactions, interactionKey(input.post.id, input.type, input.myRootId), interaction);

    // 作者自己的互动：签名后直接按投递名单广播（广播须带互动签名 + 作者转发签名双重证据，
    // 接收方验签硬约束；签名被拒则只落本地不广播）
    if (input.post.authorRootId === input.myRootId) {
      const signature = await this.signInteraction(input.post.id, input.type, input.myRootId, interaction);
      if (signature && this.sdk.feed) {
        await this.broadcastInteraction(input.post, input.type, input.myRootId, interaction, signature);
      } else if (!signature) {
        console.warn('[spark-moments] 互动签名被拒，本次互动仅本地落库不广播');
      }
      return interaction;
    }

    // 非作者：投递作者（定向），作者收到后再广播
    if (this.sdk.feed) {
      await this.deliverInteractionToAuthor(input.post, input.type, input.myRootId, interaction);
    }
    return interaction;
  }

  /** 互动投递作者（第一跳：互动者 → 作者）；签名失败则不投递（防伪造硬约束） */
  private async deliverInteractionToAuthor(
    post: MomentsPost,
    type: 'like' | 'comment',
    rootId: string,
    interaction: MomentsInteraction
  ): Promise<void> {
    // 互动投递必须带签名（作者验签后才会落库并广播）——签名拒绝则不投递
    const signature = await this.signInteraction(post.id, type, rootId, interaction);
    if (!signature) {
      console.warn('[spark-moments] 互动签名被拒，互动不投递（仅本地落库）');
      return;
    }
    await this.sdk.feed!.deliver({
      topic: MOMENTS_TOPICS.interaction,
      payload: { postId: post.id, type, rootId, interaction, signature },
      recipients: [post.authorRootId],
      replyTo: post.id
    });
  }

  /** 互动签名（identity:sign；互动投递链路必需） */
  private async signInteraction(
    postId: string,
    type: 'like' | 'comment',
    rootId: string,
    interaction: MomentsInteraction
  ): Promise<MomentsSignature | undefined> {
    try {
      const payload = buildInteractionSignPayload(postId, type, rootId, interaction.text ?? '', interaction.action);
      const result = await this.sdk.identity.sign(payload);
      return { payload, signature: result.signature, publicKey: result.publicKey };
    } catch (error) {
      console.warn('[spark-moments] 互动签名被拒，互动无法投递：', error);
      return undefined;
    }
  }

  /**
   * 作者转发签名（第二跳广播用）：作者以本机域身份签「我认可把这条互动广播给名单」。
   * 接收方验此签名 + 比对转发公钥与本地动态签名公钥，确认广播出自作者设备。
   */
  private async signInteractionForward(
    postId: string,
    type: 'like' | 'comment',
    rootId: string,
    interaction: MomentsInteraction
  ): Promise<MomentsSignature | undefined> {
    try {
      const payload = buildInteractionForwardSignPayload(postId, type, rootId, interaction.text ?? '', interaction.action);
      const result = await this.sdk.identity.sign(payload);
      return { payload, signature: result.signature, publicKey: result.publicKey };
    } catch (error) {
      console.warn('[spark-moments] 转发签名被拒，互动广播不投递：', error);
      return undefined;
    }
  }

  // ---------------------------------------------------------------------------
  // 互动接收与广播（产品 §6.2 第二跳：作者 → 投递名单）
  // ---------------------------------------------------------------------------

  /**
   * 收到互动（第一跳，互动者 → 作者投递）。验签通过后落库；
   * 仅当本机确为该动态作者（post.authorRootId === myRootId）时才广播名单——
   * 非本人动态的第一跳互动只落库不广播（防伪造放大：攻击者把第一跳直接投递给
   * 非作者收件人时，受害本机不得按 post.recipients 放大转发）。
   * @param myRootId 当前用户 rootId（作者判定依据；未注入时一律视为非作者，不广播）
   * @returns 是否处理（验签通过 + 字段齐全）
   */
  async receiveInteraction(payload: unknown, myRootId?: string): Promise<boolean> {
    await this.ensureCollectionsDeclared();
    const msg = payload as { postId?: string; type?: 'like' | 'comment'; rootId?: string; interaction?: MomentsInteraction; signature?: MomentsSignature };
    if (!msg?.postId || !msg.type || !msg.rootId || !msg.interaction) {
      return false;
    }
    // 验签（防伪造硬约束）
    if (!msg.signature) {
      return false;
    }
    const expected = buildInteractionSignPayload(msg.postId, msg.type, msg.rootId, msg.interaction.text ?? '', msg.interaction.action);
    if (msg.signature.payload !== expected) {
      return false;
    }
    const result = await this.sdk.identity.verify(expected, msg.signature.signature, msg.signature.publicKey);
    if (!result.valid) {
      return false;
    }

    // 读取对应动态（广播名单来源）；作者判定用注入的 myRootId——
    // 不可用 sdk.domain（插件域身份字符串，与 rootId 永远不等，守卫会形同虚设）
    const post = await this.sdk.data.get<MomentsPost>(MOMENTS_COLLECTIONS.posts, msg.postId);
    const isAuthor = !!post && typeof myRootId === 'string' && post.authorRootId === myRootId;

    // 落库
    await this.sdk.data.save(MOMENTS_COLLECTIONS.interactions, interactionKey(msg.postId, msg.type, msg.rootId), msg.interaction);

    // 仅作者广播给名单（除发起者）；广播携带原互动签名 + 作者转发签名双重证据
    if (isAuthor && this.sdk.feed) {
      await this.broadcastInteraction(post, msg.type, msg.rootId, msg.interaction, msg.signature);
    }
    return true;
  }

  /**
   * 广播互动给动态投递名单（除发起者外）。
   * 广播 payload 携带双重签名证据：`signature` = 原互动者签名（第一跳签名原样携带），
   * `forward` = 作者转发签名（证明「该动态作者认可此广播」）——接收方验签后才落库。
   */
  private async broadcastInteraction(
    post: MomentsPost,
    type: 'like' | 'comment',
    interactionRootId: string,
    interaction: MomentsInteraction,
    interactionSignature: MomentsSignature
  ): Promise<void> {
    const recipients = computeInteractionBroadcast(post.recipients, interactionRootId);
    if (recipients.length === 0) return;
    const forward = await this.signInteractionForward(post.id, type, interactionRootId, interaction);
    if (!forward) {
      // 转发签名失败则不广播（接收方免验签时代已结束，无签名广播会被一律拒收）
      return;
    }
    await this.sdk.feed!.deliver({
      topic: MOMENTS_TOPICS.interaction,
      payload: { postId: post.id, type, rootId: interactionRootId, interaction, broadcast: true, signature: interactionSignature, forward },
      recipients,
      replyTo: post.id
    });
  }

  // ---------------------------------------------------------------------------
  // 互动广播接收（非作者：共同好友可见评论，产品 §6.2 第三跳）
  // ---------------------------------------------------------------------------

  /**
   * 收到互动广播（作为非作者）。验签硬约束：
   * ① 原互动者签名（重算载荷比对 + 密码学验签）；
   * ② 作者转发签名（证明作者认可此广播）；
   * ③ 本机确有该动态且其签名公钥与转发公钥一致（作者身份绑定）。
   * 任一失败即拒收不落库（「收到过这条动态的人才会收到广播」只是可见性事实，
   * 不是防伪依据——recipients 由发送方指定，内核不做作者过滤）。
   * @returns 是否接受落库
   */
  async receiveInteractionBroadcast(payload: unknown): Promise<boolean> {
    const msg = payload as {
      postId?: string;
      type?: 'like' | 'comment';
      rootId?: string;
      interaction?: MomentsInteraction;
      broadcast?: boolean;
      signature?: MomentsSignature;
      forward?: MomentsSignature;
    };
    if (!msg?.postId || !msg.type || !msg.rootId || !msg.interaction || msg.broadcast !== true) {
      return false;
    }
    await this.ensureCollectionsDeclared();

    // ① 原互动者签名
    if (!msg.signature) return false;
    const expectedInteraction = buildInteractionSignPayload(msg.postId, msg.type, msg.rootId, msg.interaction.text ?? '', msg.interaction.action);
    if (msg.signature.payload !== expectedInteraction) return false;
    const interactionVerify = await this.sdk.identity.verify(expectedInteraction, msg.signature.signature, msg.signature.publicKey);
    if (!interactionVerify.valid) return false;

    // ② 作者转发签名
    if (!msg.forward) return false;
    const expectedForward = buildInteractionForwardSignPayload(msg.postId, msg.type, msg.rootId, msg.interaction.text ?? '', msg.interaction.action);
    if (msg.forward.payload !== expectedForward) return false;
    const forwardVerify = await this.sdk.identity.verify(expectedForward, msg.forward.signature, msg.forward.publicKey);
    if (!forwardVerify.valid) return false;

    // ③ 作者身份绑定：本机确有该动态，且动态签名公钥 == 转发公钥
    const post = await this.sdk.data.get<MomentsPost>(MOMENTS_COLLECTIONS.posts, msg.postId);
    if (!post || !post.signature || post.signature.publicKey !== msg.forward.publicKey) {
      console.warn('[spark-moments] 拒收作者身份无法绑定的互动广播', msg.postId);
      return false;
    }

    await this.sdk.data.save(MOMENTS_COLLECTIONS.interactions, interactionKey(msg.postId, msg.type, msg.rootId), msg.interaction);
    return true;
  }

  // ---------------------------------------------------------------------------
  // 删除动态（产品 §6.5：作者删 → 标记 deletedAt + 投递删除通知）
  // ---------------------------------------------------------------------------

  /**
   * 删除动态（作者）：本地标记 deletedAt + 向原收件人名单投递**带签名**的删除通知。
   * 删除通知必须带作者签名（buildDeleteSignPayload 绑定 postId + authorRootId）——
   * 签名被拒则不投递（仅本地删除；防伪造硬约束优于远端一致性）。
   * 接收方收到后验签 + 作者一致性校验才标记删除（见 receiveDelete）。
   */
  async deletePost(post: MomentsPost): Promise<void> {
    await this.ensureCollectionsDeclared();
    const deleted: MomentsPost = { ...post, deletedAt: Date.now() };
    await this.sdk.data.save(MOMENTS_COLLECTIONS.posts, post.id, deleted);

    if (this.sdk.feed && post.recipients.length > 0) {
      const signature = await this.signDelete(post.id, post.authorRootId);
      if (!signature) {
        console.warn('[spark-moments] 删除签名被拒，删除通知不投递（仅本地删除）：', post.id);
        return;
      }
      const recipients = computeDeleteBroadcast(post.recipients);
      // 删除通知复用 sdk.feed.deliver（topic 区分），走同样内核过滤链路
      await this.sdk.feed.deliver({
        topic: MOMENTS_TOPICS.delete,
        payload: { postId: post.id, authorRootId: post.authorRootId, deletedAt: deleted.deletedAt, signature },
        recipients
      });
    }
  }

  /** 删除通知签名（identity:sign；删除投递链路必需，签名失败不投递） */
  private async signDelete(postId: string, authorRootId: string): Promise<MomentsSignature | undefined> {
    try {
      const payload = buildDeleteSignPayload(postId, authorRootId);
      const result = await this.sdk.identity.sign(payload);
      return { payload, signature: result.signature, publicKey: result.publicKey };
    } catch (error) {
      console.warn('[spark-moments] 删除签名被拒或不可用：', error);
      return undefined;
    }
  }

  /**
   * 收到删除通知（接收方）：验签硬约束——
   * ① 必须带作者签名（无签名一律拒收，不再有旧版降级）；
   * ② 重算 buildDeleteSignPayload 比对 + 密码学验签；
   * ③ 作者一致性：payload.authorRootId 须等于本地 post.authorRootId，
   *    且本地动态有签名时其公钥须与删除签名公钥一致（作者身份绑定）。
   * 任一失败即拒收，不标记删除。
   * @returns 是否接受（标记删除）
   */
  async receiveDelete(payload: unknown): Promise<boolean> {
    const msg = payload as { postId?: string; authorRootId?: string; deletedAt?: number; signature?: MomentsSignature };
    if (!msg?.postId || !msg.authorRootId || !msg.signature) {
      return false;
    }
    await this.ensureCollectionsDeclared();

    const expected = buildDeleteSignPayload(msg.postId, msg.authorRootId);
    if (msg.signature.payload !== expected) {
      return false;
    }
    const result = await this.sdk.identity.verify(expected, msg.signature.signature, msg.signature.publicKey);
    if (!result.valid) {
      return false;
    }

    const post = await this.sdk.data.get<MomentsPost>(MOMENTS_COLLECTIONS.posts, msg.postId);
    if (!post) return false;
    if (post.authorRootId !== msg.authorRootId) {
      console.warn('[spark-moments] 拒收作者不符的删除通知', msg.postId);
      return false;
    }
    if (post.signature && post.signature.publicKey !== msg.signature.publicKey) {
      console.warn('[spark-moments] 拒收签名公钥与动态作者不符的删除通知', msg.postId);
      return false;
    }
    await this.sdk.data.save(MOMENTS_COLLECTIONS.posts, msg.postId, { ...post, deletedAt: msg.deletedAt ?? Date.now() });
    return true;
  }

  // ---------------------------------------------------------------------------
  // 删除评论（作者删任一 / 评论者删自己的；remove 广播）
  // ---------------------------------------------------------------------------

  /**
   * 删除评论：本地移除 + 投递作者（作者再广播名单）。
   * 复用 interact(remove) 链路。
   */
  async deleteComment(post: MomentsPost, commentRootId: string, myRootId: string): Promise<void> {
    await this.interact({ post, type: 'comment', action: 'remove', myRootId });
  }

  // ---------------------------------------------------------------------------
  // 时间线读取（产品 §4.1：本地 spark-moments:posts 按 createdAt 倒序，过滤 deletedAt）
  // ---------------------------------------------------------------------------

  /** 读取全部未删除动态（按 createdAt 倒序）。分页在视图层用 cursor 续拉。 */
  async loadTimeline(): Promise<MomentsPost[]> {
    await this.ensureCollectionsDeclared();
    const response = await this.sdk.data.query<MomentsPost>(MOMENTS_COLLECTIONS.posts, { limit: 500 });
    return sortTimeline(response.items.map((item) => item.value));
  }

  /** 读取某条动态 */
  async getPost(postId: string): Promise<MomentsPost | null> {
    await this.ensureCollectionsDeclared();
    return this.sdk.data.get<MomentsPost>(MOMENTS_COLLECTIONS.posts, postId);
  }

  /** 读取某动态的互动（点赞 + 评论），按 key 前缀（postId:） */
  async loadInteractions(postId: string): Promise<Array<{ key: string; interaction: MomentsInteraction }>> {
    await this.ensureCollectionsDeclared();
    const response = await this.sdk.data.query<MomentsInteraction>(MOMENTS_COLLECTIONS.interactions, {
      prefix: `${postId}:`,
      limit: 500
    });
    return response.items.map((item) => ({ key: item.key, interaction: item.value }));
  }

  /** 仅看动态：自己的动态 */
  async loadMyPosts(myRootId: string): Promise<MomentsPost[]> {
    const all = await this.loadTimeline();
    return all.filter((post) => post.authorRootId === myRootId);
  }

  /** ta 的动态：某联系人发给我的且可见的动态 */
  async loadPostsByAuthor(authorRootId: string): Promise<MomentsPost[]> {
    const all = await this.loadTimeline();
    return all.filter((post) => post.authorRootId === authorRootId);
  }

  // ---------------------------------------------------------------------------
  // 互动通知（产品 §7.7：后台/视图写应用会话，message:app）
  // ---------------------------------------------------------------------------

  /**
   * 生成互动通知应用消息（message:app 高级 + 限流）。
   * payload.summary 强制（未装插件时壳层原生渲染）；card 富渲染互动卡片。
   * @returns 是否成功写入应用会话
   */
  async notifyInteraction(input: {
    kind: 'like' | 'comment';
    fromRootIds: string[];
    fromName: string;
    fromAvatar?: string | null;
    count: number;
    postId: string;
    postExcerpt: string;
    postThumbHash?: string | null;
    commentExcerpt?: string;
    ts: number;
  }): Promise<boolean> {
    if (!this.sdk.messages) return false;
    try {
      const summary = buildInteractionSummary(input.kind, input.fromName, input.count, input.postExcerpt, input.commentExcerpt);
      await this.sdk.messages.sendAppMessage(
        { summary, ...input },
        {
          viewId: 'notify-card',
          data: {
            kind: input.kind,
            fromRootIds: input.fromRootIds,
            fromName: input.fromName,
            fromAvatar: input.fromAvatar ?? null,
            count: input.count,
            postId: input.postId,
            postExcerpt: input.postExcerpt,
            postThumbHash: input.postThumbHash ?? null,
            commentExcerpt: input.commentExcerpt,
            ts: input.ts
          }
        }
      );
      return true;
    } catch (error) {
      console.warn('[spark-moments] 互动通知发送失败（权限/限流降级）：', error);
      return false;
    }
  }
}



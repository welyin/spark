<!--
  朋友圈插件（spark-moments）· message-card 视图：互动通知卡片（cards.md §2）。
  首行：互动者头像 +「{昵称} 赞了/评论了你的动态」+ 相对时间；
  摘要块：动态文字前 60 字（纯图「[图片]」）+ 首图缩略图；评论追加评论摘录；
  底部「查看动态 ›」经 triggerCardAction('open-post') 上行。
  能力面裁剪（无网络/无签名/无 messages 写/无 feed 域）；缩略图 readBlob 若未放行则降级纯文字。
-->
<template>
  <section class="notify-card">
    <div class="row">
      <span class="actor-avatar">{{ fromName.slice(0, 1) }}</span>
      <div class="headline">
        <span class="actor-name">{{ fromName }}</span>
        {{ kind === 'like' ? '赞了你的动态' : '评论了你的动态' }}
      </div>
      <span class="time">{{ formatRelativeTime(ts) }}</span>
    </div>

    <div class="excerpt-block">
      <div class="excerpt-text">
        <span class="preview">{{ postExcerpt }}</span>
        <span v-if="kind === 'comment' && commentExcerpt" class="comment-preview">{{ commentExcerpt }}</span>
      </div>
      <ThumbImage v-if="postThumbHash" :image="thumbImage" class="excerpt-thumb" />
    </div>

    <button type="button" class="open" @click="openPost">查看动态 ›</button>
  </section>
</template>

<script lang="ts">
import { computed, defineComponent, type PropType } from 'vue';
import { ensurePluginSDK } from '../../packages/plugin-sdk/src';
import ThumbImage from './components/ThumbImage.vue';
import { formatRelativeTime } from './model';

export type NotifyCardData = {
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
};

export default defineComponent({
  name: 'NotifyCard',
  components: { ThumbImage },
  props: {
    cardData: { type: Object as PropType<NotifyCardData | undefined>, required: false, default: undefined }
  },
  setup(props) {
    const data = computed(() => props.cardData);
    const kind = computed(() => data.value?.kind ?? 'like');
    const fromName = computed(() => data.value?.fromName ?? '');
    const ts = computed(() => data.value?.ts ?? 0);
    const postExcerpt = computed(() => data.value?.postExcerpt ?? '');
    const commentExcerpt = computed(() => data.value?.commentExcerpt ?? '');
    const postThumbHash = computed(() => data.value?.postThumbHash ?? null);
    const thumbImage = computed(() => ({
      hash: postThumbHash.value ?? '',
      thumbHash: postThumbHash.value ?? '',
      name: '',
      size: 0,
      mime: 'image/jpeg'
    }));

    const openPost = () => {
      const sdk = window.__sparkPluginSDK;
      const postId = data.value?.postId;
      if (!sdk?.messages || !postId) return;
      sdk.messages.triggerCardAction('open-post', { postId });
    };

    return { kind, fromName, ts, postExcerpt, commentExcerpt, postThumbHash, thumbImage, openPost, formatRelativeTime };
  }
});
</script>

<style scoped>
.notify-card {
  padding: 12px 14px;
  font-size: 13px;
  color: var(--spark-text-1, #1e293b);
}
.row { display: flex; align-items: center; gap: 8px; }
.actor-avatar { width: 28px; height: 28px; border-radius: 50%; background: var(--spark-primary, #4f7cff); color: #fff; display: flex; align-items: center; justify-content: center; font-size: 13px; flex-shrink: 0; }
.headline { flex: 1; }
.actor-name { color: var(--spark-primary, #4f7cff); font-weight: 600; }
.time { color: var(--spark-text-3, #94a3b8); font-size: 12px; }
.excerpt-block { display: flex; gap: 10px; margin-top: 8px; background: var(--spark-bg-hover, #f8fafc); border-radius: 6px; padding: 8px; }
.excerpt-text { flex: 1; display: flex; flex-direction: column; gap: 4px; }
.preview { color: var(--spark-text-2, #475569); overflow: hidden; display: -webkit-box; -webkit-line-clamp: 2; -webkit-box-orient: vertical; }
.comment-preview { color: var(--spark-text-3, #94a3b8); font-size: 12px; }
.excerpt-thumb { width: 48px; height: 48px; border-radius: 4px; overflow: hidden; flex-shrink: 0; }
.open { margin-top: 8px; margin-left: auto; display: block; border: none; background: none; color: var(--spark-primary, #4f7cff); cursor: pointer; font-size: 13px; }
</style>

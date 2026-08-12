/**
 * 朋友圈插件（spark-moments）· 页栈组合式函数（UI 设计 README §9 落地要点）。
 *
 * 收敛插件 iframe 内页栈导航：主页 → 详情/发动态/我的动态/谁可以看/名单编辑器。
 * 两级返回语义：插件内返回 = 页栈回退；壳层头部返回 = 退出插件（壳层负责）。
 */

import { computed, defineAsyncComponent, ref, type Component } from 'vue';

/** 组件或其异步 loader（`() => import(...)` 返回模块 Promise） */
export type PageComponent = Component | (() => Promise<{ default: Component }>);

export type PageStackEntry = {
  /** 页面身份（跳转定位用） */
  name: string;
  component: PageComponent;
  props?: Record<string, unknown>;
  title?: string;
};

/** 把 async loader 包成合法组件；真实组件原样返回 */
function toComponent(c: PageComponent): Component {
  if (typeof c === 'function' && !(c as Component).render) {
    return defineAsyncComponent(c as () => Promise<{ default: Component }>);
  }
  return c as Component;
}

/** 全局唯一页栈实例（每插件主视图一份；消息卡片视图不用页栈） */
const stack = ref<PageStackEntry[]>([]);

export function usePageStack() {
  const current = computed(() => stack.value[stack.value.length - 1] ?? null);

  function push(entry: PageStackEntry): void {
    // async loader（() => import()）统一用 defineAsyncComponent 包成合法组件，
    // 否则 `<component :is>` 会把函数当函数式组件调用、渲染成 [object Promise]
    stack.value = [...stack.value, { ...entry, component: toComponent(entry.component) }];
  }

  function pop(): void {
    if (stack.value.length > 0) {
      stack.value = stack.value.slice(0, -1);
    }
  }

  function clear(): void {
    stack.value = [];
  }

  /** 跳转并压栈（供消息卡片回调定位详情页用） */
  function pushDetail(postId: string): void {
    const detail = loadDetail();
    // 延迟动态 import 避免循环依赖
    void detail.then((module) => {
      push({
        name: 'detail',
        component: module.default,
        props: { postId },
        title: '动态'
      });
    });
  }

  return { stack, current, push, pop, clear, pushDetail };
}

/** 动态详情页懒加载（避免主入口静态引入循环依赖） */
function loadDetail(): Promise<{ default: Component }> {
  return import('../detail/PostDetailView.vue');
}

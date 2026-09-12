/**
 * PC 左栏入口的全局模态层（problem L12 / shell-desktop v0.7 §一 / ui-architecture v0.7 §4.1）：
 * 左栏功能入口（全部消息 / 所有事务 / 应用管理 / 系统设置 / 我的 / 测试）统一开
 * 「顶级对话框 ＋ 半透明遮罩」（同全局搜索 ⌘K 的弹法），不再以桌面窗口或整页跳转呈现。
 *
 * 统一规格由承载组件 ShellModals.vue 落实（el-dialog 同 DesktopSearch 基准）：
 * 顶级 z-index（Element 弹层层级，压过桌面窗口与 Dock）、遮罩锁定背景、
 * 点遮罩 / Esc 关闭、单实例（任一时刻至多一个壳层模态，重开同一入口即聚焦）。
 * 例外（不套模态）：点具体空间项＝切换桌面；桌面双击应用图标＝桌面多窗口。
 * 手机端不经此层（消息 / 事务等仍是全屏 tab）。
 */
import { reactive, ref } from 'vue';

export type ShellModalId = 'messages' | 'affairs' | 'apps' | 'settings' | 'mine' | 'test';

/** 当前打开的壳层模态（null=无；互斥单实例） */
export const activeShellModal = ref<ShellModalId | null>(null);

/** 打开选项：应用管理可指定初始视图（Dock「应用市场」直达 market；
    桌面画布入口「为本空间启用」直达 enable） */
export interface ShellModalOptions {
  appsView?: 'list' | 'market' | 'enable';
}

/** 随本次打开携带的选项（消费后即清，避免下次复开沿用旧视图） */
export const shellModalOptions = reactive<ShellModalOptions>({});

/** 打开壳层模态（幂等：同一入口已开则保持聚焦，不叠第二个） */
export function openShellModal(id: ShellModalId, options: ShellModalOptions = {}): void {
  shellModalOptions.appsView = options.appsView;
  activeShellModal.value = id;
}

export function closeShellModal(): void {
  activeShellModal.value = null;
  shellModalOptions.appsView = undefined;
}

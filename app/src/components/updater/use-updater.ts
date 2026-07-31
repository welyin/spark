/**
 * 主程序更新就绪弹窗：后台自动检查+下载完成后（`updater://ready` 事件）
 * 弹重启确认框；用户确认即安装重启，取消后可去 设置→关于 手动安装
 * （后端状态保留 Downloaded）。App.vue setup 挂载；无桥接（demo/单测）
 * 时静默不订阅。
 */
import { onMounted, onUnmounted } from 'vue';
import { ElMessage, ElMessageBox } from 'element-plus';
import type { UpdaterReadyInfo } from '../../api';

export function useUpdaterReadyPrompt(): void {
  let unlisten: (() => void) | undefined;
  // 同一时刻只弹一个确认框（后台重发 ready 事件时不叠弹）
  let prompting = false;

  const prompt = (info: UpdaterReadyInfo) => {
    const updater = window.electronAPI?.updater;
    if (!updater || prompting) {
      return;
    }
    prompting = true;
    const notes = info.notes?.trim();
    ElMessageBox.confirm(
      notes
        ? `新版本 ${info.version} 已下载就绪，重启应用完成安装。\n\n${notes}`
        : `新版本 ${info.version} 已下载就绪，重启应用完成安装。`,
      '安装更新',
      {
        confirmButtonText: '重启安装',
        cancelButtonText: '稍后',
        type: 'info',
        // 区分「稍后」与右上角关闭：两者都仅关窗（状态保留，可去关于页手动安装）
        distinguishCancelAndClose: true
      }
    )
      .then(() => updater.applyRestart())
      .catch((reason: unknown) => {
        // 取消/关闭：不做任何事（稍后可去 设置→关于 手动「重启安装」）；
        // 真正的 applyRestart 失败（安装错误等）必须让用户感知
        if (reason !== 'cancel' && reason !== 'close') {
          ElMessage.error(`安装更新失败：${reason}`);
        }
      })
      .finally(() => {
        prompting = false;
      });
  };

  onMounted(async () => {
    const updater = window.electronAPI?.updater;
    if (!updater?.onReady) {
      return;
    }
    try {
      unlisten = await updater.onReady(prompt);
    } catch {
      // 订阅失败静默：不阻断应用启动，下次启动重试
    }
  });

  onUnmounted(() => unlisten?.());
}

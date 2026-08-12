import { computed, ref } from 'vue';
import { ElMessage, ElMessageBox } from 'element-plus';
import { errorMessage } from '../utils/ipc';
import type { P2pEventDto, RecoveryPendingDto } from '../api/types';

const STORAGE_KEY_PREFIX = 'spark:recovery:inbound:';
const DELAY_HOURS_DEFAULT = 24;

const pending = ref<RecoveryPendingDto | null>(null);
const readyToConfirm = ref(false);
const inbound = ref<{ requestId: string; fromDevice: string; deadline: number; op?: 'reset_password' | 'pair_new_device' } | null>(null);
const lastError = ref<string>('');
const now = ref(Date.now());
let timer: ReturnType<typeof setInterval> | null = null;

function storageKey(rootId: string): string {
  return `${STORAGE_KEY_PREFIX}${rootId}`;
}

function readInbound(rootId: string) {
  if (typeof window === 'undefined' || !window.localStorage) return null;
  try {
    const raw = window.localStorage.getItem(storageKey(rootId));
    if (!raw) return null;
    const parsed = JSON.parse(raw);
    if (parsed && typeof parsed.requestId === 'string') {
      return parsed as { requestId: string; fromDevice: string; deadline: number; op?: 'reset_password' | 'pair_new_device' };
    }
  } catch {
    // ignore
  }
  return null;
}

function writeInbound(rootId: string, value: typeof inbound.value) {
  if (typeof window === 'undefined' || !window.localStorage) return;
  if (value) {
    window.localStorage.setItem(storageKey(rootId), JSON.stringify(value));
  } else {
    window.localStorage.removeItem(storageKey(rootId));
  }
}

export function remainingMs(target?: number): number {
  if (target === undefined) return 0;
  return Math.max(0, target - now.value);
}

export function formatRemaining(ms: number): string {
  const totalSeconds = Math.ceil(ms / 1000);
  const hours = Math.floor(totalSeconds / 3600);
  const minutes = Math.floor((totalSeconds % 3600) / 60);
  const seconds = totalSeconds % 60;
  if (hours > 0) {
    return `${hours}小时${minutes.toString().padStart(2, '0')}分`;
  }
  if (minutes > 0) {
    return `${minutes}分${seconds.toString().padStart(2, '0')}秒`;
  }
  return `${seconds}秒`;
}

function recoveryErrorMessage(err: unknown): string {
  const msg = errorMessage(err);
  if (typeof msg === 'string') {
    if (msg.includes('TooEarly')) return '尚未到确认时间';
    if (msg.includes('RecoveryVetoed')) return '该请求已被否决';
    if (msg.includes('RecoveryPending')) return '已有进行中的恢复请求';
    if (msg.includes('RecoveryNotFound')) return '请求不存在或已结束';
    if (msg.includes('VetoWindowExpired')) return '否决窗口已过';
    if (msg.includes('UnsupportedOp')) return '该操作暂不支持';
  }
  return msg;
}

function ensureClock() {
  if (timer) return;
  now.value = Date.now();
  timer = setInterval(() => {
    now.value = Date.now();
  }, 1000);
}

export function stopRecoveryClock() {
  if (timer) {
    clearInterval(timer);
    timer = null;
  }
}

function recoveryApi() {
  return (typeof window !== 'undefined' && window.electronAPI?.recovery) || undefined;
}

export async function hydrateRecovery(rootId: string) {
  ensureClock();
  lastError.value = '';
  inbound.value = readInbound(rootId);
  const api = recoveryApi();
  if (!api) return;
  try {
    const status = await api.status();
    pending.value = status.pending ?? null;
    readyToConfirm.value = status.readyToConfirm ?? false;
  } catch (err) {
    lastError.value = recoveryErrorMessage(err);
  }
}

export async function initiateRecovery(op: 'reset_password' | 'pair_new_device' = 'reset_password', delayHours = DELAY_HOURS_DEFAULT) {
  lastError.value = '';
  const api = recoveryApi();
  if (!api) {
    lastError.value = '延迟恢复命令尚未接通';
    return;
  }
  try {
    await api.initiate(op, delayHours);
    ElMessage.success('延迟恢复请求已发起，请等待公示期结束');
  } catch (err) {
    lastError.value = recoveryErrorMessage(err);
    ElMessage.error(lastError.value);
    throw err;
  }
}

export async function confirmRecovery(requestId: string, newPassword: string) {
  lastError.value = '';
  const api = recoveryApi();
  if (!api) {
    lastError.value = '延迟恢复命令尚未接通';
    return;
  }
  try {
    await api.confirm(requestId, newPassword);
    ElMessage.success('密码已重置，建议重新导出备份二维码并重新备份助记词');
    pending.value = null;
    readyToConfirm.value = false;
  } catch (err) {
    lastError.value = recoveryErrorMessage(err);
    ElMessage.error(lastError.value);
    throw err;
  }
}

export async function vetoRecovery(rootId: string, requestId: string) {
  lastError.value = '';
  const api = recoveryApi();
  if (!api) {
    lastError.value = '延迟恢复命令尚未接通';
    return;
  }
  try {
    await api.veto(requestId);
    ElMessage.success('已否决该恢复请求');
    if (inbound.value?.requestId === requestId) {
      inbound.value = null;
      writeInbound(rootId, null);
    }
    if (pending.value?.requestId === requestId) {
      pending.value = { ...pending.value, vetoed: true, state: 'vetoed' };
    }
  } catch (err) {
    lastError.value = recoveryErrorMessage(err);
    ElMessage.error(lastError.value);
    throw err;
  }
}

export function clearInboundRecovery(rootId: string) {
  inbound.value = null;
  writeInbound(rootId, null);
}

export function handleRecoveryP2pEvent(rootId: string, event: Extract<P2pEventDto, { kind: 'RecoveryUpdated' }>['data']) {
  ensureClock();
  if (event.state === 'initiated') {
    inbound.value = {
      requestId: event.requestId,
      fromDevice: event.fromDevice,
      deadline: event.deadline ?? Date.now() + 24 * 60 * 60 * 1000,
      op: event.op,
    };
    writeInbound(rootId, inbound.value);
    ElMessageBox.alert(
      `设备『${event.fromDevice}』正在发起密码重置，若非本人操作请否决。`,
      '安全提醒',
      {
        confirmButtonText: '否决此操作',
        cancelButtonText: '稍后处理',
        showCancelButton: true,
        type: 'warning',
        beforeClose: (action, instance, done) => {
          if (action === 'confirm') {
            instance.confirmButtonLoading = true;
            vetoRecovery(rootId, event.requestId)
              .then(() => done())
              .catch(() => {
                instance.confirmButtonLoading = false;
              });
          } else {
            done();
          }
        },
      }
    );
    return;
  }

  if (event.state === 'vetoed' || event.state === 'committed') {
    if (inbound.value?.requestId === event.requestId) {
      inbound.value = null;
      writeInbound(rootId, null);
    }
    if (pending.value?.requestId === event.requestId) {
      pending.value = { ...pending.value, state: event.state };
      if (event.state === 'committed') {
        pending.value = null;
      }
    }
    ElMessage.info(event.state === 'vetoed' ? '该恢复请求已被否决' : '恢复请求已确认执行');
  }
}

export const pendingRecovery = computed(() => pending.value);
export const inboundRecovery = computed(() => inbound.value);
export const recoveryReadyToConfirm = computed(() => readyToConfirm.value);
export const recoveryLastError = computed(() => lastError.value);

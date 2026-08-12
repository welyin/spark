import { computed, ref } from 'vue';
import { ElMessage, ElMessageBox } from 'element-plus';
import type { P2pEventDto, PasswordUnifyStatusDto } from '../api/types';
import { errorMessage } from '../utils/ipc';

const SEEN_UNIFY_PREFIX = 'spark:pw-unify:';
const OUT_OF_GRACE_PREFIX = 'spark:pw-unify:grace:';

export type PendingUnify = {
  rotatedAt: number;
  rotatedBy: string;
  rotatedByDevice: string;
  reason: 'password_change' | 'password_reset';
};

const pendingUnify = ref<PendingUnify | null>(null);
const outOfGrace = ref<{ passwordChangedAt: number; graceMs: number } | null>(null);
const lastError = ref('');

function storageKey(prefix: string, rootId: string): string {
  return `${prefix}${rootId}`;
}

function readJson<T>(key: string): T | null {
  if (typeof window === 'undefined' || !window.localStorage) return null;
  try {
    const raw = window.localStorage.getItem(key);
    return raw ? (JSON.parse(raw) as T) : null;
  } catch {
    return null;
  }
}

function writeJson<T>(key: string, value: T | null): void {
  if (typeof window === 'undefined' || !window.localStorage) return;
  if (value === null) {
    window.localStorage.removeItem(key);
  } else {
    window.localStorage.setItem(key, JSON.stringify(value));
  }
}

export function isUnifyAcknowledged(rootId: string, rotatedAt: number): boolean {
  const ack = readJson<{ rotatedAt: number }>(storageKey(SEEN_UNIFY_PREFIX, rootId));
  return ack !== null && ack.rotatedAt >= rotatedAt;
}

function ackUnify(rootId: string, rotatedAt: number): void {
  writeJson(storageKey(SEEN_UNIFY_PREFIX, rootId), { rotatedAt });
}

export function isOutOfGraceAcknowledged(rootId: string): boolean {
  return readJson<{ ack: true }>(storageKey(OUT_OF_GRACE_PREFIX, rootId)) !== null;
}

function ackOutOfGrace(rootId: string): void {
  writeJson(storageKey(OUT_OF_GRACE_PREFIX, rootId), { ack: true });
}

export function getOutOfGraceText(graceMs: number): string {
  const days = Math.ceil(graceMs / (24 * 60 * 60 * 1000));
  return `已超 ${days} 天未同步`;
}

function unifyApi() {
  return typeof window !== 'undefined' && window.electronAPI?.passwordUnify
    ? window.electronAPI.passwordUnify
    : undefined;
}

function statusErrorMessage(err: unknown): string {
  const msg = errorMessage(err);
  if (msg.includes('ticket-mismatch')) {
    return '与设密设备上的新密码不一致';
  }
  if (msg.includes('ticket-unavailable')) {
    return '校验信息缺失';
  }
  if (msg.includes('invalid-password')) {
    return '当前密码不正确';
  }
  return msg;
}

export async function hydratePasswordUnify(rootId: string) {
  lastError.value = '';
  pendingUnify.value = null;
  const api = unifyApi();
  if (!api) return;
  try {
    const status: PasswordUnifyStatusDto = await api.status();
    if (status.pending && status.rotatedAt) {
      pendingUnify.value = {
        rotatedAt: status.rotatedAt,
        rotatedBy: '其他设备',
        rotatedByDevice: status.rotatedByDevice ?? '其他设备',
        reason: status.reason ?? 'password_change'
      };
    } else {
      pendingUnify.value = null;
    }
  } catch (err) {
    lastError.value = statusErrorMessage(err);
  }
}

export async function verifyPasswordTicket(password: string): Promise<{ ok: boolean; newPassword?: string }> {
  lastError.value = '';
  const api = unifyApi();
  if (!api) {
    lastError.value = 'password-unify 命令尚未接通';
    return { ok: false };
  }
  try {
    const result = await api.verifyTicket(password);
    if (result.ok) {
      return { ok: true, newPassword: password };
    }
    lastError.value = statusErrorMessage('ticket-mismatch');
    return { ok: false };
  } catch (err) {
    lastError.value = statusErrorMessage(err);
    return { ok: false };
  }
}

export async function unifyPassword(rootId: string, oldPassword: string, newPassword: string): Promise<boolean> {
  lastError.value = '';
  const api = unifyApi();
  if (!api) {
    lastError.value = 'password-unify 命令尚未接通';
    return false;
  }
  try {
    await api.unifyPassword(oldPassword, newPassword);
    ElMessage.success('密码已统一');
    ackUnify(rootId, pendingUnify.value?.rotatedAt ?? Date.now());
    pendingUnify.value = null;
    return true;
  } catch (err) {
    lastError.value = statusErrorMessage(err);
    ElMessage.error(lastError.value);
    return false;
  }
}

export function handlePasswordUnifyEvent(rootId: string, event: Extract<P2pEventDto, { kind: 'PasswordChangeObserved' | 'PasswordUnificationDone' | 'DeviceOutOfGrace' }>) {
  if (event.kind === 'PasswordChangeObserved') {
    pendingUnify.value = {
      rotatedAt: event.data.rotatedAt,
      rotatedBy: event.data.rotatedBy,
      rotatedByDevice: event.data.rotatedByDevice,
      reason: event.data.reason
    };

    if (isUnifyAcknowledged(rootId, event.data.rotatedAt)) return;

    if (event.data.reason === 'password_reset') {
      ElMessageBox.alert(
        `密码经延迟恢复通道于 ${formatTs(event.data.rotatedAt)} 在『${event.data.rotatedByDevice}』上被重置。若非本人操作，请立即检查设备管理与安全日志。`,
        '安全警示',
        { confirmButtonText: '知道了', type: 'warning' }
      );
    } else {
      ElMessage.warning(`密码于 ${formatTs(event.data.rotatedAt)} 在『${event.data.rotatedByDevice}』上被修改，请统一到新密码。`);
    }
    return;
  }

  if (event.kind === 'PasswordUnificationDone') {
    ackUnify(rootId, event.data.rotatedAt);
    pendingUnify.value = null;
    ElMessage.success('所有设备已完成密码统一');
    return;
  }

  if (event.kind === 'DeviceOutOfGrace') {
    outOfGrace.value = {
      passwordChangedAt: event.data.passwordChangedAt,
      graceMs: event.data.graceMs
    };
    if (isOutOfGraceAcknowledged(rootId)) return;
    ackOutOfGrace(rootId);
    const text = getOutOfGraceText(event.data.graceMs);
    ElMessageBox.alert(
      `本设备已超过 ${text.replace('已超 ', '').replace(' 未同步', '')} 未同步新密码，部分数据可能无法解密。建议立即在「设置 → 安全设置 → 统一为新密码」中完成密码统一。`,
      '设备已超期',
      { confirmButtonText: '知道了', type: 'warning' }
    );
  }
}

export function isDeviceOutOfGrace(): boolean {
  return outOfGrace.value !== null;
}

export function clearOutOfGrace(): void {
  outOfGrace.value = null;
}

export function formatTs(ts: number): string {
  return new Date(ts).toLocaleString('zh-CN', { month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit' });
}

export function ticketMismatchText(pending: PendingUnify): string {
  const when = formatTs(pending.rotatedAt);
  return [
    `与『${pending.rotatedByDevice}』于 ${when} 设的新密码不一致。`,
    '请输入那台设备上设的新密码。',
    '想不起来？可先用旧密码登录，或在设密设备上重新修改密码。'
  ].join('\n');
}

export const pendingUnifyRef = computed(() => pendingUnify.value);
export const outOfGraceRef = computed(() => outOfGrace.value);
export const passwordUnifyLastError = computed(() => lastError.value);

export function clearPasswordUnifyPending(): void {
  pendingUnify.value = null;
}

export function clearPasswordUnifyLastError(): void {
  lastError.value = '';
}

export function setPendingUnifyForTest(value: PendingUnify | null): void {
  pendingUnify.value = value;
}

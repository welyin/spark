// SecurityModule（安全设置）渲染回归：修改密码表单校验 + changePassword 成功/失败链路。
// 对齐 login-enter.render 的挂载风格：createApp + ElementPlus，jsdom 真 DOM 输入。
// 覆盖：
// - canSubmit 门控（旧口令非空 && 新口令>=8 位 && 两遍一致）
// - submitChangePassword 各校验分支（缺旧口令/新口令过短/两遍不一致）→ 错误提示
// - 调 window.electronAPI.rootIdentity.changePassword 成功：清空输入 + 成功提示
// - 失败（InvalidPassword）：错误文案透传
// - 自动锁定保存：setAutoLockDays 落 localStorage + 成功提示
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createApp, h } from 'vue';
import ElementPlus, { ElMessage } from 'element-plus';
import SecurityModule from '../../components/mine/SecurityModule.vue';
import { getAutoLockDays } from '../../utils/auto-lock';

function mount(): HTMLElement {
  const host = document.createElement('div');
  document.body.appendChild(host);
  const app = createApp({ render: () => h(SecurityModule) });
  app.use(ElementPlus);
  app.mount(host);
  return host;
}

function inputs(host: HTMLElement): HTMLInputElement[] {
  return Array.from(host.querySelectorAll('.security-form input[type="password"]')) as HTMLInputElement[];
}

async function typeInput(input: HTMLInputElement, value: string): Promise<void> {
  input.value = value;
  input.dispatchEvent(new Event('input', { bubbles: true }));
  await new Promise((resolve) => setTimeout(resolve, 0));
}

/** 从当前聚焦的密码框触发 Enter 提交（SecurityModule 用 @keyup.enter） */
async function pressEnter(input: HTMLInputElement): Promise<void> {
  input.dispatchEvent(new KeyboardEvent('keyup', { key: 'Enter', bubbles: true, cancelable: true }));
  await new Promise((resolve) => setTimeout(resolve, 0));
}

function alertText(host: HTMLElement): string {
  return host.querySelector('.el-alert__title')?.textContent ?? '';
}

function submitButton(host: HTMLElement): HTMLButtonElement {
  return host.querySelector('.security-form-actions button') as HTMLButtonElement;
}

describe('SecurityModule 修改密码', () => {
  let changePassword: ReturnType<typeof vi.fn>;
  let successSpy: ReturnType<typeof vi.fn>;
  let host: HTMLElement;

  beforeEach(() => {
    localStorage.clear();
    changePassword = vi.fn().mockResolvedValue({ success: true });
    (window as any).electronAPI = {
      rootIdentity: { changePassword }
    };
    // spyOn 返回的 SpyInstance 与 vi.fn 的 Mock 类型不匹配，这里仅作 spy 使用（断言 toHaveBeenCalledWith）
    successSpy = vi.spyOn(ElMessage, 'success').mockImplementation(() => ({}) as never) as unknown as ReturnType<typeof vi.fn>;
    host = mount();
  });

  afterEach(() => {
    host.remove();
    vi.restoreAllMocks();
  });

  const OLD = 'current-pass-1';
  const NEW = 'new-pass-123';

  it('表单就绪：canSubmit 允许提交，三处输入按 旧/新/确认 顺序渲染', async () => {
    const [old, pw, confirm] = inputs(host);
    expect(submitButton(host).disabled).toBe(true);
    await typeInput(old, OLD);
    expect(submitButton(host).disabled).toBe(true); // 新密码空
    await typeInput(pw, NEW);
    expect(submitButton(host).disabled).toBe(true); // 确认不匹配
    await typeInput(confirm, NEW);
    expect(submitButton(host).disabled).toBe(false); // 两遍一致 → 可提交
  });

  it('新密码两遍不一致：禁止提交 + 提示错误文案', async () => {
    const [old, pw, confirm] = inputs(host);
    await typeInput(old, OLD);
    await typeInput(pw, NEW);
    await typeInput(confirm, 'different-pass-9');
    expect(submitButton(host).disabled).toBe(true);
    await pressEnter(confirm);
    expect(alertText(host)).toBe('两次输入的新密码不一致');
    expect(changePassword).not.toHaveBeenCalled();
  });

  it('新密码强度不足（<8 位）：禁止提交 + 提示', async () => {
    const [old, pw, confirm] = inputs(host);
    await typeInput(old, OLD);
    await typeInput(pw, 'short');
    await typeInput(confirm, 'short');
    expect(submitButton(host).disabled).toBe(true);
    await pressEnter(confirm);
    expect(alertText(host)).toBe('新密码至少 8 位');
    expect(changePassword).not.toHaveBeenCalled();
  });

  it('缺当前密码：提示且不调 changePassword', async () => {
    const [, pw, confirm] = inputs(host);
    await typeInput(pw, NEW);
    await typeInput(confirm, NEW);
    expect(submitButton(host).disabled).toBe(true); // 旧口令空
    await pressEnter(confirm);
    expect(alertText(host)).toBe('请填写当前密码');
    expect(changePassword).not.toHaveBeenCalled();
  });

  it('改密成功：调 changePassword(old, new)，清空输入，成功提示', async () => {
    const [old, pw, confirm] = inputs(host);
    await typeInput(old, OLD);
    await typeInput(pw, NEW);
    await typeInput(confirm, NEW);
    await pressEnter(confirm);

    expect(changePassword).toHaveBeenCalledTimes(1);
    expect(changePassword).toHaveBeenCalledWith(OLD, NEW);
    expect(successSpy).toHaveBeenCalledWith('密码已修改，建议重新导出备份二维码');
    // 成功后清空三个输入
    expect(inputs(host).map((i) => i.value)).toEqual(['', '', '']);
  });

  it('改密失败（InvalidPassword）：错误文案透传，不清空输入', async () => {
    changePassword.mockRejectedValue(new Error("Error invoking remote method 'root-change-password': Error: Invalid password"));
    const [old, pw, confirm] = inputs(host);
    await typeInput(old, OLD);
    await typeInput(pw, NEW);
    await typeInput(confirm, NEW);
    await pressEnter(confirm);

    expect(alertText(host)).toBe('修改失败：Invalid password');
    expect(successSpy).not.toHaveBeenCalled();
    // 失败保留输入，便于修正重试
    expect(inputs(host).map((i) => i.value)).toEqual([OLD, NEW, NEW]);
  });
});

describe('SecurityModule N 天自动锁定', () => {
  let host: HTMLElement;
  let successSpy: ReturnType<typeof vi.fn>;

  beforeEach(() => {
    localStorage.clear();
    (window as any).electronAPI = { rootIdentity: {} };
    // spyOn 返回的 SpyInstance 与 vi.fn 的 Mock 类型不匹配，这里仅作 spy 使用（断言 toHaveBeenCalledWith）
    successSpy = vi.spyOn(ElMessage, 'success').mockImplementation(() => ({}) as never) as unknown as ReturnType<typeof vi.fn>;
  });

  afterEach(() => {
    host?.remove();
    vi.restoreAllMocks();
  });

  it('选择档位保存到 localStorage 并提示', async () => {
    host = mount();
    // 切换到「自动锁定」详情
    const lockItem = Array.from(host.querySelectorAll('.mine-list-item')).find((el) =>
      el.textContent?.includes('自动锁定')
    ) as HTMLButtonElement;
    lockItem.click();
    await new Promise((resolve) => setTimeout(resolve, 0));

    const radio7 = Array.from(host.querySelectorAll('.el-radio')).find((el) =>
      el.textContent?.includes('7 天')
    ) as HTMLElement;
    radio7.click();
    await new Promise((resolve) => setTimeout(resolve, 0));

    expect(getAutoLockDays()).toBe(7);
    expect(successSpy).toHaveBeenCalledWith('已开启 7 天自动锁定');
  });

  it('关闭档位保存为 0', async () => {
    localStorage.setItem('spark.settings.autoLockDays', '30');
    host = mount();
    const lockItem = Array.from(host.querySelectorAll('.mine-list-item')).find((el) =>
      el.textContent?.includes('自动锁定')
    ) as HTMLButtonElement;
    lockItem.click();
    await new Promise((resolve) => setTimeout(resolve, 0));

    const closeRadio = Array.from(host.querySelectorAll('.el-radio')).find((el) =>
      el.textContent?.includes('关闭')
    ) as HTMLElement;
    closeRadio.click();
    await new Promise((resolve) => setTimeout(resolve, 0));

    expect(getAutoLockDays()).toBe(0);
    expect(successSpy).toHaveBeenCalledWith('已关闭自动锁定');
  });
});

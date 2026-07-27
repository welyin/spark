/**
 * 当前选中组织（模块级单例 ref）。
 *
 * OrgPage 在选择/刷新组织列表时写入；顶栏网络状态（NetworkStatusBar）读取，
 * 避免跨页面组件层层 props/emit。
 */
import { ref } from 'vue';

export const currentOrgId = ref<string>('');

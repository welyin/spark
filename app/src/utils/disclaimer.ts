// 免责声明：首次启动须同意后进入（RootGate 弹窗），系统设置常驻展示（SystemSettingsPanel「免责声明」）。
// 文案与仓库 README「免责声明」一节保持一致；实质性修改文案时递增 DISCLAIMER_VERSION，
// 已同意旧版本的用户会在下次启动时重新确认。
const STORAGE_KEY = 'spark.disclaimer.acceptedVersion';

/** 免责声明当前版本（用户 localStorage 中记录的是已同意的版本号） */
export const DISCLAIMER_VERSION = 1;

export const DISCLAIMER_TITLE = '免责声明';

export const DISCLAIMER_PARAGRAPHS: string[] = [
  '星火（Spark）是一款开源的分布式协作工具，仅用于合法的基层社区自治、业主公共事务协商等合规场景，严禁用于任何违反法律法规的活动。',
  '本软件采用去中心化架构：您的数据仅存储于您自己的设备与您所在组织成员的设备上，开发者不运营任何服务器，不收集、存储或接触您的任何数据。',
  '本软件按「现状」提供，不作任何明示或默示的担保。使用本软件所产生的一切行为与后果，由使用者自行承担，项目开发者不承担相关法律责任。',
  '请使用者严格遵守所在地区的法律法规与物业管理相关规定，依法依规开展自治活动。'
];

export function isDisclaimerAccepted(): boolean {
  if (typeof window === 'undefined' || !window.localStorage) {
    return false;
  }
  return window.localStorage.getItem(STORAGE_KEY) === String(DISCLAIMER_VERSION);
}

export function markDisclaimerAccepted(): void {
  if (typeof window === 'undefined' || !window.localStorage) {
    return;
  }
  window.localStorage.setItem(STORAGE_KEY, String(DISCLAIMER_VERSION));
}

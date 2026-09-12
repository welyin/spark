/**
 * 哈希渐变色板（与 UserAvatar 自动头像同一套配色），
 * 供应用图标等按稳定 seed（如插件 id）取色，同一 seed 恒得同一配色。
 */
const PALETTES: Array<[string, string]> = [
  ['#3296fa', '#2b83dd'],
  ['#7b61ff', '#5a3fd6'],
  ['#00b8a9', '#008577'],
  ['#f7b500', '#e08600'],
  ['#f54a45', '#cf352f'],
  ['#eb2f96', '#c41d7f'],
  ['#34c19b', '#1f9c7c'],
  ['#ff7d00', '#e56a00']
];

function hashOf(seed: string): number {
  let hash = 0;
  for (const char of seed || 'spark') {
    hash = (hash * 31 + (char.codePointAt(0) ?? 0)) >>> 0;
  }
  return hash;
}

export function hashGradient(seed: string): string {
  const [from, to] = PALETTES[hashOf(seed) % PALETTES.length];
  return `linear-gradient(135deg, ${from}, ${to})`;
}

/** D16 每空间默认主题色：同一 seed 恒得同一色（取渐变板主色），
    空间未自定义主题色时按 spaceId 派生，保证每空间独立且稳定 */
export function hashColor(seed: string): string {
  return PALETTES[hashOf(seed) % PALETTES.length][0];
}

/**
 * SBOM 生成（plugin-dist §9.3）：安装包内置 sbom.json + 更新清单 kind:"sbom" 资产。
 * 每个依赖记录仓库锚定（repo + commit）与内容哈希（vendor 树 sha256），
 * 安装时依赖树展示（壳层后续工作）以此为数据源。
 */

export const SBOM_VERSION = 1;
export const SBOM_FILE_NAME = 'sbom.json';
export const TOOL_NAME = 'spark-plugin-cli';

/**
 * @param {object} input
 * @param {string} input.pluginId
 * @param {string} input.version
 * @param {string} input.toolVersion
 * @param {Array<{repo, commit, hash}>} input.libraries  manifest 规范化后的依赖
 * @param {Map<string, {files: number}>} [input.lockInfo]  verifyLock 的返回值（可选）
 */
export function buildSbom({ pluginId, version, toolVersion, libraries, lockInfo }) {
  return {
    sbomVersion: SBOM_VERSION,
    plugin: { id: pluginId, version },
    generatedAt: new Date().toISOString(),
    tool: { name: TOOL_NAME, version: toolVersion },
    libraries: libraries.map((library) => ({
      repo: library.repo,
      commit: library.commit,
      hash: library.hash,
      files: lockInfo?.get(library.repo)?.files ?? null
    }))
  };
}

export function renderSbom(sbom) {
  return JSON.stringify(sbom, null, 2) + '\n';
}

// DSH-PackForge core 公共 API（供 CLI / GUI / 插件以编程方式调用）。
export { Host } from './host.js';
export { isExcluded } from './security.js';
export { scanProfile, selectFiles } from './scan.js';
export { summarizeSpecial } from './special.js';
export {
  buildManifest,
  buildHomeManifest,
  validateManifest,
  validateFileEntry,
  coordsToPkgDeps,
  pkgDepsToCoords,
  coordinatesFromProfileDeps,
  gitCommitFromLock,
  isExactSemver,
  parseGitCoord,
  parsePkgGitSpec,
  resolveLocale,
  isLocaleString,
  extractBundles,
  extractDependencies,
  sanitizeSlug,
} from './manifest.js';
export {
  discoverProfiles,
  discoverHomes,
  listInstalledDshVersions,
  resolveProfileInput,
  skipProfileDir,
  compareVersions,
  sortVersionsDesc,
} from './discovery.js';
export { buildDspack, parseDspack, encodeText, decodeText, dspackMarker, DSPACK_FORMAT, DSPACK_CONTAINER_VERSION } from './dspack.js';
export { packProfile, packHome, dspackEntryPath, summarizeHome } from './pack.js';
export { exportRepo, ReleaseConflictError, renderReadme, renderDspackIgnore, renderGitignore, REPO_CONTENT_LEVELS, REPO_CONTENT_LABEL } from './repo.js';
export { loadWorkspaceConfig, exportFromWorkspace, exportHomeFromWorkspace } from './workspace.js';
export { inspectProfile, inspectHome, inspectPack } from './inspect.js';
export { installPack, verifyIntegrity, reconcileProfile, resolvePackSource } from './install.js';
export { readMarketIndex, fetchMarketPackDetail, normalizeMarketPack, packDirId, DEFAULT_MARKET_INDEX } from './market.js';
export { discoverExtensions } from './extensions.js';
export { exportChoices } from './inspect.js';

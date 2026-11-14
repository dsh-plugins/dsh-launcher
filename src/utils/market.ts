import semver from 'semver'
import { computed, ref, watch } from 'vue'
import type { Ref } from 'vue'
import type { MarketPlugin, PluginVersionInfo } from '../api/types'
import { normalizeRange } from './version-range.ts'

/** 市场字段缺失表示未限制版本，无法解析的声明需要展示具体原因。 */
export function compatibility(range: unknown, version: string) {
  try {
    const normalized = normalizeRange(range)
    if (!semver.valid(version)) {
      return { status: 'unknown' as const, reason: version }
    }
    return {
      status: semver.satisfies(version, normalized, { includePrerelease: true })
        ? 'compatible' as const : 'incompatible' as const,
      reason: normalized,
    }
  } catch (error) {
    return { status: 'unknown' as const, reason: String(error) }
  }
}

/** 确认只适用于当前插件、版本与目标实例，切换选择时必须重新确认。 */
export function useCompatibilityAck(selection: Readonly<Ref<string>>) {
  const acceptedSelection = ref<string | null>(null)
  watch(selection, () => { acceptedSelection.value = null }, { flush: 'sync' })
  return computed({
    get: () => acceptedSelection.value === selection.value,
    set: (accepted: boolean) => { acceptedSelection.value = accepted ? selection.value : null },
  })
}

/** 原始区间分段显示，避免将多个并列区间挤在一个标签中。 */
export function rangeParts(value: unknown): string[] {
  if (Array.isArray(value)) {
    return value.map(String)
  }
  return typeof value === 'string' ? value.split(';').map((part) => part.trim()) : []
}

/** 官方目录固定安装来源，版本选择直接使用索引快照。 */
export function indexedVersions(plugin: MarketPlugin): PluginVersionInfo[] | null {
  if (!plugin.catalog) {
    return null
  }
  return plugin.catalog.versions.map((entry) => ({
    version: entry.version,
    channel: semver.prerelease(entry.version) ? 'beta' : 'stable',
    published_at: new Date(entry.publishTimestamp * 1000).toISOString(),
    label: entry.source.type === 'npm' ? entry.source.registry ?? 'npm'
      : `${entry.source.url}#${entry.source.commit.slice(0, 7)}${entry.source.path ? `/${entry.source.path}` : ''}`,
    is_default: false,
    install_source: entry.source,
    support_versions: entry.engines?.dsh,
  }))
}

/** 官方本地化字段使用小写语言键，同时支持语言地区和基础语言。 */
export function localizedText(value: string | Record<string, string> | undefined, locale: string): string {
  if (typeof value === 'string') {
    return value
  }
  const language = locale.toLowerCase()
  return value?.[language] ?? value?.[language.split('-')[0]!] ?? value?.en ?? ''
}

/** 插件名称可能是单语言文本或语言映射，显示时按当前语言取值。 */
export function pluginName(name: string | Record<string, string> | undefined, locale: string): string {
  if (!name) {
    return ''
  }
  if (typeof name === 'string') {
    return name
  }
  return localizedText(name, locale) || Object.values(name)[0] || ''
}

/** 名称的全部语言取值，搜索需要覆盖当前界面语言以外的文本。 */
export function nameValues(name: string | Record<string, string> | undefined): string[] {
  if (!name) {
    return []
  }
  return typeof name === 'string' ? [name] : Object.values(name)
}

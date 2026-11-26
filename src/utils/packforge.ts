import type { PackforgeText } from '@/api/types'

export function packforgeText(value: PackforgeText | null | undefined, locale: string): string {
  if (!value) {
    return ''
  }
  if (typeof value === 'string') {
    return value
  }
  const keys = Object.keys(value)
  const match = (language: string) => keys.find(key => key.toLowerCase() === language.toLowerCase())
  const family = (language: string) => keys.find(key => key.toLowerCase().split('-')[0] === language)
  const key = match(locale) ?? family(locale.toLowerCase().split('-')[0])
    ?? match('en') ?? family('en') ?? match('zh') ?? family('zh') ?? keys[0]
  return key === undefined ? '' : value[key]
}

export function packforgeIcon(icon: string | undefined, icons: Record<string, string>): string | undefined {
  if (!icon) {
    return undefined
  }
  if (icon.startsWith('assets/plugins/')) {
    const data = icons[icon]
    return typeof data === 'string' && /^data:image\/(?:svg\+xml|png|jpeg|webp);base64,[A-Za-z0-9+/]+={0,2}$/.test(data)
      ? data : undefined
  }
  try {
    const url = new URL(icon)
    if (url.protocol === 'https:' && !url.username && !url.password) {
      if (url.origin === 'https://cdn.jsdelivr.com' && url.pathname.startsWith('/npm/') && !url.search && !url.hash) {
        url.hostname = 'cdn.jsdelivr.net'
      }
      return url.href
    }
  } catch {
    return undefined
  }
  return undefined
}

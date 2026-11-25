import type { ProxyEndpoint } from '@/api/types'

/** 地址与端口分字段保存，凭据只通过专用字段提交。 */
export function validProxyEndpoint(endpoint: ProxyEndpoint): boolean {
  if (!Number.isInteger(endpoint.port) || endpoint.port < 1 || endpoint.port > 65535) {
    return false
  }
  try {
    const url = new URL(endpoint.url.trim())
    return ['http:', 'https:', 'socks5:', 'socks5h:'].includes(url.protocol) && !!url.hostname
      && (!url.port || Number(url.port) === endpoint.port) && !url.username && !url.password
      && (url.pathname === '/' || url.pathname === '') && !url.search && !url.hash
  } catch (error) {
    if (error instanceof TypeError) { return false }
    throw error
  }
}

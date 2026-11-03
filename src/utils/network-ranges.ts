/** 检查用户填写的 CIDR，并识别多条规则合起来放行整个地址族的情况。 */
export function validNetworkRanges(entries: string[]): boolean {
  if (!entries.length) { return false }
  const ranges = new Map<number, [bigint, bigint][]>()
  for (const entry of entries) {
    const match = /^([0-9a-f:.]+)\/(\d+)$/i.exec(entry)
    if (!match) { return false }
    const [, address, length] = match
    const bits = address.includes(':') ? 128 : 32
    const prefix = Number(length)
    if (!prefix || prefix > bits) { return false }
    let words: number[]
    if (bits === 32) {
      const parts = address.split('.')
      if (parts.length !== 4 || parts.some((part) => !/^(0|[1-9]\d{0,2})$/.test(part) || Number(part) > 255)) {
        return false
      }
      words = parts.map(Number)
    } else {
      // URL 解析器会校验 IPv6 语法，并将内嵌 IPv4 转换为十六进制。
      let host: string
      try {
        host = new URL('http://[' + address + ']/').hostname.slice(1, -1)
      } catch {
        return false
      }
      const [left, right] = host.split('::').map((side) => side ? side.split(':') : [])
      const groups = right ? [...left, ...Array<string>(8 - left.length - right.length).fill('0'), ...right] : left
      words = groups.map((word) => Number.parseInt(word, 16))
    }
    const width = bits === 32 ? 8n : 16n
    const ip = words.reduce((value, word) => (value << width) + BigInt(word), 0n)
    const hostBits = BigInt(bits - prefix)
    const base = (ip >> hostBits) << hostBits
    const group = ranges.get(bits) ?? []
    group.push([base, base + (1n << hostBits)])
    ranges.set(bits, group)
  }
  for (const [bits, group] of ranges) {
    group.sort(([left], [right]) => left < right ? -1 : left > right ? 1 : 0)
    let covered = 0n
    for (const [start, end] of group) {
      if (start > covered) { break }
      if (end > covered) { covered = end }
    }
    if (covered === (1n << BigInt(bits))) { return false }
  }
  return true
}

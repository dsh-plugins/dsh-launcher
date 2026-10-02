/** Shared presentation rules for the provider pre-launch self-check.
 *
 * The same report is rendered in two places: the instance settings page
 * (`InstanceEdit.vue`, an `a-alert` per route) and the early-loading window
 * (`ProviderReport.vue`, an `a-tag` per route). They must agree — a `warn`
 * that is orange on one surface and grey on the other reads as two different
 * severities. The two components use different Arco widgets with different
 * color vocabularies, so this module owns the mapping from a report status to
 * *each* widget's value, derived from one shared severity table.
 *
 * Adding a status means editing `SEVERITY` only; both call sites follow.
 */

/** Report status as emitted by `providers::check_provider_routes`. */
export type ProviderCheckStatus = 'ok' | 'warn' | 'unknown'

/** How urgent a status is; higher wins when a route aggregates its checks. */
const SEVERITY: Record<ProviderCheckStatus, number> = {
  ok: 0,
  unknown: 1,
  warn: 2,
}

/** Normalizes an arbitrary status string from the backend to a known one. */
export function providerCheckStatus(status: string): ProviderCheckStatus {
  return status === 'warn' || status === 'unknown' ? status : 'ok'
}

/**
 * `a-tag` color for a status. `a-alert` (settings page) has no direct
 * equivalent palette, so the tag colors are chosen to read as the same
 * severity: green / orange / blue. `gray` is deliberately avoided for
 * `unknown` — grey reads as "disabled or not applicable" and would demote
 * exactly the results the user most needs to read.
 */
export function providerCheckTagColor(status: string): string {
  switch (providerCheckStatus(status)) {
    case 'warn':
      return 'orange'
    case 'unknown':
      return 'blue'
    default:
      return 'green'
  }
}

/** i18n key suffix for the status label (`...providerCheckStatus<suffix>`). */
export function providerCheckStatusKeySuffix(status: string): 'Warn' | 'Unknown' | 'Ok' {
  switch (providerCheckStatus(status)) {
    case 'warn':
      return 'Warn'
    case 'unknown':
      return 'Unknown'
    default:
      return 'Ok'
  }
}

/**
 * i18n key for a check item's message. The check `code` comes from the
 * backend and is not type-checked against the locale files, so a status added
 * there before its translation lands would otherwise render as a bare key.
 * Pair with {@link translateProviderCheck} to fall back to the raw code.
 */
export function providerCheckMessageKey(code: string): string {
  return `instanceEdit.providerChecks.${code}`
}

/**
 * Renders a check message, falling back to the raw `code` when the locale has
 * no entry for it. `te` is vue-i18n's "translation exists" check.
 */
export function translateProviderCheck(
  t: (key: string, named?: Record<string, string>) => string,
  te: (key: string) => boolean,
  code: string,
  params: Record<string, string>,
): string {
  const key = providerCheckMessageKey(code)
  return te(key) ? t(key, params) : code
}

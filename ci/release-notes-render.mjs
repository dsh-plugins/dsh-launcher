// Release-note renderer + artifact classifier.
// Shared by ci/update-release-notes.sh (CLI) and ci/release-notes.test.mjs (tests).

/**
 * Maps a release asset file name to a { platform, arch, kind, name } record,
 * or null when the name does not match any known artifact.
 */
export function classify(name) {
  // RPM uses a different scheme: dsh-launcher-0.1.0-1.x86_64.rpm
  const rpm = /^dsh-launcher-\d[\w.-]*\.(x86_64|aarch64)\.rpm$/.exec(name)
  if (rpm) {
    return { name, arch: rpm[1] === 'x86_64' ? 'x86_64' : 'arm64', kind: 'rpm' }
  }
  const m = /^dsh-launcher_\d[\w.-]*_(x64|arm64|amd64|aarch64).*\.(exe|msi|AppImage|deb|rpm|dmg)$/.exec(name)
  if (!m) return null
  const archRaw = m[1]
  return {
    name,
    arch: archRaw === 'amd64' || archRaw === 'x64' ? 'x86_64' : 'arm64',
    kind: m[2],
  }
}

const PLATFORM_OF_KIND = { exe: 'Windows', msi: 'Windows', AppImage: 'Linux', deb: 'Linux', rpm: 'Linux', dmg: 'macOS' }
const KIND_LABEL_EN = { exe: 'NSIS setup', msi: 'MSI', AppImage: 'AppImage', deb: 'DEB', rpm: 'RPM', dmg: 'DMG' }
const KIND_ORDER = { exe: 0, msi: 1, AppImage: 2, deb: 3, rpm: 4, dmg: 5 }

/**
 * Renders the English-only "Downloads" table plus the "What's Changed"
 * commit list. Every artifact row links directly to its release-asset
 * download URL. With `assets = null` (commits-only mode, used for the
 * initial release body before packaging finishes) the Downloads section is
 * omitted and a preparation note is rendered instead.
 */
export function render(tag, assets, commits, { repo = 'REPO' } = {}) {
  const commitLines = (commits ?? [])
    .map((c) => `* ${c}`)
    .join('\n')

  if (assets === null) {
    return `_Artifacts are being prepared; the downloads table will appear here once packaging finishes._

---

## What's Changed

${commitLines}
`
  }

  const sorted = [...assets]
    .filter((a) => classify(a))
    .sort((a, b) => {
      const c1 = classify(a)
      const c2 = classify(b)
      if (c1.arch !== c2.arch) return c1.arch.localeCompare(c2.arch)
      return KIND_ORDER[c1.kind] - KIND_ORDER[c2.kind]
    })

  const link = (name) => `https://github.com/${repo}/releases/download/${tag}/${name}`
  const rows = sorted.map((name) => {
    const c = classify(name)
    return `| ${PLATFORM_OF_KIND[c.kind]} | ${c.arch} | [${name}](${link(name)}) (${KIND_LABEL_EN[c.kind]}) |`
  })

  return `## Downloads

| Platform | Architecture | File |
| --- | --- | --- |
${rows.join('\n')}

---

## What's Changed

${commitLines}
`
}

// CLI mode:
//   node ci/release-notes-render.mjs <tag> <assets-json> <commits-file>
//   node ci/release-notes-render.mjs <tag> - <commits-file>   (commits-only)
import { fileURLToPath, pathToFileURL } from 'node:url'
import { readFileSync } from 'node:fs'
if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const [, , tag, assetsJson, commitsFile] = process.argv
  const assets = assetsJson === '-' ? null : JSON.parse(assetsJson)
  const commits = commitsFile
    ? readFileSync(commitsFile, 'utf8').split('\n').map((s) => s.trim()).filter(Boolean)
    : []
  const repo = process.env.GITHUB_REPOSITORY ?? 'REPO'
  if (assets !== null) {
    const unknown = assets.filter((a) => !classify(a))
    if (unknown.length > 0) {
      console.error(`release-note classifier rejected assets: ${unknown.join(', ')}`)
      process.exit(1)
    }
  }
  process.stdout.write(render(tag, assets, commits, { repo }))
}

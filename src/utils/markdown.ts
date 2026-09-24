import DOMPurify from 'dompurify'
import { Marked } from 'marked'

// Shared markdown renderer for in-app previews (issue #57 AGENTS.md editor).
// GFM + soft line breaks; output is sanitized against XSS before v-html.
const marked = new Marked({ gfm: true, breaks: true })

/** Renders markdown to sanitized HTML safe for v-html. */
export function renderMarkdown(md: string): string {
  if (!md.trim()) return ''
  return DOMPurify.sanitize(marked.parse(md) as string)
}

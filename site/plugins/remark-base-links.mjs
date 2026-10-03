// Pages is served from a sub-path (`/agent-sessions-manager/docs`), and Astro
// does not prefix `base` onto links written in Markdown. This does, so pages
// can link as `/guides/cli/` — the form that reads naturally and survives the
// site moving. Only site-absolute links are touched; `https://…`, `#anchor`,
// `./relative` and links that already carry the base are left alone.
import { visit } from 'unist-util-visit'

export function remarkBaseLinks({ base }) {
  const prefix = base.replace(/\/$/, '')
  return () => (tree) => {
    visit(tree, ['link', 'definition'], (node) => {
      const url = node.url
      if (typeof url !== 'string') return
      if (!url.startsWith('/') || url.startsWith('//')) return
      if (url === prefix || url.startsWith(prefix + '/')) return
      node.url = prefix + url
    })
  }
}

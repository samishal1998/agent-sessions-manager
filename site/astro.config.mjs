// @ts-check
import { defineConfig } from 'astro/config'
import starlight from '@astrojs/starlight'
import starlightLinksValidator from 'starlight-links-validator'
import starlightImageZoom from 'starlight-image-zoom'
import mermaid from 'astro-mermaid'
import { remarkBaseLinks } from './plugins/remark-base-links.mjs'

// The landing page lives at the root of the same Pages site; the docs are
// built into its `docs/` directory, so everything is served from this base.
const SITE = 'https://samishal1998.github.io'
const BASE = '/agent-sessions-manager/docs'

export default defineConfig({
  site: SITE,
  base: BASE,
  trailingSlash: 'always',
  markdown: { remarkPlugins: [remarkBaseLinks({ base: BASE })] },
  integrations: [
    // Diagrams are written as ```mermaid blocks and drawn in the browser, in
    // whichever theme the reader has chosen.
    mermaid({ theme: 'neutral', autoTheme: true }),
    starlight({
      title: 'asm',
      description:
        'One place to browse, search, move and sync the coding-agent sessions on every machine you use.',
      logo: { src: './src/assets/logo.svg', alt: '' },
      favicon: '/favicon.svg',
      customCss: ['./src/styles/theme.css'],
      social: [
        { icon: 'github', label: 'GitHub', href: 'https://github.com/samishal1998/agent-sessions-manager' },
      ],
      editLink: {
        baseUrl: 'https://github.com/samishal1998/agent-sessions-manager/edit/main/site/',
      },
      lastUpdated: true,
      tableOfContents: { minHeadingLevel: 2, maxHeadingLevel: 3 },
      head: [
        { tag: 'meta', attrs: { name: 'theme-color', content: '#101216' } },
      ],
      plugins: [
        // A broken internal link fails the build rather than reaching a reader.
        starlightLinksValidator({ errorOnRelativeLinks: false }),
        starlightImageZoom(),
      ],
      sidebar: [
        { label: '← asm home', link: SITE + '/agent-sessions-manager/', attrs: { class: 'home-link' } },
        {
          label: 'Start here',
          items: [
            { slug: 'start/install' },
            { slug: 'start/quickstart' },
            { slug: 'start/concepts' },
          ],
        },
        {
          label: 'Guides',
          items: [
            { slug: 'guides/cli' },
            { slug: 'guides/tui' },
            { slug: 'guides/web-ui' },
            { slug: 'guides/search' },
            { slug: 'guides/organize' },
            { slug: 'guides/import' },
            { slug: 'guides/reply' },
          ],
        },
        {
          label: 'Multiple machines',
          items: [
            { slug: 'hub/overview' },
            { slug: 'hub/setup' },
            { slug: 'hub/sync' },
            { slug: 'hub/restore' },
            { slug: 'hub/daemon' },
            { slug: 'hub/admin' },
            { slug: 'hub/security' },
          ],
        },
        {
          label: 'Concepts',
          items: [
            { slug: 'concepts/architecture' },
            { slug: 'concepts/safety' },
            { slug: 'concepts/projects' },
            { slug: 'concepts/sync-model' },
            { slug: 'concepts/indexing' },
          ],
        },
        {
          label: 'Reference',
          items: [
            { label: 'Command line', collapsed: true, items: [{ autogenerate: { directory: 'reference/cli' } }] },
            { slug: 'reference/tui-keys' },
            { slug: 'reference/web-api' },
            { slug: 'reference/agents' },
            { slug: 'reference/data-files' },
            { slug: 'reference/ir-schema' },
            { slug: 'reference/agent-formats' },
          ],
        },
        { label: 'About', items: [{ slug: 'about/changelog' }] },
      ],
    }),
  ],
})

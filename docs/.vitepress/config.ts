import { defineConfig } from 'vitepress'

export default defineConfig({
  lang: 'en-US',
  title: 'komentoj',
  description: 'Lightweight ActivityPub comment server for static blogs',
  base: '/komentoj/',

  lastUpdated: true,
  cleanUrls: true,

  head: [
    ['link', { rel: 'icon', href: '/favicon.svg', type: 'image/svg+xml' }],
  ],

  themeConfig: {
    logo: '/favicon.svg',
    siteTitle: 'komentoj',

    nav: [
      { text: 'Guide', link: '/guide/introduction' },
      { text: 'API', link: '/api/sync-posts' },
      { text: 'ActivityPub', link: '/activitypub/endpoints' },
      {
        text: 'GitHub',
        link: 'https://github.com/komentoj/komentoj',
      },
    ],

    sidebar: [
      {
        text: 'Guide',
        items: [
          { text: 'Introduction', link: '/guide/introduction' },
          { text: 'Getting Started', link: '/guide/getting-started' },
          { text: 'Configuration', link: '/guide/configuration' },
          { text: 'Blog Integration', link: '/guide/blog-integration' },
          { text: 'Deployment', link: '/guide/deployment' },
        ],
      },
      {
        text: 'API Reference',
        items: [
          { text: 'Sync Posts', link: '/api/sync-posts' },
          { text: 'Get Comments', link: '/api/get-comments' },
        ],
      },
      {
        text: 'ActivityPub',
        items: [
          { text: 'Endpoints', link: '/activitypub/endpoints' },
          { text: 'How It Works', link: '/activitypub/how-it-works' },
        ],
      },
      {
        text: 'Security',
        link: '/security',
      },
    ],

    socialLinks: [
      { icon: 'github', link: 'https://github.com/komentoj/komentoj' },
    ],

    footer: {
      message: 'Released under the MIT License.',
      copyright: 'komentoj — Esperanto for "comments"',
    },

    editLink: {
      pattern: 'https://github.com/komentoj/komentoj/edit/main/docs/:path',
      text: 'Edit this page on GitHub',
    },

    search: {
      provider: 'local',
    },
  },
})

import { defineConfig } from 'vitepress'

// https://vitepress.dev/reference/site-config
export default defineConfig({
  title: 'dman',
  description:
    'A terminal-first download manager for torrents, magnet links, Metalink, HTTP/FTP/SFTP and video sites, with a full-screen TUI, a web UI and a scriptable CLI in one binary.',
  base: '/dman/',
  cleanUrls: true,
  lastUpdated: true,

  markdown: {
    theme: { light: 'github-light', dark: 'github-dark' },
  },

  themeConfig: {
    nav: [
      { text: 'Guide', link: '/guide/getting-started', activeMatch: '/guide/' },
      { text: 'GitHub', link: 'https://github.com/rajput-hemant/dman' }
    ],

    sidebar: [
      {
        text: 'Getting started',
        items: [
          { text: 'Getting started', link: '/guide/getting-started' },
          { text: 'Install', link: '/guide/install' },
          { text: 'Configuration', link: '/guide/configuration' }
        ]
      },
      {
        text: 'Using dman',
        items: [
          { text: 'CLI', link: '/guide/cli' },
          { text: 'TUI', link: '/guide/tui' },
          { text: 'Web UI', link: '/guide/web-ui' }
        ]
      },
      {
        text: 'Integrations',
        items: [
          { text: 'Remote use', link: '/guide/remote' },
          { text: 'Search', link: '/guide/search' },
          { text: 'Browser', link: '/guide/browser' }
        ]
      },
      {
        text: 'Help',
        items: [{ text: 'Troubleshooting', link: '/guide/troubleshooting' }]
      }
    ],

    socialLinks: [
      { icon: 'github', link: 'https://github.com/rajput-hemant/dman' }
    ],

    editLink: {
      pattern: 'https://github.com/rajput-hemant/dman/edit/main/website/:path',
      text: 'Edit this page on GitHub'
    },

    search: {
      provider: 'local',
      options: {
        localization: {
          details: 'Search documentation',
          noResults: 'No results found',
          resetButton: { title: 'Clear query' }
        }
      }
    },

    outline: { level: [2, 3] },

    footer: {
      message:
        'Released under the MIT or Apache-2.0 license.',
      copyright:
        'Copyright © dman contributors'
    }
  }
})

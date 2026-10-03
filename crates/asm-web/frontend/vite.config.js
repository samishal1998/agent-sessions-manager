import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'

// `ASM_API` / `ASM_HUB` point the dev server's proxy at a running
// `asm serve` / `asm hub serve`, so the UI can be developed against real data.
export default defineConfig({
  plugins: [vue()],
  server: {
    proxy: {
      '/api': process.env.ASM_API ?? 'http://127.0.0.1:7433',
      '/hub/': process.env.ASM_HUB ?? 'http://127.0.0.1:7434',
    },
  },
  // Two pages: the session manager (`asm serve`) and the hub's admin page
  // (`asm hub serve`, at /admin).
  build: {
    outDir: process.env.ASM_OUT ?? 'dist',
    rollupOptions: { input: { main: 'index.html', hub: 'hub.html' } },
  },
})

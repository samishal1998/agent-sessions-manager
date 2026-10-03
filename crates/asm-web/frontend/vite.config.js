import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'

export default defineConfig({
  plugins: [vue()],
  server: {
    proxy: { '/api': 'http://127.0.0.1:7433' },
  },
  // Two pages: the session manager (`asm serve`) and the hub's admin page
  // (`asm hub serve`, at /admin).
  build: {
    outDir: 'dist',
    rollupOptions: { input: { main: 'index.html', hub: 'hub.html' } },
  },
})

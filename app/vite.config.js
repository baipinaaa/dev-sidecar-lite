import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'

// Tauri 期望固定端口；改成别的要同步 tauri.conf.json 的 devUrl
export default defineConfig({
  plugins: [vue()],
  clearScreen: false,
  server: {
    port: 5273,
    strictPort: true,
    host: '127.0.0.1'
  },
  build: {
    target: 'chrome105',
    outDir: 'dist',
    emptyOutDir: true,
    sourcemap: false
  }
})

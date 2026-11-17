import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

const __dirname = path.dirname(fileURLToPath(import.meta.url))

// Tauri 期望固定端口；改成别的要同步 tauri.conf.json 的 devUrl
export default defineConfig({
  plugins: [vue()],
  resolve: {
    // 原版是 vue-cli（webpack），它默认能解析省略扩展名的 .vue 与 `@/` 别名。
    // 搬过来的 view/** 里到处是 `import X from '../pages/index'`，这里必须补上，
    // 否则 vite 会报 "failed to resolve import"。
    alias: {
      '@': path.resolve(__dirname, 'src'),
    },
    extensions: ['.mjs', '.js', '.mts', '.ts', '.jsx', '.tsx', '.json', '.vue'],
  },
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

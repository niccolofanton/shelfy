import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

const VIRTUAL_ID = 'virtual:build-time'
const RESOLVED_ID = '\0' + VIRTUAL_ID

function buildTimePlugin() {
  return {
    name: 'build-time',
    resolveId(id) {
      if (id === VIRTUAL_ID) return RESOLVED_ID
    },
    load(id) {
      if (id === RESOLVED_ID) {
        return `export const buildTime = ${Date.now()}`
      }
    },
    handleHotUpdate({ server }) {
      const mod = server.moduleGraph.getModuleById(RESOLVED_ID)
      if (mod) server.moduleGraph.invalidateModule(mod)
    },
  }
}

export default defineConfig({
  plugins: [react(), buildTimePlugin()],
  root: '.',
  base: './',
  build: {
    outDir: 'dist',
    emptyOutDir: true,
  },
  server: {
    port: 5173,
    strictPort: true,
  },
})

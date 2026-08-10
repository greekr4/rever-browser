import { defineConfig } from 'electron-vite'
import react from '@vitejs/plugin-react'
import path from 'node:path'

export default defineConfig({
  main: {
    build: {
      outDir: 'out/main',
      rollupOptions: {
        // node-pty is a native module; it must stay external (its .node binary
        // can't be bundled) and load from node_modules at runtime.
        external: ['bufferutil', 'utf-8-validate', 'node-pty']
      }
    },
    resolve: {
      alias: {
        '@main': path.resolve(__dirname, 'src/main')
      }
    }
  },
  preload: {
    build: {
      outDir: 'out/preload'
    }
  },
  renderer: {
    root: 'src/renderer',
    // Pin the dev-server port. If it slides to a free port (5173 busy → 5174),
    // the renderer origin changes and every localStorage-backed store
    // (bookmarks, theme, overlay position) silently starts empty. strictPort
    // makes that a loud failure instead of silent data "loss".
    server: {
      port: 5173,
      strictPort: true
    },
    build: {
      outDir: 'out/renderer',
      rollupOptions: {
        input: path.resolve(__dirname, 'src/renderer/index.html')
      }
    },
    resolve: {
      alias: {
        '@': path.resolve(__dirname, 'src/renderer/src')
      }
    },
    plugins: [react()]
  }
})

import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'
import { VitePWA } from 'vite-plugin-pwa'
import { viteStaticCopy } from 'vite-plugin-static-copy'

// https://vite.dev/config/
export default defineConfig({
  resolve: {
    // Use ort.min.mjs (external WASM) instead of ort.bundle.min.mjs (inlined WASM)
    // to avoid bundling WASM worker code into main JS bundle
    conditions: ['onnxruntime-web-use-extern-wasm'],
  },
  plugins: [
    react({
      // 修复React 19兼容性问题
      jsxRuntime: 'automatic',
      jsxImportSource: 'react'
    }),
    // Copy onnxruntime-web worker/wasm files as standalone assets.
    // These must NOT be bundled into the main JS — they are loaded
    // by ort at runtime via new Worker() and WebAssembly.instantiate().
    viteStaticCopy({
      targets: [
        {
          src: 'node_modules/onnxruntime-web/dist/ort-wasm-simd-threaded*.wasm',
          dest: '.',
        },
        {
          src: 'node_modules/onnxruntime-web/dist/ort-wasm-simd-threaded*.mjs',
          dest: '.',
        },
        {
          src: 'node_modules/onnxruntime-web/dist/ort-wasm-simd-threaded*.wasm',
          dest: 'assets',
        },
        {
          src: 'node_modules/onnxruntime-web/dist/ort-wasm-simd-threaded*.mjs',
          dest: 'assets',
        },
        {
          src: 'src/assets/squady.png',
          dest: '.',
        },
      ],
    }),
    VitePWA({
      registerType: 'autoUpdate',
      workbox: {
        globPatterns: ['**/*.{js,css,html,ico,png,svg,wasm,onnx}'],
        maximumFileSizeToCacheInBytes: 50 * 1024 * 1024, // 50MB，支持大文件如WASM和ONNX
        navigateFallback: null, // 禁用导航回退
        runtimeCaching: [
          {
            urlPattern: /^https:\/\/fonts\.googleapis\.com\/.*/i,
            handler: 'CacheFirst',
            options: {
              cacheName: 'google-fonts-cache',
              expiration: {
                maxEntries: 10,
                maxAgeSeconds: 60 * 60 * 24 * 365 // 1年
              }
            }
          }
        ]
      },
      manifest: {
        name: 'Squady RealTime',
        short_name: 'RealTime',
        description: 'Squady Real-time Voice Chat Interface',
        theme_color: '#787CF0',
        background_color: '#ffffff',
        display: 'standalone',
        scope: '/',
        start_url: '/',
        icons: [
          {
            src: '/squady.png',
            sizes: '251x250',
            type: 'image/png'
          },
          {
            src: '/squady.png',
            sizes: '192x192',
            type: 'image/png'
          },
          {
            src: '/squady.png',
            sizes: '512x512',
            type: 'image/png',
            purpose: 'any maskable'
          }
        ]
      }
    })
  ],
  optimizeDeps: {
    // 避免优化包含 WASM 文件的依赖以及 AudioWorklet 处理器
    exclude: [
      'onnxruntime-web',
      '@steelbrain/media-speech-detection-web',
      '@steelbrain/media-ingest-audio',
    ],
    // 确保在预构建时保留 import.meta.url
    esbuildOptions: {
      keepNames: true,
    }
  },
  server: {
    // 移除 https 配置，支持 http 访问
    headers: {
      'Cross-Origin-Embedder-Policy': 'require-corp',
      'Cross-Origin-Opener-Policy': 'same-origin',
    },
    fs: {
      // 允许访问上级目录，确保 AudioWorklet 模块可以被正确加载
      allow: ['..']
    },
    // 修复WebSocket连接问题
    hmr: {
      port: 5173,
      host: 'localhost',
      protocol: 'ws', // 改为 ws 支持 http
      // 添加客户端配置
      clientPort: 5173
    }
  },
  build: {
    target: 'esnext',
    rollupOptions: {
      output: {
        // 确保React被正确打包
        manualChunks: {
          'react-vendor': ['react', 'react-dom'],
          'jotai': ['jotai']
        }
      }
    }
  },
  worker: {
    // 保持 ES 模块格式
    format: 'es'
  }
})

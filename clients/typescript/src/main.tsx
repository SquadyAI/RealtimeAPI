
// Configure onnxruntime-web BEFORE any InferenceSession is created.
// - numThreads=1: prevent Worker spawning that crashes in production builds
// - proxy=false: no proxy Worker
// - wasmPaths: load standalone WASM files from root (copied by vite-plugin-static-copy)
import * as ort from 'onnxruntime-web';
ort.env.wasm.numThreads = 1;
ort.env.wasm.proxy = false;
ort.env.wasm.wasmPaths = '/';

import { createRoot } from 'react-dom/client'
// import 'reset-css'
import './index.css'
import { App } from './App.tsx'

// PWA Service Worker 注册
if ('serviceWorker' in navigator) {
  window.addEventListener('load', () => {
    navigator.serviceWorker.register('/sw.js')
      .then((registration) => {
        console.log('SW registered: ', registration);
      })
      .catch((registrationError) => {
        console.log('SW registration failed: ', registrationError);
      });
  });
}

createRoot(document.getElementById('root')!).render(
  <App />
)

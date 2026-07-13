import { resolve } from 'node:path';
import react from '@vitejs/plugin-react';
import { defineConfig } from 'vite';

export default defineConfig({
  base: '/react-app/',
  plugins: [react()],
  build: {
    outDir: '../static/react-app',
    emptyOutDir: true,
    sourcemap: true,
    rollupOptions: {
      input: resolve(__dirname, 'index.html'),
      output: {
        entryFileNames: 'assets/[name]-[hash].js',
        chunkFileNames: 'assets/[name]-[hash].js',
        assetFileNames: 'assets/[name]-[hash][extname]'
      }
    }
  },
  server: {
    host: '127.0.0.1',
    port: 5173,
    strictPort: false,
    proxy: {
      '/api': {
        target: 'http://127.0.0.1:3000',
        changeOrigin: true,
        ws: true
      },
      '/sonos': {
        target: 'http://127.0.0.1:3000',
        changeOrigin: true
      },
      '/styles.css': {
        target: 'http://127.0.0.1:3000',
        changeOrigin: true
      },
      '/react-app/styles.css': {
        target: 'http://127.0.0.1:3000',
        changeOrigin: true,
        rewrite: () => '/styles.css'
      }
    }
  }
});

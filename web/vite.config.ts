import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

// The UI only ever talks to /api/v1 on the same origin.
// In development that is forwarded to the mock server (npm run mock);
// on the device the daemon serves both the built files and the API.
const target = process.env.API_TARGET ?? 'http://127.0.0.1:8787';

export default defineConfig({
  plugins: [react()],
  server: {
    host: true,
    port: 5173,
    proxy: {
      '/api': { target, ws: true, changeOrigin: false },
      '/mock': { target },
    },
  },
  preview: {
    port: 4173,
    proxy: {
      '/api': { target, ws: true },
      '/mock': { target },
    },
  },
  build: {
    outDir: 'dist',
    // Everything is bundled locally: the device has no internet access.
    assetsInlineLimit: 0,
    sourcemap: false,
  },
});

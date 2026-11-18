import { fileURLToPath } from 'node:url';
import react from '@vitejs/plugin-react';
import { defineConfig } from 'vite';

const page = (name: string) => fileURLToPath(new URL(name, import.meta.url));

export default defineConfig({
  plugins: [react()],
  // Tauri prints its own output above Vite's and expects the dev server on this exact port.
  clearScreen: false,
  server: { port: 1420, strictPort: true },
  build: {
    // The app only runs in macOS 14.4's WebKit or later.
    target: 'safari17',
    // The main window's page and the floating indicator's (quick-record FR-7).
    rollupOptions: { input: { main: page('index.html'), indicator: page('indicator.html') } },
  },
});

import react from '@vitejs/plugin-react';
import { defineConfig } from 'vite';

export default defineConfig({
  plugins: [react()],
  // Tauri prints its own output above Vite's and expects the dev server on this exact port.
  clearScreen: false,
  server: { port: 1420, strictPort: true },
  // The app only runs in macOS 14.4's WebKit or later.
  build: { target: 'safari17' },
});

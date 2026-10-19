import { crx } from '@crxjs/vite-plugin';
import react from '@vitejs/plugin-react';
import { defineConfig } from 'vite';
import manifest from './manifest.config.ts';

export default defineConfig({
  plugins: [react(), crx({ manifest })],
  build: {
    rollupOptions: {
      // Pages the manifest doesn't reference, which CRXJS would otherwise leave out.
      input: { offscreen: 'src/offscreen/offscreen.html', permission: 'src/permission/permission.html' },
    },
  },
});

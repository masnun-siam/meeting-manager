import { defineManifest } from '@crxjs/vite-plugin';
import pkg from './package.json' with { type: 'json' };

export default defineManifest({
  manifest_version: 3,
  name: 'Meeting Manager',
  version: pkg.version,
  // tabCapture.getMediaStreamId consumed by an offscreen document needs Chrome 116.
  minimum_chrome_version: '116',
  permissions: ['tabCapture', 'offscreen', 'downloads', 'nativeMessaging', 'storage'],
  action: { default_popup: 'src/popup/index.html' },
  background: { service_worker: 'src/background/serviceWorker.ts', type: 'module' },
  // Public key only, fixing the ID to obbaikcmcdooakogedoiiflflbbhpgdh, which the native host trusts.
  // Keep it byte-for-byte: any change gives a new ID.
  key: 'MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEA1NzeBg2z8mj0UtNmJYChfjVTJJcAYVtf4N9BImppQMrEE+CN1DS131XpoRjsV5k4eHN5Ksbp38tNnMelPmmWqMjWQZAoFdC3KItjTn0qTHA804cAI+dSCq8mnhdYsqj9a1S0n/yJdjzpB8GbHegH903aRzgDB5gjuyqGYMDu521idRnNppCGkSc256IHUf2iomtPvTsCm+5uBMLLblwCxR52Ysb3dFhinyv4hyimX8wHKjqI7NrBeCqnhRF3B0OoZi5Kr4fQ7e1CsfSg1pAz5mYYvpI0TAEaZW6ws7IRZ5W1WQCSpFwYH4BjB5Yymtklj9DuDz4XvV/ocnfNR5GQUwIDAQAB', // gitleaks:allow (public key, not a secret)
});

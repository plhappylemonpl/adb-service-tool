import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig } from 'vite';

// @tauri-apps/cli ustawia TAURI_DEV_HOST przy dev na urządzeniu
// @ts-expect-error process to globalny obiekt Node (bez @types/node)
const host = process.env.TAURI_DEV_HOST;

export default defineConfig({
  plugins: [sveltekit()],
  // Tauri oczekuje stałego portu
  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true,
    host: host || false,
    hmr: host ? { protocol: 'ws', host, port: 5174 } : undefined,
    watch: {
      // nie obserwuj backendu Rust
      ignored: ['**/src-tauri/**']
    }
  }
});

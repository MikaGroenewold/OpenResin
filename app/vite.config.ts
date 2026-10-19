import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// https://v2.tauri.app/start/frontend/vite/
const host = process.env.TAURI_DEV_HOST;

export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    watch: { ignored: ["**/src-tauri/**"] },
  },
  build: {
    target: ["es2021", "chrome105", "safari15"],
    chunkSizeWarningLimit: 2000,
  },
});

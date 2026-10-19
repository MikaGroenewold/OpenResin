import { defineConfig, mergeConfig } from "vitest/config";
import viteConfig from "./vite.config";

// Frontend tests: jsdom + React Testing Library, Tauri IPC mocked in src/__tests__/setup.ts.
export default mergeConfig(
  viteConfig,
  defineConfig({
    test: {
      environment: "jsdom",
      include: ["src/**/*.test.{ts,tsx}"],
      setupFiles: ["./src/__tests__/setup.ts"],
      restoreMocks: true,
      testTimeout: 10_000,
    },
  }),
);

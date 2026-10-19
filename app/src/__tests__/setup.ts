// Global test setup (vitest setupFiles). Keep it free of imports that pull in
// i18n.ts: the UI language is fixed when that module loads, and some test files
// select German before importing anything.
import "@testing-library/jest-dom/vitest";
import { cleanup, configure } from "@testing-library/react";
import { clearMocks } from "@tauri-apps/api/mocks";
import { afterEach, beforeEach, vi } from "vitest";
import { useStore } from "../store";
import { installBackend } from "./harness";
import { FakeSceneView } from "./fakeScene";

// three.js needs WebGL, which jsdom does not have: use the recording fake.
vi.mock("../viewport/SceneView", async () => ({ SceneView: (await import("./fakeScene")).FakeSceneView }));

// headroom for slow CI machines (only matters when something is wrong)
configure({ asyncUtilTimeout: 3000 });

// Browser APIs jsdom lacks.
let blobId = 0;
URL.createObjectURL = () => `blob:openresin/${++blobId}`;
URL.revokeObjectURL = () => {};
class FakeIntersectionObserver {
  constructor(private cb: (entries: { isIntersecting: boolean }[]) => void) {}
  observe() {
    queueMicrotask(() => this.cb([{ isIntersecting: true }]));
  }
  unobserve() {}
  disconnect() {}
}
(globalThis as unknown as { IntersectionObserver: unknown }).IntersectionObserver = FakeIntersectionObserver;
HTMLElement.prototype.setPointerCapture = () => {};

const initialState = useStore.getState();

beforeEach(() => {
  useStore.setState(initialState, true);
  installBackend();
});

afterEach(async () => {
  cleanup();
  // unmounting unregisters event listeners asynchronously; let that finish before the IPC mock goes away
  await new Promise((r) => setTimeout(r, 0));
  clearMocks();
  vi.unstubAllGlobals();
  FakeSceneView.instances = [];
  try {
    localStorage.clear();
  } catch {
    /* ignore */
  }
});

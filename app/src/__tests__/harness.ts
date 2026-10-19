// Shared test helpers: the fake backend wired into Tauri's IPC mock, and small
// render/interaction helpers for the component tests.
import { act, fireEvent, render, screen, within, type RenderResult } from "@testing-library/react";
import { mockIPC, mockWindows } from "@tauri-apps/api/mocks";
import { emit } from "@tauri-apps/api/event";
import type { ReactElement } from "react";
import { api, type ObjectInfo, type Vec3 } from "../api";
import { useStore } from "../store";
import { FakeBackend } from "./fakeBackend";

/** The backend of the running test (replaced before every test by setup.ts). */
export let backend = new FakeBackend();

export function installBackend(): FakeBackend {
  backend = new FakeBackend();
  mockWindows("main");
  mockIPC(backend.handle, { shouldMockEvents: true });
  return backend;
}

/** Emits a backend event (`app.emit(...)` in Rust) to the frontend listeners. */
export async function emitEvent(event: string, payload: unknown) {
  await act(async () => {
    await emit(event, payload);
  });
}

/** Lets pending promise chains and React updates settle. */
export async function settle(rounds = 5) {
  for (let i = 0; i < rounds; i++) {
    await act(async () => {
      await new Promise((r) => setTimeout(r, 0));
    });
  }
}

/** Loads config and printer list from the fake backend into the store. */
export async function loadConfig() {
  useStore.getState().setConfig(await api.getConfig());
  useStore.getState().setPrinters(await api.listPrinters());
}

/** Puts models on the fake plate and mirrors them into the store. */
export function seed(...models: (string | { name: string; size?: Vec3 })[]): ObjectInfo[] {
  for (const m of models) {
    const o = typeof m === "string" ? { name: m } : m;
    backend.addObject(o.name, o.size);
  }
  const infos = backend.infos();
  useStore.getState().replaceObjects(infos);
  return infos;
}

export async function renderWithConfig(ui: ReactElement): Promise<RenderResult> {
  await loadConfig();
  const r = render(ui);
  await settle();
  return r;
}

/** The side-panel section with the given heading. */
export function panel(heading: string | RegExp): HTMLElement {
  const h = screen.getByRole("heading", { name: heading });
  return h.closest("section") as HTMLElement;
}

export const inPanel = (heading: string | RegExp) => within(panel(heading));

/** Types a value into a NumberField and commits it like a user (blur). */
export function setNumber(input: HTMLElement, value: string | number) {
  fireEvent.change(input, { target: { value: String(value) } });
  fireEvent.blur(input);
}

/** Toasts as "kind: text". */
export const toasts = () => useStore.getState().toasts.map((t) => `${t.kind}: ${t.text}`);

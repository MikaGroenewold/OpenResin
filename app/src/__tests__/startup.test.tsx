// Files and flags passed on the command line are handled once per process, so
// every test here loads a fresh copy of the app modules.
import { render, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { backend, settle } from "./harness";

async function freshApp() {
  vi.resetModules();
  const [{ App }, { useStore }] = await Promise.all([import("../App"), import("../store")]);
  return { App, useStore };
}

beforeEach(() => localStorage.setItem("openresin.onboarded", "1"));

describe("startup", () => {
  it("imports model files passed on the command line", async () => {
    backend.startupFiles = ["C:\\Users\\me\\Downloads\\benchy.stl"];
    const { App, useStore } = await freshApp();
    render(<App />);
    await waitFor(() => expect(useStore.getState().objects.map((o) => o.name)).toEqual(["benchy"]));
    expect(backend.lastCall("import_models")).toEqual({ paths: ["C:\\Users\\me\\Downloads\\benchy.stl"] });
  });

  it("opens a sliced file passed on the command line in the preview", async () => {
    backend.startupFiles = ["C:\\jobs\\ring.goo"];
    const { App, useStore } = await freshApp();
    render(<App />);
    await waitFor(() => expect(useStore.getState().mode).toBe("preview"));
    expect(useStore.getState().slice?.suggested_name).toBe("ring.goo");
  });

  it("--printers starts on the printers page without the welcome dialog", async () => {
    localStorage.removeItem("openresin.onboarded");
    backend.startupFlags = ["--printers"];
    const { App, useStore } = await freshApp();
    const { queryByRole } = render(<App />);
    await waitFor(() => expect(useStore.getState().mode).toBe("devices"));
    await settle();
    expect(queryByRole("dialog", { name: "Welcome to OpenResin" })).toBeNull();
  });

  it("does not import startup files again when the backend still holds a plate (UI reload)", async () => {
    backend.startupFiles = ["C:\\benchy.stl"];
    backend.addObject("kept");
    const { App, useStore } = await freshApp();
    render(<App />);
    await waitFor(() => expect(useStore.getState().objects.map((o) => o.name)).toEqual(["kept"]));
    await settle();
    expect(backend.callsOf("startup_files")).toEqual([]);
    expect(backend.callsOf("import_models")).toEqual([]);
  });
});

import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { App } from "../App";
import { ErrorBoundary } from "../ErrorBoundary";
import { useStore } from "../store";
import { Toasts } from "../ui";
import { backend, emitEvent, seed, settle, toasts } from "./harness";
import type { DeviceStatusEvent } from "../api";

const st = () => useStore.getState();

async function renderApp({ onboarded = true } = {}) {
  if (onboarded) localStorage.setItem("openresin.onboarded", "1");
  const r = render(<App />);
  await settle();
  return r;
}

function stubReload() {
  const reload = vi.fn();
  vi.stubGlobal("location", { ...window.location, reload });
  return reload;
}

describe("App start", () => {
  it("loads config, printers and the plate kept by the backend", async () => {
    backend.addObject("kept", [10, 10, 10]);
    await renderApp();
    for (const cmd of ["get_config", "list_printers", "scene_objects", "history_state", "saved_devices"]) expect(backend.callsOf(cmd).length).toBeGreaterThan(0);
    expect(screen.getByText("kept")).toBeInTheDocument();
    expect(screen.getByTitle("Choose printer")).toHaveTextContent("Mars 5 Ultra");
    expect(document.documentElement.dataset.theme).toBe("dark");
  });

  it("shows a toast when the backend cannot be reached", async () => {
    backend.fail("get_config", "backend not ready");
    await renderApp();
    expect(toasts()).toContain("error: backend not ready");
  });
});

describe("keyboard shortcuts", () => {
  beforeEach(async () => {
    seed("a", "b");
  });

  it("Ctrl+A selects all, Escape clears selection, tool and panel", async () => {
    const user = userEvent.setup();
    await renderApp();
    await user.keyboard("{Control>}a{/Control}");
    expect(st().selection).toHaveLength(2);
    await user.click(screen.getByRole("button", { name: "Auto Orient" }));
    await user.keyboard("e");
    expect(st()).toMatchObject({ tool: "rotate", panel: "orient" });
    await user.keyboard("{Escape}");
    expect(st()).toMatchObject({ selection: [], tool: "select", panel: null });
  });

  it("Q/W/E/R/M choose the tools", async () => {
    const user = userEvent.setup();
    await renderApp();
    const tools: [string, string][] = [
      ["w", "move"],
      ["e", "rotate"],
      ["r", "scale"],
      ["m", "measure"],
      ["q", "select"],
    ];
    for (const [key, tool] of tools) {
      await user.keyboard(key);
      expect(st().tool).toBe(tool);
    }
  });

  it("arrow keys nudge the selection by 1, 10 (Shift) or 0.1 mm (Alt)", async () => {
    const user = userEvent.setup();
    await renderApp();
    const id = st().objects[0].id;
    st().select([id]);
    await user.keyboard("{ArrowRight}");
    await waitFor(() => expect(backend.callsOf("set_transform")).toHaveLength(1));
    await user.keyboard("{Shift>}{ArrowUp}{/Shift}");
    await waitFor(() => expect(backend.callsOf("set_transform")).toHaveLength(2));
    await user.keyboard("{Alt>}{ArrowLeft}{/Alt}");
    await waitFor(() => expect(backend.callsOf("set_transform")).toHaveLength(3));
    await user.keyboard("{ArrowDown}");
    await waitFor(() => expect(backend.callsOf("set_transform")).toHaveLength(4));
    const t = st().objects.find((o) => o.id === id)!.transform.translation;
    expect(t[0]).toBeCloseTo(0.9, 6);
    expect(t[1]).toBeCloseTo(9, 6);
  });

  it("Delete removes the selection, Ctrl+Z / Ctrl+Y undo and redo", async () => {
    const user = userEvent.setup();
    await renderApp();
    const [a, b] = st().objects.map((o) => o.id);
    st().select([a]);
    await user.keyboard("{Delete}");
    await waitFor(() => expect(st().objects.map((o) => o.id)).toEqual([b]));
    await user.keyboard("{Control>}z{/Control}");
    await waitFor(() => expect(st().objects.map((o) => o.id)).toEqual([a, b]));
    await user.keyboard("{Control>}y{/Control}");
    await waitFor(() => expect(st().objects.map((o) => o.id)).toEqual([b]));
    await user.keyboard("{Control>}{Shift>}z{/Shift}{/Control}");
    await waitFor(() => expect(backend.callsOf("redo")).toHaveLength(2));
  });

  it("Ctrl+Enter slices, Ctrl+O opens models, Ctrl+S saves the project", async () => {
    const user = userEvent.setup();
    await renderApp();
    await user.keyboard("{Control>}{Enter}{/Control}");
    await waitFor(() => expect(st().mode).toBe("preview"));
    await user.keyboard("{Control>}o{/Control}");
    await waitFor(() => expect(backend.callsOf("plugin:dialog|open")).toHaveLength(1));
    await user.keyboard("{Control>}s{/Control}");
    await waitFor(() => expect(backend.callsOf("plugin:dialog|save")).toHaveLength(1));
  });

  it("F1 and ? open the keyboard help", async () => {
    const user = userEvent.setup();
    await renderApp();
    await user.keyboard("{F1}");
    const help = screen.getByRole("dialog", { name: "Keyboard & mouse" });
    expect(within(help).getByText("Ctrl+Z / Ctrl+Y")).toBeInTheDocument();
    expect(within(help).getByText("Undo / redo")).toBeInTheDocument();
    await user.click(within(help).getByRole("button", { name: "Close" }));
    expect(screen.queryByRole("dialog")).toBeNull();
    await user.keyboard("?");
    expect(screen.getByRole("dialog", { name: "Keyboard & mouse" })).toBeInTheDocument();
  });

  it("ignores shortcuts while typing in a field", async () => {
    const user = userEvent.setup();
    await renderApp();
    useStore.setState({ panel: "label", tool: "label" });
    const text = await screen.findByLabelText("Text");
    await user.clear(text);
    await user.type(text, "wer{Delete}");
    expect(st().tool).toBe("label");
    expect(st().objects).toHaveLength(2);
    expect(st().label.text).toBe("wer");
  });

  it("tool keys do nothing outside the plate view", async () => {
    const user = userEvent.setup();
    await renderApp();
    st().setMode("devices");
    await user.keyboard("w");
    expect(st().tool).toBe("select");
  });
});

describe("backend events", () => {
  const status = (state: DeviceStatusEvent["status"]["state"], connected = true): DeviceStatusEvent => ({
    ip: "192.168.1.50",
    status: { state },
    connected,
  });

  it("device-status updates the device and announces finished prints, errors and lost connections", async () => {
    await renderApp();
    st().updateDevice("192.168.1.50", { name: "Workshop", connected: true, status: status("printing") });
    await emitEvent("device-status", status("completed"));
    expect(toasts()).toContain("success: Workshop: print finished 🎉");
    expect(st().devices["192.168.1.50"].status?.status.state).toBe("completed");
    await emitEvent("device-status", status("printing"));
    await emitEvent("device-status", status("stopped"));
    expect(toasts()).toContain("info: Workshop: print stopped");
    await emitEvent("device-status", status("error"));
    expect(toasts()).toContain("error: Workshop: printer reports an error");
    await emitEvent("device-status", status("offline", false));
    expect(toasts()).toContain("error: Workshop: connection lost");
    expect(st().devices["192.168.1.50"].connected).toBe(false);
  });

  it("does not announce anything for the first status of a printer", async () => {
    await renderApp();
    await emitEvent("device-status", status("completed"));
    expect(toasts()).toEqual([]);
    expect(st().devices["192.168.1.50"]).toMatchObject({ connected: true, name: "192.168.1.50" });
  });

  it("upload, download and slice progress land in the store", async () => {
    await renderApp();
    await emitEvent("upload-progress", { ip: "192.168.1.50", sent: 10, total: 100, done: false });
    expect(st().devices["192.168.1.50"].upload).toEqual({ ip: "192.168.1.50", sent: 10, total: 100, done: false });
    await emitEvent("download-progress", { ip: "192.168.1.50", path: "/local/a.goo", done: 5, total: 10 });
    expect(st().downloads).toEqual({ "192.168.1.50|/local/a.goo": { ip: "192.168.1.50", path: "/local/a.goo", done: 5, total: 10 } });
    await emitEvent("download-progress", { ip: "192.168.1.50", path: "/local/a.goo", done: 10, total: 10 });
    expect(st().downloads).toEqual({});
    await emitEvent("slice-progress", { done: 40, total: 400 });
    expect(st().sliceProgress).toEqual({ done: 40, total: 400 });
  });

  it("files dropped on the window are added to the plate", async () => {
    await renderApp();
    await emitEvent("tauri://drag-enter", { paths: ["C:\\a.stl"], position: { x: 1, y: 1 } });
    expect(screen.getByText("Drop to add to the plate")).toBeInTheDocument();
    await emitEvent("tauri://drag-leave", null);
    expect(screen.queryByText("Drop to add to the plate")).toBeNull();
    await emitEvent("tauri://drag-drop", { paths: ["C:\\models\\dropped.stl"], position: { x: 1, y: 1 } });
    await waitFor(() => expect(st().objects.map((o) => o.name)).toEqual(["dropped"]));
  });

  it("files dropped while a printer's file manager is open go to that printer", async () => {
    await renderApp();
    st().setMode("devices");
    st().setDropTarget("192.168.1.50");
    const onDrop = vi.fn();
    window.addEventListener("printer-drop", onDrop);
    await emitEvent("tauri://drag-over", { position: { x: 1, y: 1 } });
    expect(screen.getByText("Drop to upload to the printer")).toBeInTheDocument();
    await emitEvent("tauri://drag-drop", { paths: ["C:\\jobs\\a.goo"], position: { x: 1, y: 1 } });
    window.removeEventListener("printer-drop", onDrop);
    expect(onDrop).toHaveBeenCalledTimes(1);
    expect((onDrop.mock.calls[0][0] as CustomEvent).detail).toEqual(["C:\\jobs\\a.goo"]);
    expect(backend.callsOf("import_models")).toEqual([]);
  });
});

describe("top bar", () => {
  it("undo/redo buttons follow the history", async () => {
    const user = userEvent.setup();
    await renderApp();
    expect(screen.getByRole("button", { name: "Undo (Ctrl+Z)" })).toBeDisabled();
    await user.click(screen.getByRole("button", { name: "Cylinder" }));
    await waitFor(() => expect(screen.getByRole("button", { name: "Undo (Ctrl+Z)" })).toBeEnabled());
    await user.click(screen.getByRole("button", { name: "Undo (Ctrl+Z)" }));
    await waitFor(() => expect(screen.getByRole("button", { name: "Redo (Ctrl+Y)" })).toBeEnabled());
  });

  it("switches theme and persists it", async () => {
    const user = userEvent.setup();
    await renderApp();
    await user.click(screen.getByRole("button", { name: "Toggle theme" }));
    expect(backend.lastCall("set_theme")).toEqual({ theme: "light" });
    expect(document.documentElement.dataset.theme).toBe("light");
  });

  it("switches the language and reloads the interface", async () => {
    const user = userEvent.setup();
    const reload = stubReload();
    await renderApp();
    const lang = screen.getByTitle("Language");
    expect(lang).toHaveTextContent("EN");
    await user.click(lang);
    expect(localStorage.getItem("openresin.lang")).toBe("de");
    expect(reload).toHaveBeenCalledTimes(1);
  });

  it("opens the printer picker, the settings and the printers page", async () => {
    const user = userEvent.setup();
    await renderApp();
    await user.click(screen.getByTitle("Choose printer"));
    expect(screen.getByRole("dialog", { name: "Choose printer" })).toBeInTheDocument();
    await user.keyboard("{Escape}");
    expect(screen.queryByRole("dialog")).toBeNull();
    await user.click(screen.getByRole("button", { name: "Edit settings" }));
    expect(screen.getByRole("dialog", { name: "Print settings" })).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Cancel" }));
    const nav = within(screen.getByRole("navigation", { name: "Workflow" }));
    expect(nav.getByRole("button", { name: "Preview" })).toBeDisabled();
    await user.click(nav.getByRole("button", { name: "Printers" }));
    expect(st().mode).toBe("devices");
    expect(screen.getByRole("heading", { name: "Connect your printer" })).toBeInTheDocument();
  });

  it("shows the number of connected printers and the busy label", async () => {
    await renderApp();
    st().updateDevice("192.168.1.50", { connected: true });
    st().setBusy("Arranging…");
    expect(await screen.findByLabelText("1 connected")).toBeInTheDocument();
    expect(screen.getByText("Arranging…")).toBeInTheDocument();
  });

  it("New plate in the menu clears the plate after confirmation", async () => {
    const user = userEvent.setup();
    seed("a");
    await renderApp();
    await user.click(screen.getByRole("button", { name: "New plate" }));
    await waitFor(() => expect(st().objects).toEqual([]));
    expect(backend.asked).toEqual(["Remove all models from the plate?"]);
  });
});

describe("onboarding", () => {
  afterEach(() => localStorage.clear());

  it("walks through language, printer and network on first start", async () => {
    const user = userEvent.setup();
    await renderApp({ onboarded: false });
    const dialog = within(screen.getByRole("dialog", { name: "Welcome to OpenResin" }));
    expect(dialog.getByRole("button", { name: "English" })).toHaveClass("primary");
    expect(dialog.queryByRole("button", { name: "Previous" })).toBeNull();
    await user.click(dialog.getByRole("button", { name: "Next" }));
    await user.type(dialog.getByPlaceholderText("Search printers…"), "photon");
    await user.click(dialog.getByRole("button", { name: /Photon Mono X/ }));
    await waitFor(() => expect(backend.lastCall("set_printer")).toEqual({ id: "anycubic-photon-mono-x" }));
    expect(dialog.getByText("Photon Mono X", { selector: "strong" })).toBeInTheDocument();
    await user.click(dialog.getByRole("button", { name: "Previous" }));
    expect(dialog.getByRole("button", { name: "Deutsch" })).toBeInTheDocument();
    await user.click(dialog.getByRole("button", { name: "Next" }));
    await user.click(dialog.getByRole("button", { name: "Next" }));
    backend.addPrinter({ ip: "192.168.1.50" });
    await user.click(dialog.getByRole("button", { name: "Search my network now" }));
    await waitFor(() => expect(backend.callsOf("discover_printers")).toHaveLength(1));
    expect(st().mode).toBe("devices");
    await user.click(dialog.getByRole("button", { name: "Open a model" }));
    expect(screen.queryByRole("dialog", { name: "Welcome to OpenResin" })).toBeNull();
    expect(localStorage.getItem("openresin.onboarded")).toBe("1");
    expect(st().mode).toBe("model");
    await waitFor(() => expect(backend.callsOf("plugin:dialog|open")).toHaveLength(1));
  });

  it("can be skipped and does not come back", async () => {
    const user = userEvent.setup();
    const { unmount } = await renderApp({ onboarded: false });
    await user.click(screen.getByRole("button", { name: "Skip" }));
    expect(screen.queryByRole("dialog")).toBeNull();
    unmount();
    render(<App />);
    await settle();
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("choosing Deutsch stores the language and reloads", async () => {
    const user = userEvent.setup();
    const reload = stubReload();
    await renderApp({ onboarded: false });
    await user.click(screen.getByRole("button", { name: "Deutsch" }));
    expect(localStorage.getItem("openresin.lang")).toBe("de");
    expect(reload).toHaveBeenCalled();
  });
});

describe("ErrorBoundary", () => {
  function Boom(): never {
    throw new Error("kaputt");
  }

  it("renders children when nothing fails", () => {
    render(
      <ErrorBoundary>
        <p>fine</p>
      </ErrorBoundary>,
    );
    expect(screen.getByText("fine")).toBeInTheDocument();
  });

  it("shows a recoverable error screen instead of a blank window", async () => {
    const user = userEvent.setup();
    const reload = stubReload();
    const err = vi.spyOn(console, "error").mockImplementation(() => {});
    // React rethrows render errors as uncaught window errors in development; keep the log clean
    const swallow = (e: ErrorEvent) => e.preventDefault();
    window.addEventListener("error", swallow);
    render(
      <ErrorBoundary>
        <Boom />
      </ErrorBoundary>,
    );
    expect(screen.getByRole("heading", { name: "Something went wrong in the interface." })).toBeInTheDocument();
    expect(screen.getByText("kaputt")).toBeInTheDocument();
    expect(err).toHaveBeenCalled();
    window.removeEventListener("error", swallow);
    await user.click(screen.getByRole("button", { name: "Reload interface" }));
    expect(reload).toHaveBeenCalled();
  });
});

describe("Toasts", () => {
  it("shows toasts and dismisses them on click", async () => {
    const user = userEvent.setup();
    render(<Toasts />);
    useStore.getState().toast("error", "Upload failed");
    const t = await screen.findByText("Upload failed");
    expect(t).toHaveClass("toast", "error");
    await user.click(t);
    expect(screen.queryByText("Upload failed")).toBeNull();
  });
});

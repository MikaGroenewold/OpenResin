import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { savedToEntry, useStore } from "../store";
import { defaultSettings } from "./fakeBackend";
import type { ObjectInfo, SliceSummary } from "../api";

const st = () => useStore.getState();

const obj = (id: number, name = `M${id}`): ObjectInfo => ({
  id,
  name,
  triangles: 12,
  transform: { translation: [0, 0, 0], rotation: [0, 0, 0, 1], scale: [1, 1, 1] },
  bounds: { min: [-5, -5, 0], max: [5, 5, 10] },
  support_points: 0,
  volume_mm3: 1000,
  mesh_key: id,
  subtract: false,
  cutouts: 0,
});
const slice = { layer_count: 10 } as SliceSummary;

describe("initial state", () => {
  it("starts in the Prepare ribbon with the select tool and no panel", () => {
    const s = st();
    expect(s.mode).toBe("model");
    expect(s.ribbon).toBe("prepare");
    expect(s.tool).toBe("select");
    expect(s.panel).toBeNull();
    expect(s.objects).toEqual([]);
    expect(s.selection).toEqual([]);
    expect(s.label).toEqual({ text: "OpenResin", height: 5, depth: 0.8, mode: "emboss" });
    expect(s.drill).toEqual({ diameter: 3, depth: 4 });
    expect(s.history).toEqual({ can_undo: false, can_redo: false });
    expect(s.sendTo).toBeNull();
  });
});

describe("objects", () => {
  it("upsertObjects adds new and replaces existing objects by id, keeping order, and drops the slice", () => {
    st().setSlice(slice);
    st().upsertObjects([obj(1), obj(2)]);
    st().upsertObjects([obj(1, "renamed"), obj(3)]);
    expect(st().objects.map((o) => [o.id, o.name])).toEqual([
      [1, "renamed"],
      [2, "M2"],
      [3, "M3"],
    ]);
    expect(st().slice).toBeNull();
  });

  it("replaceObjects replaces the list and keeps only selected ids that still exist", () => {
    st().upsertObjects([obj(1), obj(2), obj(3)]);
    st().select([1, 3]);
    st().setSlice(slice);
    st().replaceObjects([obj(3), obj(4)]);
    expect(st().objects.map((o) => o.id)).toEqual([3, 4]);
    expect(st().selection).toEqual([3]);
    expect(st().slice).toBeNull();
  });

  it("removeObjects removes objects and their selection", () => {
    st().upsertObjects([obj(1), obj(2), obj(3)]);
    st().select([1, 2]);
    st().setSlice(slice);
    st().removeObjects([2, 3]);
    expect(st().objects.map((o) => o.id)).toEqual([1]);
    expect(st().selection).toEqual([1]);
    expect(st().slice).toBeNull();
  });

  it("select replaces the selection", () => {
    st().select([5, 6]);
    expect(st().selection).toEqual([5, 6]);
    st().select([]);
    expect(st().selection).toEqual([]);
  });

  it("support points are stored per object and bumping invalidates the slice", () => {
    const p = { position: [1, 2, 3] as [number, number, number], normal: [0, 0, -1] as [number, number, number], size: "light" as const };
    st().setSupportPoints(1, [p]);
    st().setSupportPoints(2, []);
    expect(st().supportPoints).toEqual({ 1: [p], 2: [] });
    st().setSlice(slice);
    st().bumpSupports(1);
    st().bumpSupports(1);
    st().bumpSupports(2);
    expect(st().supportVersion).toEqual({ 1: 2, 2: 1 });
    expect(st().slice).toBeNull();
  });
});

describe("mode, ribbon, tool and panel transitions", () => {
  it("setRibbon switches back to the model view and resets tool, panel and measurement", () => {
    st().setMode("preview");
    st().setTool("rotate");
    st().setPanel("orient");
    st().setMeasure({ a: [0, 0, 0], b: [1, 1, 1] });
    st().setRibbon("support");
    expect(st()).toMatchObject({ ribbon: "support", mode: "model", tool: "select", panel: null, measure: null });
  });

  it("setTool clears a running measurement", () => {
    st().setMeasure({ a: [0, 0, 0], b: [1, 0, 0] });
    st().setTool("move");
    expect(st().tool).toBe("move");
    expect(st().measure).toBeNull();
  });

  it("setPanel toggles the same panel off and switches between panels", () => {
    st().setPanel("copy");
    expect(st().panel).toBe("copy");
    st().setPanel("mirror");
    expect(st().panel).toBe("mirror");
    st().setPanel("mirror");
    expect(st().panel).toBeNull();
    st().setPanel(null);
    expect(st().panel).toBeNull();
  });

  it("setMode changes only the mode", () => {
    st().setTool("scale");
    st().setMode("devices");
    expect(st().mode).toBe("devices");
    expect(st().tool).toBe("scale");
  });
});

describe("simple setters", () => {
  it.each([
    ["setOverhang", "overhang", true],
    ["setDrill", "drill", { diameter: 2, depth: 6 }],
    ["setLabel", "label", { text: "ABC", height: 8, depth: 1, mode: "engrave" }],
    ["setHistory", "history", { can_undo: true, can_redo: false }],
    ["setMeasure", "measure", { a: [0, 0, 0], b: [3, 4, 0] }],
    ["setBusy", "busy", "Loading…"],
    ["setSliceProgress", "sliceProgress", { done: 3, total: 10 }],
    ["setLayer", "layer", 42],
    ["selectDevice", "selectedDevice", "10.0.0.2"],
    ["setDeviceTab", "deviceTab", "files"],
    ["setDropTarget", "dropTarget", "10.0.0.2"],
    ["setSendTo", "sendTo", ""],
    ["setPrinters", "printers", [{ id: "x" }]],
    ["setConfig", "config", { theme: "light", settings: defaultSettings() }],
  ] as const)("%s sets %s", (setter, key, value) => {
    (st()[setter] as (v: unknown) => void)(value);
    expect(st()[key]).toEqual(value);
  });

  it("setSlice resets the layer to the first one", () => {
    st().setLayer(7);
    st().setSlice(slice);
    expect(st().slice).toBe(slice);
    expect(st().layer).toBe(0);
    st().setLayer(3);
    st().setSlice(null);
    expect(st().layer).toBe(0);
  });
});

describe("devices and downloads", () => {
  it("updateDevice creates an entry with defaults and merges patches", () => {
    st().updateDevice("10.0.0.2", { connecting: true });
    expect(st().devices["10.0.0.2"]).toEqual({ ip: "10.0.0.2", name: "10.0.0.2", saved: false, connected: false, connecting: true });
    st().updateDevice("10.0.0.2", { connecting: false, connected: true, name: "Mars" });
    expect(st().devices["10.0.0.2"]).toMatchObject({ name: "Mars", connected: true, connecting: false, saved: false });
  });

  it("removeDevice deletes only that device", () => {
    st().updateDevice("a", {});
    st().updateDevice("b", {});
    st().removeDevice("a");
    expect(Object.keys(st().devices)).toEqual(["b"]);
  });

  it("setDownload stores progress and removes it with null", () => {
    st().setDownload("ip|/local/a.goo", { done: 1, total: 4 });
    expect(st().downloads).toEqual({ "ip|/local/a.goo": { done: 1, total: 4 } });
    st().setDownload("ip|/local/a.goo", null);
    expect(st().downloads).toEqual({});
  });

  it("savedToEntry marks the device as saved", () => {
    expect(savedToEntry({ ip: "1.2.3.4", name: "Shop", model: "M5U" })).toEqual({ name: "Shop", model: "M5U", saved: true });
  });
});

describe("toasts", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("adds toasts with unique ids and dismisses them", () => {
    st().toast("info", "one");
    st().toast("success", "two");
    const [a, b] = st().toasts;
    expect(a.id).not.toBe(b.id);
    expect(st().toasts.map((t) => [t.kind, t.text])).toEqual([
      ["info", "one"],
      ["success", "two"],
    ]);
    st().dismissToast(a.id);
    expect(st().toasts.map((t) => t.text)).toEqual(["two"]);
  });

  it("hides info toasts after 4.5 s and errors after 9 s", () => {
    st().toast("info", "info");
    st().toast("error", "error");
    vi.advanceTimersByTime(4499);
    expect(st().toasts).toHaveLength(2);
    vi.advanceTimersByTime(1);
    expect(st().toasts.map((t) => t.text)).toEqual(["error"]);
    vi.advanceTimersByTime(4500);
    expect(st().toasts).toEqual([]);
  });
});

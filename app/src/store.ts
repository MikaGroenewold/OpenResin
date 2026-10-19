// Global UI state.
import { create } from "zustand";
import type {
  ConfigView,
  DeviceStatusEvent,
  DiscoveredPrinter,
  ObjectInfo,
  PrinterProfile,
  SavedDevice,
  SliceSummary,
  SupportPoint,
  UploadProgress,
} from "./api";

export type Mode = "model" | "preview" | "devices";
export type Ribbon = "repair" | "prepare" | "support" | "export";
export type Tool =
  | "select"
  | "move"
  | "rotate"
  | "scale"
  | "measure"
  | "layflat"
  | "support_add"
  | "support_remove"
  | "drill"
  | "label";
/** Side panel shown for the active ribbon tool. */
export type Panel =
  | null
  | "copy"
  | "mirror"
  | "align"
  | "layout"
  | "support"
  | "raft"
  | "repair"
  | "hollow"
  | "cut"
  | "label"
  | "platform"
  | "boolean"
  | "orient";

export interface Toast {
  id: number;
  kind: "info" | "success" | "error";
  text: string;
}

export interface DeviceEntry {
  ip: string;
  name: string;
  model?: string | null;
  saved: boolean;
  discovered?: DiscoveredPrinter;
  connected: boolean;
  connecting: boolean;
  status?: DeviceStatusEvent;
  upload?: UploadProgress;
  camera?: { stream_url: string; snapshot_url: string } | null;
  error?: string | null;
}

interface State {
  config: ConfigView | null;
  printers: PrinterProfile[];
  objects: ObjectInfo[];
  selection: number[];
  supportPoints: Record<number, SupportPoint[]>;
  supportVersion: Record<number, number>;
  mode: Mode;
  ribbon: Ribbon;
  tool: Tool;
  panel: Panel;
  overhang: boolean;
  /** Drain hole settings used by the drill tool. */
  drill: { diameter: number; depth: number };
  label: { text: string; height: number; depth: number; mode: "emboss" | "engrave" };
  history: { can_undo: boolean; can_redo: boolean };
  measure: { a: [number, number, number]; b: [number, number, number] } | null;
  busy: string | null;
  sliceProgress: { done: number; total: number } | null;
  slice: SliceSummary | null;
  layer: number;
  devices: Record<string, DeviceEntry>;
  selectedDevice: string | null;
  deviceTab: "overview" | "files" | "history";
  downloads: Record<string, { done: number; total: number }>;
  /** Printer that receives files dropped on the window (file manager open). */
  dropTarget: string | null;
  /** Printer preselected in the open "send to printer" dialog (null = closed). */
  sendTo: string | "" | null;
  toasts: Toast[];

  setConfig: (c: ConfigView) => void;
  setPrinters: (p: PrinterProfile[]) => void;
  upsertObjects: (o: ObjectInfo[]) => void;
  replaceObjects: (o: ObjectInfo[]) => void;
  removeObjects: (ids: number[]) => void;
  select: (ids: number[]) => void;
  setSupportPoints: (id: number, pts: SupportPoint[]) => void;
  bumpSupports: (id: number) => void;
  setMode: (m: Mode) => void;
  setRibbon: (r: Ribbon) => void;
  setTool: (t: Tool) => void;
  setPanel: (p: Panel) => void;
  setOverhang: (v: boolean) => void;
  setDrill: (d: { diameter: number; depth: number }) => void;
  setLabel: (l: State["label"]) => void;
  setHistory: (h: { can_undo: boolean; can_redo: boolean }) => void;
  setMeasure: (m: State["measure"]) => void;
  setBusy: (b: string | null) => void;
  setSliceProgress: (p: { done: number; total: number } | null) => void;
  setSlice: (s: SliceSummary | null) => void;
  setLayer: (l: number) => void;
  updateDevice: (ip: string, patch: Partial<DeviceEntry>) => void;
  removeDevice: (ip: string) => void;
  selectDevice: (ip: string | null) => void;
  setDeviceTab: (t: "overview" | "files" | "history") => void;
  setDownload: (key: string, p: { done: number; total: number } | null) => void;
  setDropTarget: (ip: string | null) => void;
  setSendTo: (ip: string | "" | null) => void;
  toast: (kind: Toast["kind"], text: string) => void;
  dismissToast: (id: number) => void;
}

let toastId = 0;

export const useStore = create<State>((set) => ({
  config: null,
  printers: [],
  objects: [],
  selection: [],
  supportPoints: {},
  supportVersion: {},
  mode: "model",
  ribbon: "prepare",
  tool: "select",
  panel: null,
  overhang: false,
  drill: { diameter: 3, depth: 4 },
  label: { text: "OpenResin", height: 5, depth: 0.8, mode: "emboss" },
  history: { can_undo: false, can_redo: false },
  measure: null,
  busy: null,
  sliceProgress: null,
  slice: null,
  layer: 0,
  devices: {},
  selectedDevice: null,
  deviceTab: "overview",
  downloads: {},
  dropTarget: null,
  sendTo: null,
  toasts: [],

  setConfig: (config) => set({ config }),
  setPrinters: (printers) => set({ printers }),
  upsertObjects: (objs) =>
    set((s) => {
      const map = new Map(s.objects.map((o) => [o.id, o]));
      for (const o of objs) map.set(o.id, o);
      return { objects: [...map.values()], slice: null };
    }),
  replaceObjects: (objects) =>
    set((s) => ({
      objects,
      selection: s.selection.filter((id) => objects.some((o) => o.id === id)),
      slice: null,
    })),
  removeObjects: (ids) =>
    set((s) => ({
      objects: s.objects.filter((o) => !ids.includes(o.id)),
      selection: s.selection.filter((id) => !ids.includes(id)),
      slice: null,
    })),
  select: (selection) => set({ selection }),
  setSupportPoints: (id, pts) => set((s) => ({ supportPoints: { ...s.supportPoints, [id]: pts } })),
  bumpSupports: (id) =>
    set((s) => ({ supportVersion: { ...s.supportVersion, [id]: (s.supportVersion[id] ?? 0) + 1 }, slice: null })),
  setMode: (mode) => set({ mode }),
  setRibbon: (ribbon) => set({ ribbon, mode: "model", tool: "select", panel: null, measure: null }),
  setTool: (tool) => set({ tool, measure: null }),
  setPanel: (panel) => set((s) => ({ panel: s.panel === panel ? null : panel })),
  setOverhang: (overhang) => set({ overhang }),
  setDrill: (drill) => set({ drill }),
  setLabel: (label) => set({ label }),
  setHistory: (history) => set({ history }),
  setMeasure: (measure) => set({ measure }),
  setBusy: (busy) => set({ busy }),
  setSliceProgress: (sliceProgress) => set({ sliceProgress }),
  setSlice: (slice) => set({ slice, layer: 0 }),
  setLayer: (layer) => set({ layer }),
  updateDevice: (ip, patch) =>
    set((s) => {
      const prev: DeviceEntry = s.devices[ip] ?? {
        ip,
        name: ip,
        saved: false,
        connected: false,
        connecting: false,
      };
      return { devices: { ...s.devices, [ip]: { ...prev, ...patch } } };
    }),
  removeDevice: (ip) =>
    set((s) => {
      const d = { ...s.devices };
      delete d[ip];
      return { devices: d };
    }),
  selectDevice: (selectedDevice) => set({ selectedDevice }),
  setDeviceTab: (deviceTab) => set({ deviceTab }),
  setDownload: (key, p) =>
    set((s) => {
      const d = { ...s.downloads };
      if (p) d[key] = p;
      else delete d[key];
      return { downloads: d };
    }),
  setDropTarget: (dropTarget) => set({ dropTarget }),
  setSendTo: (sendTo) => set({ sendTo }),
  toast: (kind, text) =>
    set((s) => {
      const id = ++toastId;
      setTimeout(() => useStore.getState().dismissToast(id), kind === "error" ? 9000 : 4500);
      return { toasts: [...s.toasts, { id, kind, text }] };
    }),
  dismissToast: (id) => set((s) => ({ toasts: s.toasts.filter((t) => t.id !== id) })),
}));

export const savedToEntry = (d: SavedDevice): Partial<DeviceEntry> => ({
  name: d.name,
  model: d.model,
  saved: true,
});

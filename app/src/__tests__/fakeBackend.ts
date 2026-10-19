// In-memory stand-in for the Tauri backend (app/src-tauri). It keeps a small
// scene, undo history, config, presets and fake network printers so UI flows
// can be exercised end to end through the real `invoke` path (mockIPC).
import type {
  Aabb,
  ConfigView,
  DeviceStatusEvent,
  DiscoveredPrinter,
  FileEntry,
  MachineState,
  ObjectInfo,
  PrintSettings,
  PrinterProfile,
  SavedDevice,
  SliceSummary,
  SupportPoint,
  SupportSettings,
  Transform,
  Vec3,
} from "../api";
import type { MeshReport } from "../views/GeometryPanels";

export interface Call {
  cmd: string;
  /** Arguments as they would cross the IPC boundary (JSON, `undefined` dropped). */
  args: Record<string, unknown>;
}

interface Obj {
  id: number;
  name: string;
  /** Unscaled model size (mm); the model sits on z = 0 in local space. */
  size: Vec3;
  transform: Transform;
  supports: SupportPoint[];
  supportSettings: SupportSettings | null;
  mesh_key: number;
  subtract: boolean;
  cutouts: number;
  triangles: number;
}

export interface FakePrinter {
  ip: string;
  name: string;
  model: string;
  protocol: "sdcp" | "chitu-udp";
  /** false = connect_printer fails like an unreachable host. */
  reachable: boolean;
  /** Answers discovery broadcasts. */
  discoverable: boolean;
  state: MachineState;
  files: Record<string, FileEntry[] | null>;
  history: unknown[];
  camera: boolean;
  timeLapse: boolean;
}

export const PRINTER: PrinterProfile = {
  id: "elegoo-mars-5-ultra",
  vendor: "ELEGOO",
  name: "Mars 5 Ultra",
  resolution_x: 9024,
  resolution_y: 5120,
  size_x: 153.4,
  size_y: 77.8,
  size_z: 165,
  mirror: "lcd",
  formats: ["goo"],
  network: "sdcp",
};

export const OTHER_PRINTER: PrinterProfile = {
  id: "anycubic-photon-mono-x",
  vendor: "Anycubic",
  name: "Photon Mono X",
  resolution_x: 3840,
  resolution_y: 2400,
  size_x: 192,
  size_y: 120,
  size_z: 245,
  mirror: "lcd",
  formats: ["pwmx"],
};

export function defaultSettings(): PrintSettings {
  const normal = {
    exposure: 2.5,
    light_off_delay: 0.5,
    wait_before_lift: 0,
    wait_after_lift: 0,
    wait_after_retract: 0.5,
    lift: { distance: 6, speed: 65, distance2: 0, speed2: 0 },
    retract: { distance: 6, speed: 150, distance2: 0, speed2: 0 },
    light_pwm: 255,
  };
  return {
    name: "Standard resin",
    layer_height: 0.05,
    bottom_layers: 4,
    transition_layers: 0,
    bottom: { ...normal, exposure: 30, lift: { distance: 6, speed: 60, distance2: 0, speed2: 0 } },
    normal,
    wait_mode: true,
    anti_aliasing: 4,
    resin_density: 1.1,
    resin_price_per_liter: 30,
    currency: "€",
  };
}

/** Mirrors `SupportSettings::default()` in crates/core/src/support/mod.rs. */
export function defaultSupportSettings(): SupportSettings {
  return {
    lift_height: 5,
    spacing: 3,
    overhang_angle: 45,
    tip_length: 2.5,
    tip_diameter: [0.3, 0.45, 0.65],
    shaft_diameter: [0.8, 1.1, 1.6],
    base_diameter: 3.5,
    base_height: 0.8,
    raft: true,
    raft_thickness: 1,
    raft_margin: 1.5,
    model_contacts: true,
    detect_islands: true,
    island_layer_step: 0.1,
    default_size: "medium",
    segments: 8,
    cross_bracing: false,
    bracing_max_distance: 10,
    bracing_min_height: 8,
    bracing_diameter: 0.8,
  };
}

export function cleanReport(): MeshReport {
  return {
    vertices: 8,
    triangles: 12,
    shells: 1,
    boundary_edges: 0,
    holes: 0,
    non_manifold_edges: 0,
    inconsistent_edges: 0,
    degenerate_triangles: 0,
    duplicate_triangles: 0,
    inverted_shells: 0,
    cavities: 0,
    volume: 8000,
    watertight: true,
  };
}

/** Encodes a tetrahedron the way `encode_mesh` does (u32 counts, f32 xyz, u32 indices). */
export function meshBuffer(): ArrayBuffer {
  const pos = [0, 0, 0, 10, 0, 0, 0, 10, 0, 0, 0, 10];
  const idx = [0, 2, 1, 0, 1, 3, 0, 3, 2, 1, 2, 3];
  const buf = new ArrayBuffer(8 + pos.length * 4 + idx.length * 4);
  const dv = new DataView(buf);
  dv.setUint32(0, pos.length / 3, true);
  dv.setUint32(4, idx.length / 3, true);
  pos.forEach((v, i) => dv.setFloat32(8 + i * 4, v, true));
  idx.forEach((v, i) => dv.setUint32(8 + pos.length * 4 + i * 4, v, true));
  return buf;
}

const PNG = new Uint8Array([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]).buffer;

const clone = <T>(v: T): T => (v === undefined ? v : JSON.parse(JSON.stringify(v)));
const basename = (p: string) => p.split(/[\\/]/).pop() ?? p;
const stem = (p: string) => basename(p).replace(/\.[^.]+$/, "");

export class FakeBackend {
  calls: Call[] = [];
  /** Commands the fake does not implement (a test should never hit these). */
  unknown: string[] = [];
  private failures = new Map<string, { error: unknown; sticky: boolean }>();
  private gates = new Map<string, Promise<void>>();

  objects: Obj[] = [];
  private nextId = 1;
  private nextMeshKey = 1;
  undoStack: Obj[][] = [];
  redoStack: Obj[][] = [];
  /** Sizes for imported files by file stem (default 20 mm cube). */
  modelSizes: Record<string, Vec3> = {};
  reports: Record<number, MeshReport> = {};

  config: ConfigView = {
    printer: clone(PRINTER),
    settings: defaultSettings(),
    support_settings: defaultSupportSettings(),
    devices: [],
    theme: "dark",
    formats: [
      { id: "goo", extension: "goo", description: "ELEGOO GOO" },
      { id: "ctb", extension: "ctb", description: "CTB (unencrypted)" },
      { id: "sl1", extension: "sl1", description: "Prusa SL1" },
      { id: "zip", extension: "zip", description: "CHITUBOX ZIP" },
    ],
  };
  printers: PrinterProfile[] = [clone(PRINTER), clone(OTHER_PRINTER)];
  userPresets: PrintSettings[] = [];
  projects = new Map<string, Obj[]>();
  exported: { path: string; withSupports: boolean }[] = [];
  savedSlices: string[] = [];
  slice: SliceSummary | null = null;
  startupFiles: string[] = [];
  startupFlags: string[] = [];

  network: FakePrinter[] = [];
  connected = new Set<string>();
  uploads: { ip: string; start: boolean; name?: string; path?: string }[] = [];
  commands: { ip: string; command: string; file?: string }[] = [];

  /** Results returned by the dialog plugin, consumed in order (default: cancelled). */
  openResults: (string | string[] | null)[] = [];
  saveResults: (string | null)[] = [];
  /** Answers for `ask` (plugin:dialog|message); default yes. */
  askAnswers: boolean[] = [];
  asked: string[] = [];

  // ------------------------------------------------------------ test helpers

  /** Makes the next call (or every call with `sticky`) of `cmd` fail with `error`. */
  fail(cmd: string, error: unknown = `${cmd} failed`, sticky = false) {
    this.failures.set(cmd, { error, sticky });
  }

  /** Holds `cmd` until the returned function is called. */
  hold(cmd: string): () => void {
    let release!: () => void;
    this.gates.set(cmd, new Promise<void>((r) => (release = r)));
    return () => {
      this.gates.delete(cmd);
      release();
    };
  }

  callsOf(cmd: string): Record<string, unknown>[] {
    return this.calls.filter((c) => c.cmd === cmd).map((c) => c.args);
  }

  lastCall(cmd: string): Record<string, unknown> | undefined {
    const l = this.callsOf(cmd);
    return l[l.length - 1];
  }

  addObject(name: string, size: Vec3 = [20, 20, 20], at: Vec3 = [0, 0, 0]): ObjectInfo {
    const o = this.newObj(name, size);
    o.transform.translation = [...at];
    this.objects.push(o);
    return this.info(o);
  }

  addPrinter(p: Partial<FakePrinter> & { ip: string }): FakePrinter {
    const full: FakePrinter = {
      name: "Mars 5 Ultra",
      model: "ELEGOO Mars 5 Ultra",
      protocol: "sdcp",
      reachable: true,
      discoverable: true,
      state: "idle",
      files: {
        "/local": [
          { path: "/local/benchy.goo", is_dir: false, size: 12_345_678 },
          { path: "/local/cube.goo", is_dir: false, size: 2_048 },
          { path: "/local/minis", is_dir: true, size: null },
        ],
        "/local/minis": [{ path: "/local/minis/knight.goo", is_dir: false, size: 4_000_000 }],
        "/usb": null,
      },
      history: [],
      camera: true,
      timeLapse: false,
      ...p,
    };
    this.network.push(full);
    return full;
  }

  info(o: Obj): ObjectInfo {
    const t = o.transform;
    const s = [0, 1, 2].map((i) => o.size[i] * Math.abs(t.scale[i])) as Vec3;
    const bounds: Aabb = {
      min: [t.translation[0] - s[0] / 2, t.translation[1] - s[1] / 2, t.translation[2]],
      max: [t.translation[0] + s[0] / 2, t.translation[1] + s[1] / 2, t.translation[2] + s[2]],
    };
    return {
      id: o.id,
      name: o.name,
      triangles: o.triangles,
      transform: clone(t),
      bounds,
      support_points: o.supports.length,
      volume_mm3: s[0] * s[1] * s[2],
      mesh_key: o.mesh_key,
      subtract: o.subtract,
      cutouts: o.cutouts,
    };
  }

  infos(): ObjectInfo[] {
    return this.objects.map((o) => this.info(o));
  }

  // ------------------------------------------------------------ internals

  private newObj(name: string, size: Vec3): Obj {
    return {
      id: this.nextId++,
      name,
      size: [...size],
      transform: { translation: [0, 0, 0], rotation: [0, 0, 0, 1], scale: [1, 1, 1] },
      supports: [],
      supportSettings: null,
      mesh_key: this.nextMeshKey++,
      subtract: false,
      cutouts: 0,
      triangles: 12,
    };
  }

  private get(id: unknown): Obj {
    const o = this.objects.find((x) => x.id === id);
    if (!o) throw "object not found";
    return o;
  }

  private checkpoint() {
    this.undoStack.push(clone(this.objects));
    this.redoStack = [];
  }

  private invalidateSlice() {
    this.slice = null;
  }

  /** Row packing with a 3 mm gap, centred on the plate; false when it overflows. */
  private arrange(): boolean {
    const gap = 3;
    const p = this.config.printer;
    let x = 0;
    let rowY = 0;
    let rowH = 0;
    const placed: { o: Obj; x: number; y: number }[] = [];
    for (const o of this.objects) {
      const w = this.info(o).bounds.max[0] - this.info(o).bounds.min[0];
      const d = this.info(o).bounds.max[1] - this.info(o).bounds.min[1];
      if (x > 0 && x + w > p.size_x) {
        x = 0;
        rowY += rowH + gap;
        rowH = 0;
      }
      placed.push({ o, x: x + w / 2, y: rowY + d / 2 });
      x += w + gap;
      rowH = Math.max(rowH, d);
    }
    const totalW = Math.max(0, ...placed.map((q) => q.x + (this.info(q.o).bounds.max[0] - this.info(q.o).bounds.min[0]) / 2));
    const totalD = rowY + rowH;
    for (const q of placed) {
      q.o.transform.translation[0] = q.x - totalW / 2;
      q.o.transform.translation[1] = q.y - totalD / 2;
    }
    return totalW <= p.size_x && totalD <= p.size_y;
  }

  private dropTo(o: Obj, z: number) {
    o.transform.translation[2] = z;
  }

  private summary(format: string | null, durationMs: number, name: string): SliceSummary {
    const s = this.config.settings;
    const p = this.config.printer;
    const height = Math.max(0, ...this.objects.map((o) => this.info(o).bounds.max[2]));
    const layers = Math.max(1, Math.ceil(height / s.layer_height));
    const f = format ?? p.formats[0] ?? "zip";
    const ext = this.config.formats.find((x) => x.id === f)?.extension ?? f;
    const volume = this.objects.reduce((a, o) => a + this.info(o).volume_mm3, 0) / 1000;
    const time = s.bottom_layers * (s.bottom.exposure + 6) + (layers - s.bottom_layers) * (s.normal.exposure + 6);
    return {
      format: f,
      extension: ext,
      layer_count: layers,
      print_time_s: time,
      volume_ml: volume,
      weight_g: volume * s.resin_density,
      cost: (volume / 1000) * s.resin_price_per_liter,
      currency: s.currency,
      height_mm: layers * s.layer_height,
      file_size: 1_234_567,
      duration_ms: durationMs,
      info: {
        format: f,
        machine_name: p.name,
        resolution_x: p.resolution_x,
        resolution_y: p.resolution_y,
        size_x: p.size_x,
        size_y: p.size_y,
        layer_count: layers,
        layer_height: s.layer_height,
        exposure: s.normal.exposure,
        bottom_exposure: s.bottom.exposure,
        bottom_layers: s.bottom_layers,
        print_time_s: time,
        volume_ml: volume,
        mirror_x: true,
        mirror_y: false,
        anti_aliasing: s.anti_aliasing,
      },
      layer_areas: Array.from({ length: layers }, () => 400),
      suggested_name: `${name}.${ext}`,
      warnings: [],
    };
  }

  private device(ip: unknown): FakePrinter {
    const d = this.network.find((x) => x.ip === ip);
    if (!d || !d.reachable) throw `could not connect to ${String(ip)}: connection refused`;
    return d;
  }

  private connectedDevice(ip: unknown): FakePrinter {
    if (!this.connected.has(String(ip))) throw "printer not connected";
    return this.device(ip);
  }

  private status(d: FakePrinter): DeviceStatusEvent {
    return {
      ip: d.ip,
      status: {
        state: d.state,
        free_storage: 4_000_000_000,
        uv_led_temp: 31.5,
        box_temp: 0,
        camera_connected: d.camera,
        time_lapse: d.timeLapse,
      },
      attributes: { Name: d.name, MachineName: d.model, FirmwareVersion: "V1.2.3", Resolution: "9024x5120", NetworkStatus: "wlan" },
      connected: true,
      protocol: d.protocol,
      camera: d.camera,
      downloads: true,
    };
  }

  // ------------------------------------------------------------ IPC entry

  handle = async (cmd: string, payload?: unknown): Promise<unknown> => {
    const args = (payload ?? {}) as Record<string, any>;
    this.calls.push({ cmd, args: clone(args) ?? {} });
    const gate = this.gates.get(cmd);
    if (gate) await gate;
    const f = this.failures.get(cmd);
    if (f) {
      if (!f.sticky) this.failures.delete(cmd);
      throw f.error;
    }
    return this.dispatch(cmd, args);
  };

  private dispatch(cmd: string, a: Record<string, any>): unknown {
    switch (cmd) {
      // ---------------------------------------------------------- dialogs
      case "plugin:dialog|open":
        return this.openResults.length ? this.openResults.shift() : null;
      case "plugin:dialog|save":
        return this.saveResults.length ? this.saveResults.shift() : null;
      case "plugin:dialog|message": {
        this.asked.push(String(a.message));
        const yes = this.askAnswers.length ? this.askAnswers.shift() : true;
        return yes ? "Yes" : "No";
      }

      // ---------------------------------------------------------- config
      case "startup_files":
        return [...this.startupFiles];
      case "startup_flags":
        return [...this.startupFlags];
      case "list_printers":
        return clone(this.printers);
      case "get_config":
        return clone({ ...this.config, devices: this.config.devices });
      case "set_printer": {
        const p = this.printers.find((x) => x.id === a.id);
        if (!p) throw `unknown printer ${a.id}`;
        this.config.printer = clone(p);
        this.invalidateSlice();
        return clone(this.config);
      }
      case "set_settings":
        if (!(a.settings?.layer_height > 0)) throw "layer height must be positive";
        this.config.settings = clone(a.settings);
        this.invalidateSlice();
        return null;
      case "set_support_settings":
        this.config.support_settings = clone(a.settings);
        return null;
      case "save_custom_printer": {
        const p = clone(a.printer) as PrinterProfile;
        this.printers = [...this.printers.filter((x) => x.id !== p.id), p];
        this.config.printer = p;
        return clone(this.config);
      }
      case "set_theme":
        this.config.theme = a.theme;
        return null;

      // ---------------------------------------------------------- presets
      case "list_presets": {
        const abs = defaultSettings();
        abs.name = "ABS-like resin";
        abs.normal.exposure = 3;
        abs.bottom.exposure = 35;
        const builtin = [defaultSettings(), abs].filter(
          (b) => !this.userPresets.some((u) => u.name === b.name),
        );
        return [
          ...builtin.map((settings) => ({ settings, builtin: true })),
          ...this.userPresets.map((settings) => ({ settings: clone(settings), builtin: false })),
        ];
      }
      case "save_preset": {
        if (!String(a.settings?.name ?? "").trim()) throw "the preset needs a name";
        this.userPresets = [...this.userPresets.filter((p) => p.name !== a.settings.name), clone(a.settings)];
        this.config.settings = clone(a.settings);
        return null;
      }
      case "delete_preset":
        this.userPresets = this.userPresets.filter((p) => p.name !== a.name);
        return null;

      // ---------------------------------------------------------- scene
      case "import_models": {
        const loaded = (a.paths as string[]).map((p) => {
          if (!/\.(stl|obj|3mf)$/i.test(p)) throw `unsupported file type: ${p}`;
          return this.newObj(stem(p), this.modelSizes[stem(p)] ?? [20, 20, 20]);
        });
        this.checkpoint();
        this.objects.push(...loaded);
        if (this.objects.length > 1) this.arrange();
        this.invalidateSlice();
        return loaded.map((o) => this.info(o));
      }
      case "scene_objects":
        return this.infos();
      case "get_mesh":
      case "get_support_mesh":
      case "get_cutout_mesh":
        this.get(a.id);
        return meshBuffer();
      case "set_transform": {
        this.checkpoint();
        const o = this.get(a.id);
        const old = o.transform;
        const t = a.transform as Transform;
        const sameRot = old.rotation.every((v, i) => Math.abs(v - t.rotation[i]) < 1e-6);
        const xyMirrorOnly =
          sameRot &&
          old.scale.every((v, i) => Math.abs(Math.abs(v) - Math.abs(t.scale[i])) < 1e-6) &&
          Math.sign(old.scale[2]) === Math.sign(t.scale[2]);
        o.transform = clone(t);
        if (!xyMirrorOnly) o.supports = [];
        if (a.keepOnPlate) this.dropTo(o, o.supports.length ? (o.supportSettings?.lift_height ?? 0) : 0);
        this.invalidateSlice();
        return this.info(o);
      }
      case "remove_objects":
        this.checkpoint();
        this.objects = this.objects.filter((o) => !(a.ids as number[]).includes(o.id));
        this.invalidateSlice();
        return null;
      case "duplicate_object": {
        this.checkpoint();
        const src = this.get(a.id);
        const out: Obj[] = [];
        for (let i = 0; i < Math.min(100, Math.max(1, a.count)); i++) {
          const c = { ...clone(src), id: this.nextId++ };
          this.objects.push(c);
          out.push(c);
        }
        this.arrange();
        this.invalidateSlice();
        return out.map((o) => this.info(o));
      }
      case "arrange": {
        this.checkpoint();
        const fits = this.arrange();
        this.invalidateSlice();
        return [fits, this.infos()];
      }
      case "lay_flat": {
        this.checkpoint();
        const o = this.get(a.id);
        o.transform.rotation = [Math.SQRT1_2, 0, 0, Math.SQRT1_2];
        o.supports = [];
        this.dropTo(o, 0);
        return this.info(o);
      }
      case "rename_object": {
        const o = this.get(a.id);
        o.name = a.name;
        return this.info(o);
      }
      case "add_primitive": {
        if (!["cube", "cylinder", "sphere", "cone"].includes(a.kind)) throw `unknown shape ${a.kind}`;
        const s = a.size ?? 20;
        this.checkpoint();
        const name = a.kind[0].toUpperCase() + a.kind.slice(1);
        this.objects.push(this.newObj(`${name} ${s} mm`, [s, s, s]));
        if (this.objects.length > 1) this.arrange();
        return this.infos();
      }

      // ---------------------------------------------------------- supports
      case "auto_supports": {
        this.checkpoint();
        // like scene_cmds::auto_supports: the settings apply to these objects only;
        // the saved defaults change through set_support_settings alone
        const settings = clone(a.settings) as SupportSettings;
        const out: ObjectInfo[] = [];
        for (const id of a.ids as number[]) {
          const o = this.objects.find((x) => x.id === id);
          if (!o) continue;
          this.dropTo(o, settings.lift_height);
          o.supportSettings = settings;
          const n = Math.max(1, Math.round((o.size[0] * o.size[1]) / (settings.spacing * settings.spacing)));
          o.supports = Array.from({ length: n }, (_, i) => ({
            position: [i, 0, settings.lift_height] as Vec3,
            normal: [0, 0, -1] as Vec3,
            size: settings.default_size,
          }));
          out.push(this.info(o));
        }
        this.invalidateSlice();
        return out;
      }
      case "get_support_points":
        return clone(this.get(a.id).supports);
      case "set_support_points": {
        this.checkpoint();
        const o = this.get(a.id);
        o.supports = clone(a.points);
        return this.info(o);
      }
      case "clear_supports": {
        this.checkpoint();
        const o = this.get(a.id);
        o.supports = [];
        this.dropTo(o, 0);
        return this.info(o);
      }

      // ---------------------------------------------------------- project / export
      case "save_project":
        this.projects.set(a.path, clone(this.objects));
        return null;
      case "open_project": {
        const p = this.projects.get(a.path);
        if (!p) throw `could not open ${a.path}`;
        this.objects = clone(p);
        this.undoStack = [];
        this.redoStack = [];
        return this.infos();
      }
      case "new_project":
        this.checkpoint();
        this.objects = [];
        this.invalidateSlice();
        return null;
      case "export_stl":
        this.exported.push({ path: a.path, withSupports: a.withSupports });
        return null;

      // ---------------------------------------------------------- slicing
      case "slice":
        if (!this.objects.length) throw "the plate is empty";
        this.slice = this.summary(a.format ?? null, 850, "plate");
        return clone(this.slice);
      case "cancel_slice":
        return null;
      case "get_layer_image":
        if (!this.slice) throw "nothing sliced";
        if (a.index < 0 || a.index >= this.slice.layer_count) throw "layer out of range";
        return PNG.slice(0);
      case "save_slice":
        if (!this.slice) throw "nothing sliced";
        this.savedSlices.push(a.path);
        return null;
      case "open_sliced_file": {
        if (!/\.(goo|ctb|sl1|zip)$/i.test(a.path)) throw "unsupported file";
        this.slice = { ...this.summary(null, 0, stem(a.path)), suggested_name: basename(a.path) };
        return clone(this.slice);
      }

      // ---------------------------------------------------------- geometry
      case "analyze_object":
        this.get(a.id);
        return clone(this.reports[a.id] ?? cleanReport());
      case "repair_objects": {
        this.checkpoint();
        const results = (a.ids as number[]).map((id) => {
          const o = this.get(id);
          const before = this.reports[id] ?? cleanReport();
          const after = cleanReport();
          this.reports[id] = after;
          o.mesh_key = this.nextMeshKey++;
          return {
            id,
            name: o.name,
            report: { holes_filled: a.options?.fill_holes ? before.holes : 0, triangles_flipped: 0, duplicates_removed: 0, degenerate_removed: 0, vertices_welded: 0, after },
          };
        });
        return [results, this.infos()];
      }
      case "split_object": {
        const o = this.get(a.id);
        if (!o.name.includes("multi")) return [1, this.infos()];
        this.checkpoint();
        this.objects = this.objects.filter((x) => x !== o);
        this.objects.push(this.newObj(`${o.name} (part 1)`, o.size), this.newObj(`${o.name} (part 2)`, o.size));
        this.arrange();
        return [2, this.infos()];
      }
      case "cut_object": {
        if (!["x", "y", "z"].includes(a.axis)) throw `unknown axis ${a.axis}`;
        this.checkpoint();
        const o = this.get(a.id);
        const i = ["x", "y", "z"].indexOf(a.axis);
        const b = this.info(o).bounds;
        const lo = [...o.size] as Vec3;
        const hi = [...o.size] as Vec3;
        lo[i] = a.position - b.min[i];
        hi[i] = b.max[i] - a.position;
        if (lo[i] <= 0 || hi[i] <= 0) throw "the plane does not cut the model";
        this.objects = this.objects.filter((x) => x !== o);
        this.objects.push(this.newObj(`${o.name} (cut 1)`, lo), this.newObj(`${o.name} (cut 2)`, hi));
        this.arrange();
        return this.infos();
      }
      case "set_subtract": {
        this.checkpoint();
        for (const id of a.ids as number[]) this.get(id).subtract = a.subtract;
        return this.infos();
      }
      case "hollow_object": {
        this.checkpoint();
        const o = this.get(a.id);
        o.cutouts += 1;
        const s = this.info(o).bounds;
        const w = a.options.wall_thickness * 2;
        const cavity = Math.max(0, s.max[0] - s.min[0] - w) * Math.max(0, s.max[1] - s.min[1] - w) * Math.max(0, s.max[2] - s.min[2] - w);
        return [cavity, this.info(o)];
      }
      case "add_drain_hole": {
        this.checkpoint();
        const o = this.get(a.id);
        o.cutouts += 1;
        return this.info(o);
      }
      case "clear_cutouts": {
        this.checkpoint();
        const o = this.get(a.id);
        o.cutouts = 0;
        return this.info(o);
      }
      case "add_label": {
        if (!String(a.text ?? "").trim()) throw "the label needs text";
        this.checkpoint();
        const o = this.get(a.id);
        if (a.options.mode === "emboss") o.mesh_key = this.nextMeshKey++;
        else o.cutouts += 1;
        return this.info(o);
      }
      case "add_platform": {
        this.checkpoint();
        const o = this.get(a.id);
        o.size[2] += a.options.thickness;
        o.size[0] += 2 * a.options.margin;
        o.size[1] += 2 * a.options.margin;
        o.mesh_key = this.nextMeshKey++;
        o.supports = [];
        this.dropTo(o, 0);
        return this.info(o);
      }

      // ---------------------------------------------------------- edit
      case "history_state":
        return { can_undo: this.undoStack.length > 0, can_redo: this.redoStack.length > 0 };
      case "undo": {
        const prev = this.undoStack.pop();
        if (!prev) throw "nothing to undo";
        this.redoStack.push(this.objects);
        this.objects = prev;
        this.invalidateSlice();
        return this.infos();
      }
      case "redo": {
        const next = this.redoStack.pop();
        if (!next) throw "nothing to redo";
        this.undoStack.push(this.objects);
        this.objects = next;
        this.invalidateSlice();
        return this.infos();
      }
      case "fill_plate": {
        this.checkpoint();
        const src = this.get(a.id);
        const spacing = a.spacing ?? 3;
        const b = this.info(src).bounds;
        const p = this.config.printer;
        const perRow = Math.floor((p.size_x + spacing) / (b.max[0] - b.min[0] + spacing));
        const rows = Math.floor((p.size_y + spacing) / (b.max[1] - b.min[1] + spacing));
        const added = Math.max(0, perRow * rows - this.objects.length);
        for (let i = 0; i < added; i++) this.objects.push({ ...clone(src), id: this.nextId++ });
        this.arrange();
        this.invalidateSlice();
        return [added, this.infos()];
      }
      case "align_objects": {
        const group = (a.ids as number[]).map((id) => this.objects.find((o) => o.id === id)).filter(Boolean) as Obj[];
        if (!group.length) throw "nothing selected";
        this.checkpoint();
        const bs = group.map((o) => this.info(o).bounds);
        const gmin = [0, 1, 2].map((i) => Math.min(...bs.map((b) => b.min[i])));
        const gmax = [0, 1, 2].map((i) => Math.max(...bs.map((b) => b.max[i])));
        const gc = [0, 1].map((i) => (gmin[i] + gmax[i]) / 2);
        group.forEach((o, k) => {
          const b = bs[k];
          const c = [0, 1].map((i) => (b.min[i] + b.max[i]) / 2);
          const t = o.transform.translation;
          switch (a.mode) {
            case "left": t[0] += gmin[0] - b.min[0]; break;
            case "right": t[0] += gmax[0] - b.max[0]; break;
            case "front": t[1] += gmin[1] - b.min[1]; break;
            case "back": t[1] += gmax[1] - b.max[1]; break;
            case "center_x": t[0] += gc[0] - c[0]; break;
            case "center_y": t[1] += gc[1] - c[1]; break;
            case "plate_center": t[0] -= gc[0]; t[1] -= gc[1]; break;
            case "drop": t[2] = o.supports.length ? (o.supportSettings?.lift_height ?? 0) : 0; break;
            default: throw `unknown align mode ${a.mode}`;
          }
        });
        this.invalidateSlice();
        return this.infos();
      }
      case "combine_objects": {
        const ids = a.ids as number[];
        if (ids.length < 2) throw "select at least two models to combine";
        this.checkpoint();
        const parts = this.objects.filter((o) => ids.includes(o.id));
        this.objects = this.objects.filter((o) => !ids.includes(o.id));
        const bs = parts.map((o) => this.info(o).bounds);
        const size: Vec3 = [0, 1, 2].map((i) => Math.max(...bs.map((b) => b.max[i])) - Math.min(...bs.map((b) => b.min[i]))) as Vec3;
        const merged = this.newObj(`${parts[0]?.name ?? ""} (combined)`, size);
        merged.triangles = parts.reduce((s, o) => s + o.triangles, 0);
        this.objects.push(merged);
        this.invalidateSlice();
        return this.infos();
      }
      case "auto_orient": {
        this.checkpoint();
        const goal = a.options?.goal ?? "balanced";
        const q: Record<string, [number, number, number, number]> = {
          balanced: [0.3826834, 0, 0, 0.9238795],
          fewest_supports: [Math.SQRT1_2, 0, 0, Math.SQRT1_2],
          lowest_height: [0, Math.SQRT1_2, 0, Math.SQRT1_2],
        };
        for (const id of a.ids as number[]) {
          const o = this.objects.find((x) => x.id === id);
          if (!o) continue;
          o.transform.rotation = q[goal];
          o.supports = [];
          this.dropTo(o, 0);
        }
        this.invalidateSlice();
        return this.infos();
      }

      // ---------------------------------------------------------- devices
      case "discover_printers":
        return this.network.filter((d) => d.discoverable).map(
          (d): DiscoveredPrinter => ({ ip: d.ip, protocol: d.protocol, name: d.name, model: d.model, protocol_version: "V3.0.0" }),
        );
      case "saved_devices":
        return clone(this.config.devices);
      case "save_device": {
        const d = a.device as SavedDevice;
        if (!/^\d+\.\d+\.\d+\.\d+$/.test(d.ip)) throw `invalid IP address: ${d.ip}`;
        this.config.devices = [...this.config.devices.filter((x) => x.ip !== d.ip), { ip: d.ip, name: d.name, model: d.model ?? null }];
        return clone(this.config.devices);
      }
      case "forget_device":
        this.config.devices = this.config.devices.filter((x) => x.ip !== a.ip);
        this.connected.delete(a.ip);
        return clone(this.config.devices);
      case "connect_printer": {
        const d = this.device(a.ip);
        this.connected.add(d.ip);
        return this.status(d);
      }
      case "disconnect_printer":
        this.connected.delete(a.ip);
        return null;
      case "printer_files": {
        const d = this.connectedDevice(a.ip);
        const list = d.files[a.dir ?? "/local"];
        if (list == null) throw "no such directory";
        return clone(list);
      }
      case "printer_delete": {
        const d = this.connectedDevice(a.ip);
        const failed: string[] = [];
        for (const f of a.files as string[]) {
          const dir = f.slice(0, f.lastIndexOf("/"));
          const list = d.files[dir];
          if (!list?.some((e) => e.path === f)) failed.push(f);
          else d.files[dir] = list.filter((e) => e.path !== f);
        }
        return failed;
      }
      case "printer_history":
        return clone(this.connectedDevice(a.ip).history);
      case "printer_command": {
        const d = this.connectedDevice(a.ip);
        if (!["pause", "resume", "stop", "start", "refresh"].includes(a.command)) throw `unknown command ${a.command}`;
        if (a.command === "start" && !a.file) throw "no file given";
        if (a.command === "start" && d.state !== "idle") throw "the printer is busy";
        this.commands.push({ ip: a.ip, command: a.command, file: a.file });
        if (a.command === "start") d.state = "printing";
        if (a.command === "pause") d.state = "paused";
        if (a.command === "resume") d.state = "printing";
        if (a.command === "stop") d.state = "stopped";
        return null;
      }
      case "upload_to_printer": {
        const d = this.connectedDevice(a.ip);
        if (!a.path && !this.slice) throw "nothing sliced";
        const name = a.name ?? (a.path ? basename(a.path) : this.slice!.suggested_name);
        this.uploads.push({ ip: a.ip, start: a.start, name: a.name, path: a.path });
        const list = d.files["/local"] ?? [];
        d.files["/local"] = [...list.filter((f) => f.path !== `/local/${name}`), { path: `/local/${name}`, is_dir: false, size: 1000 }];
        if (a.start) d.state = "printing";
        return name;
      }
      case "cancel_upload":
      case "cancel_download":
      case "camera_stop":
        return null;
      case "printer_set_time_lapse":
        this.connectedDevice(a.ip).timeLapse = a.enable;
        return null;
      case "printer_rename":
        this.connectedDevice(a.ip).name = a.name;
        return null;
      case "printer_download":
        this.connectedDevice(a.ip);
        return 1000;
      case "printer_thumbnail":
        this.connectedDevice(a.ip);
        return PNG.slice(0);
      case "camera_start": {
        const d = this.connectedDevice(a.ip);
        if (!d.camera) throw "this printer has no network camera";
        return { stream_url: `http://127.0.0.1:9999/${d.ip}/stream.mjpg`, snapshot_url: `http://127.0.0.1:9999/${d.ip}/snap.jpg`, source: "rtsp" };
      }
      case "camera_snapshot":
        this.connectedDevice(a.ip);
        return null;
    }
    this.unknown.push(cmd);
    throw `unknown command ${cmd}`;
  }
}

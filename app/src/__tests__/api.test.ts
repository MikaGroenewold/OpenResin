import { mockIPC } from "@tauri-apps/api/mocks";
import { beforeEach, describe, expect, it } from "vitest";
import { api, decodeMesh, errorText, type AlignMode, type SupportSize, type Transform } from "../api";
import { meshBuffer } from "./fakeBackend";
import { registeredCommands, rustCommands, rustEnumVariants, rustMatchArms, rustStructFields } from "./rust";

type Recorded = { cmd: string; args: Record<string, unknown> };
let calls: Recorded[] = [];

const BINARY = new Set(["get_mesh", "get_support_mesh", "get_cutout_mesh"]);

beforeEach(() => {
  calls = [];
  mockIPC((cmd, args) => {
    // what really crosses the IPC boundary is JSON: undefined keys disappear
    calls.push({ cmd, args: JSON.parse(JSON.stringify(args ?? {})) });
    return BINARY.has(cmd) ? meshBuffer() : null;
  });
});

const T: Transform = { translation: [1, 2, 3], rotation: [0, 0, 0, 1], scale: [1, -1, 1] };
const settings = { name: "x" } as never;

/** One call per wrapper with every (optional) argument given. */
const CASES: Record<keyof typeof api, [string, () => Promise<unknown>, Record<string, unknown>]> = {
  startupFiles: ["startup_files", () => api.startupFiles(), {}],
  startupFlags: ["startup_flags", () => api.startupFlags(), {}],
  listPrinters: ["list_printers", () => api.listPrinters(), {}],
  getConfig: ["get_config", () => api.getConfig(), {}],
  setPrinter: ["set_printer", () => api.setPrinter("elegoo"), { id: "elegoo" }],
  setSettings: ["set_settings", () => api.setSettings(settings), { settings: { name: "x" } }],
  setSupportSettings: ["set_support_settings", () => api.setSupportSettings(settings), { settings: { name: "x" } }],
  saveCustomPrinter: ["save_custom_printer", () => api.saveCustomPrinter({ id: "c" } as never), { printer: { id: "c" } }],
  setTheme: ["set_theme", () => api.setTheme("light"), { theme: "light" }],
  importModels: ["import_models", () => api.importModels(["a.stl"]), { paths: ["a.stl"] }],
  sceneObjects: ["scene_objects", () => api.sceneObjects(), {}],
  getMesh: ["get_mesh", () => api.getMesh(4), { id: 4 }],
  setTransform: ["set_transform", () => api.setTransform(4, T, true), { id: 4, transform: T, keepOnPlate: true }],
  removeObjects: ["remove_objects", () => api.removeObjects([1, 2]), { ids: [1, 2] }],
  duplicateObject: ["duplicate_object", () => api.duplicateObject(1, 3), { id: 1, count: 3 }],
  arrange: ["arrange", () => api.arrange(), {}],
  layFlat: ["lay_flat", () => api.layFlat(1, 7), { id: 1, face: 7 }],
  listPresets: ["list_presets", () => api.listPresets(), {}],
  savePreset: ["save_preset", () => api.savePreset(settings), { settings: { name: "x" } }],
  deletePreset: ["delete_preset", () => api.deletePreset("Mine"), { name: "Mine" }],
  analyzeObject: ["analyze_object", () => api.analyzeObject(2), { id: 2 }],
  repairObjects: ["repair_objects", () => api.repairObjects([2], { fill_holes: true }), { ids: [2], options: { fill_holes: true } }],
  splitObject: ["split_object", () => api.splitObject(2), { id: 2 }],
  cutObject: ["cut_object", () => api.cutObject(2, "z", 10), { id: 2, axis: "z", position: 10 }],
  setSubtract: ["set_subtract", () => api.setSubtract([2], true), { ids: [2], subtract: true }],
  hollowObject: [
    "hollow_object",
    () => api.hollowObject(2, { wall_thickness: 2, voxel_size: 0 }),
    { id: 2, options: { wall_thickness: 2, voxel_size: 0 } },
  ],
  addDrainHole: [
    "add_drain_hole",
    () => api.addDrainHole(2, [1, 2, 3], [0, 0, -1], 3, 4),
    { id: 2, position: [1, 2, 3], normal: [0, 0, -1], diameter: 3, depth: 4 },
  ],
  clearCutouts: ["clear_cutouts", () => api.clearCutouts(2), { id: 2 }],
  getCutoutMesh: ["get_cutout_mesh", () => api.getCutoutMesh(2), { id: 2 }],
  addPrimitive: ["add_primitive", () => api.addPrimitive("cone", 15), { kind: "cone", size: 15 }],
  addLabel: [
    "add_label",
    () => api.addLabel(2, "Hi", { height: 5, depth: 0.8, mode: "engrave" }, [1, 1, 1], [0, 1, 0]),
    { id: 2, text: "Hi", options: { height: 5, depth: 0.8, mode: "engrave" }, position: [1, 1, 1], normal: [0, 1, 0] },
  ],
  addPlatform: [
    "add_platform",
    () => api.addPlatform(2, { margin: 2, thickness: 1.5, chamfer: 1 }),
    { id: 2, options: { margin: 2, thickness: 1.5, chamfer: 1 } },
  ],
  historyState: ["history_state", () => api.historyState(), {}],
  undo: ["undo", () => api.undo(), {}],
  redo: ["redo", () => api.redo(), {}],
  fillPlate: ["fill_plate", () => api.fillPlate(2, 3), { id: 2, spacing: 3 }],
  alignObjects: ["align_objects", () => api.alignObjects([1, 2], "left"), { ids: [1, 2], mode: "left" }],
  combineObjects: ["combine_objects", () => api.combineObjects([1, 2]), { ids: [1, 2] }],
  autoOrient: [
    "auto_orient",
    () => api.autoOrient([1], { goal: "lowest_height" }),
    { ids: [1], options: { goal: "lowest_height" } },
  ],
  renameObject: ["rename_object", () => api.renameObject(1, "Benchy"), { id: 1, name: "Benchy" }],
  autoSupports: ["auto_supports", () => api.autoSupports([1], settings), { ids: [1], settings: { name: "x" } }],
  getSupportPoints: ["get_support_points", () => api.getSupportPoints(1), { id: 1 }],
  setSupportPoints: [
    "set_support_points",
    () => api.setSupportPoints(1, [{ position: [1, 2, 3], normal: [0, 0, -1], size: "heavy" }]),
    { id: 1, points: [{ position: [1, 2, 3], normal: [0, 0, -1], size: "heavy" }] },
  ],
  clearSupports: ["clear_supports", () => api.clearSupports(1), { id: 1 }],
  getSupportMesh: ["get_support_mesh", () => api.getSupportMesh(1), { id: 1 }],
  saveProject: ["save_project", () => api.saveProject("/p.orproj"), { path: "/p.orproj" }],
  openProject: ["open_project", () => api.openProject("/p.orproj"), { path: "/p.orproj" }],
  newProject: ["new_project", () => api.newProject(), {}],
  exportStl: ["export_stl", () => api.exportStl("/p.stl", true), { path: "/p.stl", withSupports: true }],
  slice: ["slice", () => api.slice("goo", "data:image/png;base64,AA"), { format: "goo", thumbnail: "data:image/png;base64,AA" }],
  cancelSlice: ["cancel_slice", () => api.cancelSlice(), {}],
  getLayerImage: ["get_layer_image", () => api.getLayerImage(12, 2048), { index: 12, maxSize: 2048 }],
  saveSlice: ["save_slice", () => api.saveSlice("/a.goo"), { path: "/a.goo" }],
  openSlicedFile: ["open_sliced_file", () => api.openSlicedFile("/a.goo"), { path: "/a.goo" }],
  discoverPrinters: ["discover_printers", () => api.discoverPrinters(1234), { timeoutMs: 1234 }],
  savedDevices: ["saved_devices", () => api.savedDevices(), {}],
  saveDevice: [
    "save_device",
    () => api.saveDevice({ ip: "10.0.0.2", name: "Mars", model: "M5U" }),
    { device: { ip: "10.0.0.2", name: "Mars", model: "M5U" } },
  ],
  forgetDevice: ["forget_device", () => api.forgetDevice("10.0.0.2"), { ip: "10.0.0.2" }],
  connectPrinter: ["connect_printer", () => api.connectPrinter("10.0.0.2"), { ip: "10.0.0.2" }],
  disconnectPrinter: ["disconnect_printer", () => api.disconnectPrinter("10.0.0.2"), { ip: "10.0.0.2" }],
  printerFiles: ["printer_files", () => api.printerFiles("10.0.0.2", "/usb"), { ip: "10.0.0.2", dir: "/usb" }],
  printerDelete: ["printer_delete", () => api.printerDelete("10.0.0.2", ["/local/a.goo"]), { ip: "10.0.0.2", files: ["/local/a.goo"] }],
  printerHistory: ["printer_history", () => api.printerHistory("10.0.0.2"), { ip: "10.0.0.2" }],
  printerCommand: [
    "printer_command",
    () => api.printerCommand("10.0.0.2", "start", "/local/a.goo"),
    { ip: "10.0.0.2", command: "start", file: "/local/a.goo" },
  ],
  uploadToPrinter: [
    "upload_to_printer",
    () => api.uploadToPrinter("10.0.0.2", true, "a.goo", "C:\\jobs\\a.goo"),
    { ip: "10.0.0.2", start: true, name: "a.goo", path: "C:\\jobs\\a.goo" },
  ],
  printerSetTimeLapse: ["printer_set_time_lapse", () => api.printerSetTimeLapse("10.0.0.2", true), { ip: "10.0.0.2", enable: true }],
  printerRename: ["printer_rename", () => api.printerRename("10.0.0.2", "Shop"), { ip: "10.0.0.2", name: "Shop" }],
  printerDownload: [
    "printer_download",
    () => api.printerDownload("10.0.0.2", "/local/a.goo", "C:\\a.goo"),
    { ip: "10.0.0.2", path: "/local/a.goo", dest: "C:\\a.goo" },
  ],
  cancelDownload: ["cancel_download", () => api.cancelDownload("10.0.0.2", "/local/a.goo"), { ip: "10.0.0.2", path: "/local/a.goo" }],
  printerThumbnail: ["printer_thumbnail", () => api.printerThumbnail("10.0.0.2", "/local/a.goo"), { ip: "10.0.0.2", path: "/local/a.goo" }],
  cancelUpload: ["cancel_upload", () => api.cancelUpload("10.0.0.2"), { ip: "10.0.0.2" }],
  cameraStart: ["camera_start", () => api.cameraStart("10.0.0.2", "rtsp://cam"), { ip: "10.0.0.2", url: "rtsp://cam" }],
  cameraStop: ["camera_stop", () => api.cameraStop("10.0.0.2"), { ip: "10.0.0.2" }],
  cameraSnapshot: ["camera_snapshot", () => api.cameraSnapshot("10.0.0.2", "C:\\snap.jpg"), { ip: "10.0.0.2", path: "C:\\snap.jpg" }],
};

describe("api wrappers", () => {
  const rust = rustCommands();
  const registered = registeredCommands();

  it.each(Object.entries(CASES))("%s invokes the right command with the right payload", async (_name, [cmd, call, expected]) => {
    await call();
    expect(calls).toHaveLength(1);
    expect(calls[0].cmd).toBe(cmd);
    expect(calls[0].args).toEqual(expected);
  });

  it.each(Object.entries(CASES))("%s matches the Rust signature", async (_name, [cmd, call]) => {
    const r = rust.get(cmd);
    expect(r, `#[tauri::command] fn ${cmd} not found in app/src-tauri/src`).toBeDefined();
    expect(registered, `${cmd} is not registered in generate_handler!`).toContain(cmd);
    await call();
    const sent = Object.keys(calls[0].args).sort();
    // every key must be a parameter (Tauri ignores unknown keys silently) …
    for (const k of sent) expect(r!.params.map((p) => p.jsName), `${cmd}: unknown argument "${k}"`).toContain(k);
    // … and every non-optional parameter must be sent
    for (const p of r!.params.filter((p) => !p.optional)) expect(sent, `${cmd}: missing argument "${p.jsName}"`).toContain(p.jsName);
  });

  it("covers every registered command except the ones the UI does not use", () => {
    const wrapped = new Set(Object.values(CASES).map(([cmd]) => cmd));
    const unused = registered.filter((c) => !wrapped.has(c));
    // pick_surface is implemented in Rust but the viewport picks with three.js
    expect(unused).toEqual(["pick_surface"]);
  });

  it("omits optional arguments that are not given (Rust receives None)", async () => {
    await api.layFlat(3);
    await api.printerFiles("10.0.0.2");
    await api.uploadToPrinter("10.0.0.2", false);
    await api.autoOrient([1]);
    await api.discoverPrinters();
    expect(calls.map((c) => c.args)).toEqual([
      { id: 3 },
      { ip: "10.0.0.2" },
      { ip: "10.0.0.2", start: false },
      { ids: [1] },
      { timeoutMs: 2500 },
    ]);
  });

  it("decodes binary meshes returned by get_mesh", async () => {
    const m = await api.getMesh(1);
    expect(m.positions).toBeInstanceOf(Float32Array);
    expect(m.indices).toBeInstanceOf(Uint32Array);
    expect(m.positions.length).toBe(12);
    expect(m.indices.length).toBe(12);
    expect(Array.from(m.positions.slice(3, 6))).toEqual([10, 0, 0]);
    expect(Array.from(m.indices.slice(0, 3))).toEqual([0, 2, 1]);
  });
});

describe("decodeMesh", () => {
  it("handles an empty mesh", () => {
    const buf = new ArrayBuffer(8);
    const m = decodeMesh(buf);
    expect(m.positions.length).toBe(0);
    expect(m.indices.length).toBe(0);
  });

  it("reads little-endian counts and copies the data", () => {
    const buf = meshBuffer();
    const m = decodeMesh(buf);
    new DataView(buf).setFloat32(8, 99, true);
    expect(m.positions[0]).toBe(0); // independent copy
  });
});

describe("errorText", () => {
  it("passes backend error strings through", () => expect(errorText("object not found")).toBe("object not found"));
  it("uses Error messages", () => expect(errorText(new Error("boom"))).toBe("boom"));
  it("serializes anything else", () => expect(errorText({ code: 3 })).toBe('{"code":3}'));
});

describe("payload types match the Rust structs and enums", () => {
  const keysOk = (struct: string, obj: Record<string, unknown>) => {
    const fields = rustStructFields(struct);
    const names = fields.map((f) => f.name);
    for (const k of Object.keys(obj)) expect(names, `${struct} has no field "${k}"`).toContain(k);
    for (const f of fields.filter((f) => !f.optional)) expect(Object.keys(obj), `${struct}.${f.name} is required`).toContain(f.name);
  };

  it("option objects built in the UI", () => {
    keysOk("OrientOptions", { goal: "balanced" });
    keysOk("HollowOptions", { wall_thickness: 2, voxel_size: 0 });
    keysOk("LabelOptions", { height: 5, depth: 0.8, mode: "emboss" });
    keysOk("BasePlateOptions", { margin: 2, thickness: 1.5, chamfer: 1 });
    keysOk("SavedDevice", { ip: "1.2.3.4", name: "x" });
    keysOk("SavedDevice", { ip: "1.2.3.4", name: "x", model: null });
    keysOk("SupportPoint", { position: [0, 0, 0], normal: [0, 0, 1], size: "medium" });
    keysOk("Transform", { translation: [0, 0, 0], rotation: [0, 0, 0, 1], scale: [1, 1, 1] });
  });

  it("string unions accepted by the backend", () => {
    const goals = { balanced: 1, fewest_supports: 1, lowest_height: 1 } satisfies Record<
      NonNullable<Parameters<typeof api.autoOrient>[1]>["goal"],
      1
    >;
    expect(rustEnumVariants("OrientGoal").sort()).toEqual(Object.keys(goals).sort());

    const modes = { emboss: 1, engrave: 1 } satisfies Record<Parameters<typeof api.addLabel>[2]["mode"], 1>;
    expect(rustEnumVariants("LabelMode").sort()).toEqual(Object.keys(modes).sort());

    const sizes = { light: 1, medium: 1, heavy: 1 } satisfies Record<SupportSize, 1>;
    expect(rustEnumVariants("SupportSize").sort()).toEqual(Object.keys(sizes).sort());

    const align = {
      left: 1,
      right: 1,
      front: 1,
      back: 1,
      center_x: 1,
      center_y: 1,
      plate_center: 1,
      drop: 1,
    } satisfies Record<AlignMode, 1>;
    expect(rustMatchArms("align_objects").sort()).toEqual(Object.keys(align).sort());

    const commands = { pause: 1, resume: 1, stop: 1, start: 1, refresh: 1 } satisfies Record<
      Parameters<typeof api.printerCommand>[1],
      1
    >;
    expect(rustMatchArms("printer_command").sort()).toEqual(Object.keys(commands).sort());

    const shapes = { cube: 1, cylinder: 1, sphere: 1, cone: 1 } satisfies Record<Parameters<typeof api.addPrimitive>[0], 1>;
    expect(rustMatchArms("add_primitive").sort()).toEqual(Object.keys(shapes).sort());

    const axes = { x: 1, y: 1, z: 1 } satisfies Record<Parameters<typeof api.cutObject>[1], 1>;
    expect(rustMatchArms("cut_object").sort()).toEqual(Object.keys(axes).sort());
  });

  it("SupportSettings fields known to the UI exist in Rust", () => {
    const names = rustStructFields("SupportSettings").map((f) => f.name);
    for (const k of [
      "lift_height",
      "spacing",
      "overhang_angle",
      "tip_length",
      "tip_diameter",
      "shaft_diameter",
      "base_diameter",
      "base_height",
      "raft",
      "raft_thickness",
      "raft_margin",
      "model_contacts",
      "detect_islands",
      "island_layer_step",
      "default_size",
      "segments",
      "cross_bracing",
      "bracing_max_distance",
      "bracing_min_height",
      "bracing_diameter",
    ] satisfies (keyof import("../api").SupportSettings)[])
      expect(names).toContain(k);
  });
});

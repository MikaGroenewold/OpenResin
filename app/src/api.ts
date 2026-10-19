// Typed bindings for the Tauri backend commands.
import { invoke } from "@tauri-apps/api/core";

export type Vec3 = [number, number, number];
export type Quat = [number, number, number, number];

export interface Transform {
  translation: Vec3;
  rotation: Quat;
  scale: Vec3;
}

export interface Aabb {
  min: Vec3;
  max: Vec3;
}

export type MirrorMode = "none" | "lcd" | "x" | "y";
export type NetworkProtocol = "sdcp" | "chitu-udp" | "anycubic" | "halot";

export interface PrinterProfile {
  id: string;
  vendor: string;
  name: string;
  resolution_x: number;
  resolution_y: number;
  size_x: number;
  size_y: number;
  size_z: number;
  mirror: MirrorMode;
  formats: string[];
  native_formats?: string[];
  network?: NetworkProtocol;
  machine_type?: string;
}

export interface Motion {
  distance: number;
  speed: number;
  distance2: number;
  speed2: number;
}

export interface LayerParams {
  exposure: number;
  light_off_delay: number;
  wait_before_lift: number;
  wait_after_lift: number;
  wait_after_retract: number;
  lift: Motion;
  retract: Motion;
  light_pwm: number;
}

export interface PrintSettings {
  name: string;
  layer_height: number;
  bottom_layers: number;
  transition_layers: number;
  bottom: LayerParams;
  normal: LayerParams;
  wait_mode: boolean;
  anti_aliasing: number;
  resin_density: number;
  resin_price_per_liter: number;
  currency: string;
}

export type SupportSize = "light" | "medium" | "heavy";

export interface SupportSettings {
  lift_height: number;
  spacing: number;
  overhang_angle: number;
  tip_length: number;
  tip_diameter: [number, number, number];
  shaft_diameter: [number, number, number];
  base_diameter: number;
  base_height: number;
  raft: boolean;
  raft_thickness: number;
  raft_margin: number;
  model_contacts: boolean;
  detect_islands: boolean;
  island_layer_step: number;
  default_size: SupportSize;
  segments: number;
  cross_bracing?: boolean;
  bracing_max_distance?: number;
  bracing_min_height?: number;
  bracing_diameter?: number;
}

export interface SupportPoint {
  position: Vec3;
  normal: Vec3;
  size: SupportSize;
}

export interface ObjectInfo {
  id: number;
  name: string;
  triangles: number;
  transform: Transform;
  bounds: Aabb;
  support_points: number;
  volume_mm3: number;
  mesh_key: number;
  subtract: boolean;
  cutouts: number;
}

export interface HistoryState {
  can_undo: boolean;
  can_redo: boolean;
}

export type AlignMode = "left" | "right" | "front" | "back" | "center_x" | "center_y" | "plate_center" | "drop";

export interface FormatView {
  id: string;
  extension: string;
  description: string;
}

export interface SavedDevice {
  ip: string;
  name: string;
  model?: string | null;
}

export interface ConfigView {
  printer: PrinterProfile;
  settings: PrintSettings;
  support_settings: SupportSettings;
  devices: SavedDevice[];
  theme: string;
  formats: FormatView[];
}

export interface SlicedInfo {
  format: string;
  machine_name: string;
  resolution_x: number;
  resolution_y: number;
  size_x: number;
  size_y: number;
  layer_count: number;
  layer_height: number;
  exposure: number;
  bottom_exposure: number;
  bottom_layers: number;
  print_time_s: number;
  volume_ml: number;
  mirror_x: boolean;
  mirror_y: boolean;
  anti_aliasing: number;
}

export interface SliceSummary {
  format: string;
  extension: string;
  layer_count: number;
  print_time_s: number;
  volume_ml: number;
  weight_g: number;
  cost: number;
  currency: string;
  height_mm: number;
  file_size: number;
  duration_ms: number;
  info: SlicedInfo;
  layer_areas: number[];
  suggested_name: string;
  warnings: string[];
}

export interface DiscoveredPrinter {
  ip: string;
  protocol: "sdcp" | "chitu-udp";
  name: string;
  model?: string | null;
  brand?: string | null;
  id?: string | null;
  mainboard_id?: string | null;
  protocol_version?: string | null;
  firmware_version?: string | null;
}

export type MachineState =
  | "unknown"
  | "offline"
  | "idle"
  | "printing"
  | "pausing"
  | "paused"
  | "stopping"
  | "stopped"
  | "completed"
  | "file_transfer"
  | "homing"
  | "file_checking"
  | "exposure_test"
  | "self_test"
  | "error";

export interface PrinterStatus {
  state: MachineState;
  detail?: string | null;
  filename?: string | null;
  current_layer?: number | null;
  total_layers?: number | null;
  elapsed_s?: number | null;
  total_s?: number | null;
  uv_led_temp?: number | null;
  box_temp?: number | null;
  error_code?: number | null;
  free_storage?: number | null;
  camera_connected?: boolean | null;
  time_lapse?: boolean | null;
}

export interface DeviceStatusEvent {
  ip: string;
  status: PrinterStatus;
  attributes?: Record<string, unknown> | null;
  connected: boolean;
  protocol?: "sdcp" | "chitu-udp" | null;
  camera?: boolean;
  downloads?: boolean;
}

export interface DownloadProgress {
  ip: string;
  path: string;
  done: number;
  total: number;
}

export interface UploadProgress {
  ip: string;
  sent: number;
  total: number;
  done: boolean;
  error?: string | null;
}

export interface FileEntry {
  path: string;
  is_dir: boolean;
  size?: number | null;
}

export interface CameraUrls {
  stream_url: string;
  snapshot_url: string;
  source: string;
}

export interface MeshData {
  positions: Float32Array;
  indices: Uint32Array;
}

export function decodeMesh(buf: ArrayBuffer): MeshData {
  const dv = new DataView(buf);
  const v = dv.getUint32(0, true);
  const t = dv.getUint32(4, true);
  return {
    positions: new Float32Array(buf.slice(8, 8 + v * 12)),
    indices: new Uint32Array(buf.slice(8 + v * 12, 8 + v * 12 + t * 12)),
  };
}

export const api = {
  startupFiles: () => invoke<string[]>("startup_files"),
  startupFlags: () => invoke<string[]>("startup_flags"),
  listPrinters: () => invoke<PrinterProfile[]>("list_printers"),
  getConfig: () => invoke<ConfigView>("get_config"),
  setPrinter: (id: string) => invoke<ConfigView>("set_printer", { id }),
  setSettings: (settings: PrintSettings) => invoke<void>("set_settings", { settings }),
  setSupportSettings: (settings: SupportSettings) => invoke<void>("set_support_settings", { settings }),
  saveCustomPrinter: (printer: PrinterProfile) => invoke<ConfigView>("save_custom_printer", { printer }),
  setTheme: (theme: string) => invoke<void>("set_theme", { theme }),

  importModels: (paths: string[]) => invoke<ObjectInfo[]>("import_models", { paths }),
  sceneObjects: () => invoke<ObjectInfo[]>("scene_objects"),
  getMesh: async (id: number) => decodeMesh(await invoke<ArrayBuffer>("get_mesh", { id })),
  setTransform: (id: number, transform: Transform, keepOnPlate: boolean) =>
    invoke<ObjectInfo>("set_transform", { id, transform, keepOnPlate }),
  removeObjects: (ids: number[]) => invoke<void>("remove_objects", { ids }),
  duplicateObject: (id: number, count: number) => invoke<ObjectInfo[]>("duplicate_object", { id, count }),
  arrange: () => invoke<[boolean, ObjectInfo[]]>("arrange"),
  layFlat: (id: number, face?: number) => invoke<ObjectInfo>("lay_flat", { id, face }),

  listPresets: () => invoke<{ settings: PrintSettings; builtin: boolean }[]>("list_presets"),
  savePreset: (settings: PrintSettings) => invoke<void>("save_preset", { settings }),
  deletePreset: (name: string) => invoke<void>("delete_preset", { name }),
  analyzeObject: (id: number) => invoke<unknown>("analyze_object", { id }),
  repairObjects: (ids: number[], options?: unknown) =>
    invoke<[{ id: number; name: string; report: unknown }[], ObjectInfo[]]>("repair_objects", { ids, options }),
  splitObject: (id: number) => invoke<[number, ObjectInfo[]]>("split_object", { id }),
  cutObject: (id: number, axis: "x" | "y" | "z", position: number) =>
    invoke<ObjectInfo[]>("cut_object", { id, axis, position }),
  setSubtract: (ids: number[], subtract: boolean) => invoke<ObjectInfo[]>("set_subtract", { ids, subtract }),
  hollowObject: (id: number, options: { wall_thickness: number; voxel_size: number }) =>
    invoke<[number, ObjectInfo]>("hollow_object", { id, options }),
  addDrainHole: (id: number, position: Vec3, normal: Vec3, diameter: number, depth: number) =>
    invoke<ObjectInfo>("add_drain_hole", { id, position, normal, diameter, depth }),
  clearCutouts: (id: number) => invoke<ObjectInfo>("clear_cutouts", { id }),
  getCutoutMesh: async (id: number) => decodeMesh(await invoke<ArrayBuffer>("get_cutout_mesh", { id })),
  addPrimitive: (kind: "cube" | "cylinder" | "sphere" | "cone", size?: number) =>
    invoke<ObjectInfo[]>("add_primitive", { kind, size }),
  addLabel: (
    id: number,
    text: string,
    options: { height: number; depth: number; mode: "emboss" | "engrave" },
    position: Vec3,
    normal: Vec3,
  ) => invoke<ObjectInfo>("add_label", { id, text, options, position, normal }),
  addPlatform: (id: number, options: { margin: number; thickness: number; chamfer: number }) =>
    invoke<ObjectInfo>("add_platform", { id, options }),
  historyState: () => invoke<HistoryState>("history_state"),
  undo: () => invoke<ObjectInfo[]>("undo"),
  redo: () => invoke<ObjectInfo[]>("redo"),
  fillPlate: (id: number, spacing?: number) => invoke<[number, ObjectInfo[]]>("fill_plate", { id, spacing }),
  alignObjects: (ids: number[], mode: AlignMode) => invoke<ObjectInfo[]>("align_objects", { ids, mode }),
  combineObjects: (ids: number[]) => invoke<ObjectInfo[]>("combine_objects", { ids }),
  autoOrient: (ids: number[], options?: { goal: "balanced" | "fewest_supports" | "lowest_height" }) =>
    invoke<ObjectInfo[]>("auto_orient", { ids, options }),
  renameObject: (id: number, name: string) => invoke<ObjectInfo>("rename_object", { id, name }),

  autoSupports: (ids: number[], settings: SupportSettings) =>
    invoke<ObjectInfo[]>("auto_supports", { ids, settings }),
  getSupportPoints: (id: number) => invoke<SupportPoint[]>("get_support_points", { id }),
  setSupportPoints: (id: number, points: SupportPoint[]) =>
    invoke<ObjectInfo>("set_support_points", { id, points }),
  clearSupports: (id: number) => invoke<ObjectInfo>("clear_supports", { id }),
  getSupportMesh: async (id: number) => decodeMesh(await invoke<ArrayBuffer>("get_support_mesh", { id })),

  saveProject: (path: string) => invoke<void>("save_project", { path }),
  openProject: (path: string) => invoke<ObjectInfo[]>("open_project", { path }),
  newProject: () => invoke<void>("new_project"),
  exportStl: (path: string, withSupports: boolean) => invoke<void>("export_stl", { path, withSupports }),

  slice: (format: string | null, thumbnail: string | null) => invoke<SliceSummary>("slice", { format, thumbnail }),
  cancelSlice: () => invoke<void>("cancel_slice"),
  getLayerImage: (index: number, maxSize: number) => invoke<ArrayBuffer>("get_layer_image", { index, maxSize }),
  saveSlice: (path: string) => invoke<void>("save_slice", { path }),
  openSlicedFile: (path: string) => invoke<SliceSummary>("open_sliced_file", { path }),

  discoverPrinters: (timeoutMs = 2500) => invoke<DiscoveredPrinter[]>("discover_printers", { timeoutMs }),
  savedDevices: () => invoke<SavedDevice[]>("saved_devices"),
  saveDevice: (device: SavedDevice) => invoke<SavedDevice[]>("save_device", { device }),
  forgetDevice: (ip: string) => invoke<SavedDevice[]>("forget_device", { ip }),
  connectPrinter: (ip: string) => invoke<DeviceStatusEvent>("connect_printer", { ip }),
  disconnectPrinter: (ip: string) => invoke<void>("disconnect_printer", { ip }),
  printerFiles: (ip: string, dir?: string) => invoke<FileEntry[]>("printer_files", { ip, dir }),
  printerDelete: (ip: string, files: string[]) => invoke<string[]>("printer_delete", { ip, files }),
  printerHistory: (ip: string) => invoke<unknown>("printer_history", { ip }),
  printerCommand: (ip: string, command: "pause" | "resume" | "stop" | "start" | "refresh", file?: string) =>
    invoke<void>("printer_command", { ip, command, file }),
  uploadToPrinter: (ip: string, start: boolean, name?: string, path?: string) =>
    invoke<string>("upload_to_printer", { ip, start, name, path }),
  printerSetTimeLapse: (ip: string, enable: boolean) => invoke<void>("printer_set_time_lapse", { ip, enable }),
  printerRename: (ip: string, name: string) => invoke<void>("printer_rename", { ip, name }),
  printerDownload: (ip: string, path: string, dest: string) => invoke<number>("printer_download", { ip, path, dest }),
  cancelDownload: (ip: string, path: string) => invoke<void>("cancel_download", { ip, path }),
  printerThumbnail: (ip: string, path: string) => invoke<ArrayBuffer>("printer_thumbnail", { ip, path }),
  cancelUpload: (ip: string) => invoke<void>("cancel_upload", { ip }),
  cameraStart: (ip: string, url?: string) => invoke<CameraUrls>("camera_start", { ip, url }),
  cameraStop: (ip: string) => invoke<void>("camera_stop", { ip }),
  cameraSnapshot: (ip: string, path: string) => invoke<void>("camera_snapshot", { ip, path }),
};

export function errorText(e: unknown): string {
  if (typeof e === "string") return e;
  if (e instanceof Error) return e.message;
  return JSON.stringify(e);
}

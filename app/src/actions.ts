// User actions shared by several components.
import { open, save } from "@tauri-apps/plugin-dialog";
import { api, errorText, type ObjectInfo, type SupportPoint, type Vec3 } from "./api";
import { useStore, savedToEntry } from "./store";
import type { SceneView } from "./viewport/SceneView";
import { markLoaded, refreshHistory, syncScene } from "./editActions";
import { confirmDialog } from "./ui";
import { checkImported } from "./views/GeometryPanels";
import { t as tr } from "./i18n";

export const MODEL_EXTENSIONS = ["stl", "obj", "3mf"];
const BASE_SLICED = ["goo", "ctb", "sl1", "zip"];
/** All printer file extensions the backend can read. */
const slicedExtensions = () => {
  const list = useStore.getState().config?.formats.map((f) => f.extension) ?? [];
  return list.length ? [...new Set(list)] : BASE_SLICED;
};

/** The live scene view (needed for thumbnails); set by the Viewport component. */
export const sceneRef: { current: SceneView | null } = { current: null };

const st = () => useStore.getState();

export async function run<T>(label: string | null, f: () => Promise<T>): Promise<T | undefined> {
  if (label) st().setBusy(label);
  try {
    return await f();
  } catch (e) {
    st().toast("error", errorText(e));
    return undefined;
  } finally {
    if (label) st().setBusy(null);
  }
}

export async function loadMeshes(objs: ObjectInfo[]) {
  for (const o of objs) {
    const mesh = await api.getMesh(o.id);
    sceneRef.current?.setObjectGeometry(o.id, mesh);
    sceneRef.current?.setObjectTransform(o.id, o.transform);
    markLoaded(o);
  }
}

export async function importPaths(paths: string[]) {
  const models = paths.filter((p) => MODEL_EXTENSIONS.includes(p.split(".").pop()?.toLowerCase() ?? ""));
  const projects = paths.filter((p) => p.toLowerCase().endsWith(".orproj"));
  const sliced = paths.filter((p) => slicedExtensions().includes(p.split(".").pop()?.toLowerCase() ?? ""));
  if (projects.length) await openProject(projects[0]);
  if (models.length) {
    await run(tr("Loading models…"), async () => {
      const objs = await api.importModels(models);
      // arrangement may have moved existing objects too
      await syncScene();
      st().select(objs.map((o) => o.id));
      void checkImported(objs.map((o) => o.id));
      st().setMode("model");
      st().toast("success", tr(objs.length === 1 ? "Loaded {n} model" : "Loaded {n} models", { n: objs.length }));
    });
  }
  if (sliced.length && !models.length && !projects.length) await openSliced(sliced[0]);
}

export async function openModelsDialog() {
  const sel = await open({
    multiple: true,
    filters: [
      { name: tr("3D models"), extensions: MODEL_EXTENSIONS },
      { name: tr("OpenResin project"), extensions: ["orproj"] },
      { name: tr("Sliced files"), extensions: slicedExtensions() },
    ],
  });
  if (!sel) return;
  await importPaths(Array.isArray(sel) ? sel : [sel]);
}

export async function openProject(path?: string) {
  const p =
    path ??
    (await open({ multiple: false, filters: [{ name: tr("OpenResin project"), extensions: ["orproj"] }] }));
  if (!p || Array.isArray(p)) return;
  await run(tr("Opening project…"), async () => {
    const objs = await api.openProject(p);
    st().setConfig(await api.getConfig());
    await syncScene(objs);
    st().setMode("model");
  });
}

export async function saveProject() {
  const p = await save({ filters: [{ name: tr("OpenResin project"), extensions: ["orproj"] }], defaultPath: "plate.orproj" });
  if (!p) return;
  await run(tr("Saving project…"), async () => {
    await api.saveProject(p);
    st().toast("success", tr("Project saved"));
  });
}

export async function exportStl() {
  const p = await save({ filters: [{ name: "STL", extensions: ["stl"] }], defaultPath: "plate.stl" });
  if (!p) return;
  await run(tr("Exporting STL…"), () => api.exportStl(p, true));
}

export async function newProject() {
  if (st().objects.length && !(await confirmDialog(tr("Remove all models from the plate?")))) return;
  await api.newProject();
  await syncScene([]);
  st().setSlice(null);
}

export async function openSliced(path?: string) {
  const p =
    path ?? (await open({ multiple: false, filters: [{ name: tr("Sliced files"), extensions: slicedExtensions() }] }));
  if (!p || Array.isArray(p)) return;
  await run(tr("Opening file…"), async () => {
    const s = await api.openSlicedFile(p);
    st().setSlice(s);
    st().setMode("preview");
  });
}

export async function deleteSelection() {
  const ids = st().selection;
  if (!ids.length) return;
  await api.removeObjects(ids);
  for (const id of ids) sceneRef.current?.removeObject(id);
  st().removeObjects(ids);
  await refreshHistory();
}

export async function refreshCutouts(id: number) {
  const o = st().objects.find((x) => x.id === id);
  if (!o || o.cutouts === 0) {
    sceneRef.current?.setCutouts(id, null);
    return;
  }
  sceneRef.current?.setCutouts(id, await api.getCutoutMesh(id));
}

export async function refreshSupports(id: number) {
  const [pts, mesh] = await Promise.all([api.getSupportPoints(id), api.getSupportMesh(id)]);
  st().setSupportPoints(id, pts);
  sceneRef.current?.setSupports(id, pts.length ? mesh : null);
  st().bumpSupports(id);
}

export function applyObject(o: ObjectInfo) {
  st().upsertObjects([o]);
  sceneRef.current?.setObjectTransform(o.id, o.transform);
  void refreshHistory();
}

export async function autoSupports(ids: number[], opts: { magic?: boolean } = {}) {
  const cfg = st().config;
  if (!cfg || !ids.length) return;
  // "magic" = denser contacts, island detection and supports on the model
  const settings = opts.magic
    ? {
        ...cfg.support_settings,
        spacing: Math.max(1, cfg.support_settings.spacing * 0.75),
        overhang_angle: Math.min(60, cfg.support_settings.overhang_angle + 10),
        detect_islands: true,
        model_contacts: true,
      }
    : cfg.support_settings;
  await run(tr(opts.magic ? "Placing magic supports…" : "Generating supports…"), async () => {
    const objs = await api.autoSupports(ids, settings);
    for (const o of objs) {
      applyObject(o);
      await refreshSupports(o.id);
    }
    const n = objs.reduce((a, o) => a + o.support_points, 0);
    st().toast("success", tr(n === 1 ? "Added {n} support" : "Added {n} supports", { n }));
    await refreshHistory();
  });
}

export async function clearSupports(ids: number[]) {
  for (const id of ids) {
    const o = await api.clearSupports(id);
    applyObject(o);
    await refreshSupports(id);
  }
}

export async function addSupportAt(id: number, position: Vec3, normal: Vec3) {
  const cfg = st().config;
  const pts: SupportPoint[] = [...(st().supportPoints[id] ?? [])];
  pts.push({ position, normal, size: cfg?.support_settings.default_size ?? "medium" });
  const o = await api.setSupportPoints(id, pts);
  applyObject(o);
  await refreshSupports(id);
}

export async function removeSupportNear(id: number, point: Vec3) {
  const pts = st().supportPoints[id] ?? [];
  if (!pts.length) return;
  // closest contact in XY (supports are vertical below the contact)
  let best = -1;
  let bestD = Infinity;
  pts.forEach((p, i) => {
    const d = Math.hypot(p.position[0] - point[0], p.position[1] - point[1]) + Math.max(0, point[2] - p.position[2]) * 0.05;
    if (d < bestD) {
      bestD = d;
      best = i;
    }
  });
  if (best < 0 || bestD > 4) return;
  const next = pts.filter((_, i) => i !== best);
  const o = await api.setSupportPoints(id, next);
  applyObject(o);
  await refreshSupports(id);
}

/** Slices the plate; resolves to true when a new slice result is available. */
export async function sliceNow(format: string | null): Promise<boolean> {
  const s = st();
  if (!s.objects.length) {
    s.toast("info", tr("Add a model to the plate first"));
    return false;
  }
  const thumb = sceneRef.current?.captureThumbnail(400) ?? null;
  s.setSliceProgress({ done: 0, total: 1 });
  try {
    const res = await api.slice(format, thumb);
    st().setSlice(res);
    st().setMode("preview");
    for (const w of res.warnings) st().toast("info", w);
    return true;
  } catch (e) {
    const msg = errorText(e);
    if (msg !== "cancelled") st().toast("error", msg);
    return false;
  } finally {
    st().setSliceProgress(null);
  }
}

export async function saveSliced() {
  const s = st().slice;
  if (!s) return;
  const p = await save({
    defaultPath: s.suggested_name,
    filters: [{ name: s.extension.toUpperCase(), extensions: [s.extension] }],
  });
  if (!p) return;
  await run(tr("Saving…"), async () => {
    await api.saveSlice(p);
    st().toast("success", tr("File saved"));
  });
}

export async function loadSavedDevices() {
  const list = await api.savedDevices();
  for (const d of list) st().updateDevice(d.ip, savedToEntry(d));
}

export async function connectDevice(ip: string) {
  const s = st();
  s.updateDevice(ip, { connecting: true, error: null });
  try {
    const ev = await api.connectPrinter(ip);
    st().updateDevice(ip, { connected: true, connecting: false, status: ev });
    return true;
  } catch (e) {
    st().updateDevice(ip, { connected: false, connecting: false, error: errorText(e) });
    return false;
  }
}

export async function discover() {
  await run(tr("Searching the network…"), async () => {
    const found = await api.discoverPrinters(3000);
    for (const p of found) {
      st().updateDevice(p.ip, { discovered: p, name: st().devices[p.ip]?.saved ? st().devices[p.ip].name : p.name, model: p.model });
    }
    st().toast(
      found.length ? "success" : "info",
      found.length ? tr(found.length === 1 ? "Found {n} printer" : "Found {n} printers", { n: found.length }) : tr("No printers answered"),
    );
  });
}

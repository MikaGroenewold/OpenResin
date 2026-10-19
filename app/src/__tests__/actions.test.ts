// End-to-end flows of the plate/printer actions through the real invoke path
// against the fake backend.
import { beforeEach, describe, expect, it } from "vitest";
import { api } from "../api";
import {
  addSupportAt,
  autoSupports,
  clearSupports,
  connectDevice,
  deleteSelection,
  discover,
  exportStl,
  importPaths,
  loadSavedDevices,
  newProject,
  openModelsDialog,
  openProject,
  openSliced,
  removeSupportNear,
  run,
  saveProject,
  saveSliced,
  sceneRef,
  sliceNow,
} from "../actions";
import { align, arrangeAll, autoOrient, combine, copies, fillPlate, mirror, moveObjects, nudge, redo, syncScene, undo } from "../editActions";
import { useStore } from "../store";
import type { SceneView } from "../viewport/SceneView";
import { FakeSceneView } from "./fakeScene";
import { backend, settle } from "./harness";

const st = () => useStore.getState();
const toasts = () => st().toasts.map((t) => `${t.kind}: ${t.text}`);
let view: FakeSceneView;

async function boot() {
  view = new FakeSceneView();
  sceneRef.current = view as unknown as SceneView;
  st().setConfig(await api.getConfig());
  st().setPrinters(await api.listPrinters());
}

/** Imports `names` as STL files and returns their ids. */
async function plate(...names: string[]) {
  await importPaths(names.map((n) => `C:\\models\\${n}.stl`));
  return st().objects.map((o) => o.id);
}

beforeEach(async () => {
  await boot();
});

describe("import", () => {
  it("loads models into the store, the 3D view and selects them", async () => {
    await importPaths(["C:\\models\\benchy.stl", "D:/prints/Part.OBJ", "/home/me/notes.txt"]);
    expect(backend.lastCall("import_models")).toEqual({ paths: ["C:\\models\\benchy.stl", "D:/prints/Part.OBJ"] });
    expect(st().objects.map((o) => o.name)).toEqual(["benchy", "Part"]);
    const ids = st().objects.map((o) => o.id);
    expect(st().selection).toEqual(ids);
    expect(st().mode).toBe("model");
    for (const id of ids) {
      expect(view.hasObject(id)).toBe(true);
      expect(view.objects.get(id)?.transform).toEqual(st().objects.find((o) => o.id === id)?.transform);
    }
    expect(toasts()).toContain("success: Loaded 2 models");
    expect(st().history.can_undo).toBe(true);
    expect(st().busy).toBeNull();
    await settle();
    // every imported model is checked for mesh errors
    expect(backend.callsOf("analyze_object")).toEqual(ids.map((id) => ({ id })));
  });

  it("arranges new models next to existing ones without overlap", async () => {
    await plate("a");
    await plate("b", "c");
    const b = st().objects.map((o) => o.bounds);
    for (let i = 0; i < b.length; i++)
      for (let j = i + 1; j < b.length; j++) {
        const overlapX = b[i].min[0] < b[j].max[0] && b[j].min[0] < b[i].max[0];
        const overlapY = b[i].min[1] < b[j].max[1] && b[j].min[1] < b[i].max[1];
        expect(overlapX && overlapY).toBe(false);
      }
  });

  it("warns about models with mesh errors", async () => {
    backend.reports[1] = { ...(await import("./fakeBackend")).cleanReport(), holes: 3, watertight: false };
    await plate("broken");
    await settle();
    expect(toasts()).toContain("info: broken: 3 mesh issues found – use Repair › Model Repair");
  });

  it("ignores unsupported files", async () => {
    await importPaths(["C:\\docs\\readme.txt"]);
    expect(backend.callsOf("import_models")).toEqual([]);
    expect(st().objects).toEqual([]);
  });

  it("opens projects and sliced files by extension", async () => {
    await plate("cube");
    await saveProjectTo("C:\\p\\plate.orproj");
    backend.objects = [];
    await importPaths(["C:\\p\\plate.orproj"]);
    expect(backend.lastCall("open_project")).toEqual({ path: "C:\\p\\plate.orproj" });
    expect(st().objects.map((o) => o.name)).toEqual(["cube"]);

    await importPaths(["C:\\jobs\\ring.goo"]);
    expect(backend.lastCall("open_sliced_file")).toEqual({ path: "C:\\jobs\\ring.goo" });
    expect(st().mode).toBe("preview");
    expect(st().slice?.suggested_name).toBe("ring.goo");
  });

  it("uses the file dialog with model, project and sliced-file filters", async () => {
    backend.openResults.push(["C:\\a.stl", "C:\\b.3mf"]);
    await openModelsDialog();
    const opts = backend.lastCall("plugin:dialog|open")!.options as { multiple: boolean; filters: { name: string; extensions: string[] }[] };
    expect(opts.multiple).toBe(true);
    expect(opts.filters).toEqual([
      { name: "3D models", extensions: ["stl", "obj", "3mf"] },
      { name: "OpenResin project", extensions: ["orproj"] },
      { name: "Sliced files", extensions: ["goo", "ctb", "sl1", "zip"] },
    ]);
    expect(backend.lastCall("import_models")).toEqual({ paths: ["C:\\a.stl", "C:\\b.3mf"] });
  });

  it("does nothing when the dialog is cancelled", async () => {
    await openModelsDialog();
    expect(backend.callsOf("import_models")).toEqual([]);
  });

  it("shows backend errors as a toast and clears the busy state", async () => {
    backend.fail("import_models", "could not read benchy.stl: unexpected end of file");
    await importPaths(["C:\\benchy.stl"]);
    expect(toasts()).toEqual(["error: could not read benchy.stl: unexpected end of file"]);
    expect(st().busy).toBeNull();
    expect(st().objects).toEqual([]);
  });

  it("shows the busy label while loading", async () => {
    const release = backend.hold("import_models");
    const p = importPaths(["C:\\slow.stl"]);
    await settle(1);
    expect(st().busy).toBe("Loading models…");
    release();
    await p;
    expect(st().busy).toBeNull();
  });
});

async function saveProjectTo(path: string) {
  backend.saveResults.push(path);
  await saveProject();
}

describe("selection editing", () => {
  it("nudges the selected models only", async () => {
    const [a, b] = await plate("a", "b");
    const before = st().objects.map((o) => [...o.transform.translation]);
    st().select([a]);
    await nudge(5, -2);
    expect(backend.callsOf("set_transform")).toHaveLength(1);
    const call = backend.lastCall("set_transform")!;
    expect(call.id).toBe(a);
    expect(call.keepOnPlate).toBe(false);
    expect((call.transform as { translation: number[] }).translation).toEqual([before[0][0] + 5, before[0][1] - 2, before[0][2]]);
    expect(st().objects[0].transform.translation).toEqual([before[0][0] + 5, before[0][1] - 2, before[0][2]]);
    expect(st().objects[1].transform.translation).toEqual(before[1]);
    expect(view.objects.get(a)?.transform?.translation).toEqual([before[0][0] + 5, before[0][1] - 2, before[0][2]]);
    expect(view.objects.get(b)?.transform?.translation).toEqual(before[1]);
  });

  it("does not call the backend without a selection", async () => {
    await plate("a");
    st().select([]);
    await nudge(1, 0);
    expect(backend.callsOf("set_transform")).toEqual([]);
  });

  it("moves several models in one go (drag in the viewport)", async () => {
    const [a, b] = await plate("a", "b");
    const objs = st().objects;
    await moveObjects([
      { id: a, transform: { ...objs[0].transform, translation: [10, 10, 0] } },
      { id: b, transform: { ...objs[1].transform, translation: [-10, -10, 0] } },
    ]);
    expect(st().objects.map((o) => o.transform.translation)).toEqual([
      [10, 10, 0],
      [-10, -10, 0],
    ]);
    expect(st().history.can_undo).toBe(true);
  });

  it("mirrors in X and keeps the model on the plate", async () => {
    const [a] = await plate("a");
    st().select([a]);
    await mirror(0);
    expect(backend.lastCall("set_transform")).toMatchObject({ id: a, keepOnPlate: true, transform: { scale: [-1, 1, 1] } });
    expect(st().objects[0].transform.scale).toEqual([-1, 1, 1]);
    await mirror(0);
    expect(st().objects[0].transform.scale).toEqual([1, 1, 1]);
  });

  it("keeps supports when mirroring in X/Y but removes them when mirroring in Z", async () => {
    const [a] = await plate("a");
    st().select([a]);
    await autoSupports([a]);
    const n = st().objects[0].support_points;
    expect(n).toBeGreaterThan(0);
    await mirror(1);
    expect(st().objects[0].support_points).toBe(n);
    expect(view.objects.get(a)?.supports).not.toBeNull();
    await mirror(2);
    expect(st().objects[0].support_points).toBe(0);
    expect(view.objects.get(a)?.supports).toBeNull();
  });

  it("deletes the selection", async () => {
    const [a, b] = await plate("a", "b");
    st().select([a]);
    await deleteSelection();
    expect(backend.lastCall("remove_objects")).toEqual({ ids: [a] });
    expect(st().objects.map((o) => o.id)).toEqual([b]);
    expect(st().selection).toEqual([]);
    expect(view.hasObject(a)).toBe(false);
  });
});

describe("copies, fill plate, arrange, align, combine, orient", () => {
  it("copies the selected model", async () => {
    const [a] = await plate("a");
    st().select([a]);
    await copies(3);
    expect(backend.lastCall("duplicate_object")).toEqual({ id: a, count: 3 });
    expect(st().objects).toHaveLength(4);
    expect(st().objects.every((o) => o.name === "a")).toBe(true);
    for (const o of st().objects) expect(view.hasObject(o.id)).toBe(true);
  });

  it("asks for exactly one model before copying", async () => {
    const [a, b] = await plate("a", "b");
    st().select([a, b]);
    await copies(2);
    expect(backend.callsOf("duplicate_object")).toEqual([]);
    expect(toasts()).toContain("info: Select one model to copy");
  });

  it("fills the plate and reports the number of copies", async () => {
    const [a] = await plate("a");
    st().select([a]);
    await fillPlate(2);
    const [, args] = [null, backend.lastCall("fill_plate")];
    expect(args).toEqual({ id: a, spacing: 2 });
    // 153.4 × 77.8 mm plate, 20 mm parts with 2 mm gaps: 7 × 3 = 21 parts
    expect(st().objects).toHaveLength(21);
    expect(toasts()).toContain("success: Added 20 copies");
    await fillPlate(2);
    expect(toasts()).toContain("success: No room for more copies");
  });

  it("asks for a selection before filling the plate", async () => {
    await plate("a");
    st().select([]);
    await fillPlate(3);
    expect(backend.callsOf("fill_plate")).toEqual([]);
    expect(toasts()).toContain("info: Select one model to fill the plate with");
  });

  it("arranges everything and warns when it does not fit", async () => {
    backend.modelSizes.huge = [120, 70, 10];
    await plate("huge", "huge");
    await arrangeAll();
    expect(backend.callsOf("arrange")).toHaveLength(1);
    expect(toasts()).toContain("error: Not everything fits on the plate");
  });

  it("aligns all models when nothing is selected", async () => {
    const ids = await plate("a", "b", "c");
    st().select([]);
    await align("left");
    expect(backend.lastCall("align_objects")).toEqual({ ids, mode: "left" });
    const mins = st().objects.map((o) => o.bounds.min[0]);
    expect(new Set(mins.map((v) => v.toFixed(3))).size).toBe(1);
  });

  it("aligns only the selection", async () => {
    const [a, b] = await plate("a", "b", "c");
    st().select([a, b]);
    await align("plate_center");
    expect(backend.lastCall("align_objects")).toEqual({ ids: [a, b], mode: "plate_center" });
  });

  it("combines the selected models into one", async () => {
    const [a, b, c] = await plate("a", "b", "c");
    st().select([a, b]);
    await combine();
    expect(backend.lastCall("combine_objects")).toEqual({ ids: [a, b] });
    expect(st().objects.map((o) => o.name)).toEqual(["c", "a (combined)"]);
    expect(st().selection).toEqual([]);
    expect(view.hasObject(a)).toBe(false);
    expect(view.hasObject(b)).toBe(false);
    expect(view.hasObject(c)).toBe(true);
  });

  it("needs two models to combine", async () => {
    const [a] = await plate("a", "b");
    st().select([a]);
    await combine();
    expect(backend.callsOf("combine_objects")).toEqual([]);
    expect(toasts()).toContain("info: Select at least two models (Ctrl+click)");
  });

  it("auto-orients with the chosen goal", async () => {
    const [a, b] = await plate("a", "b");
    st().select([b]);
    await autoOrient("fewest_supports");
    expect(backend.lastCall("auto_orient")).toEqual({ ids: [b], options: { goal: "fewest_supports" } });
    expect(st().objects.find((o) => o.id === b)?.transform.rotation).toEqual([Math.SQRT1_2, 0, 0, Math.SQRT1_2]);
    expect(st().objects.find((o) => o.id === a)?.transform.rotation).toEqual([0, 0, 0, 1]);
    expect(toasts()).toContain("success: Oriented 1 model");
    st().select([]);
    await autoOrient();
    expect(backend.lastCall("auto_orient")).toEqual({ ids: [a, b], options: { goal: "balanced" } });
    expect(toasts()).toContain("success: Oriented 2 models");
  });
});

describe("undo / redo", () => {
  it("restores the previous plate and redoes it", async () => {
    const [a] = await plate("a");
    st().select([a]);
    const before = [...st().objects[0].transform.translation];
    await nudge(10, 0);
    expect(st().history).toEqual({ can_undo: true, can_redo: false });
    await undo();
    expect(st().objects[0].transform.translation).toEqual(before);
    expect(view.objects.get(a)?.transform?.translation).toEqual(before);
    expect(st().history.can_redo).toBe(true);
    await redo();
    expect(st().objects[0].transform.translation).toEqual([before[0] + 10, before[1], before[2]]);
    expect(st().history.can_redo).toBe(false);
  });

  it("undoing an import removes the model from the view", async () => {
    const [a] = await plate("a");
    await undo();
    expect(st().objects).toEqual([]);
    expect(view.hasObject(a)).toBe(false);
  });

  it("reports when there is nothing to undo", async () => {
    await undo();
    expect(toasts()).toEqual(["error: nothing to undo"]);
  });
});

describe("supports", () => {
  it("generates supports with the configured settings", async () => {
    const [a] = await plate("a");
    await autoSupports([a]);
    const call = backend.lastCall("auto_supports")!;
    expect(call).toEqual({ ids: [a], settings: st().config!.support_settings });
    const o = st().objects[0];
    expect(o.support_points).toBe(44); // 20×20 mm at 3 mm spacing
    expect(o.bounds.min[2]).toBe(5); // lifted by lift_height
    expect(st().supportPoints[a]).toHaveLength(44);
    expect(st().supportVersion[a]).toBe(1);
    expect(view.objects.get(a)?.supports).not.toBeNull();
    expect(toasts()).toContain("success: Added 44 supports");
  });

  it("magic supports use denser contacts, islands and model contacts", async () => {
    const [a] = await plate("a");
    await autoSupports([a], { magic: true });
    const s = backend.lastCall("auto_supports")!.settings as Record<string, unknown>;
    expect(s).toMatchObject({ spacing: 2.25, overhang_angle: 55, detect_islands: true, model_contacts: true });
  });

  it("magic supports leave the saved support settings untouched", async () => {
    // Magic Support passes derived settings (spacing × 0.75, angle + 10); they must not
    // become the user's defaults, or every further click would compound them.
    const [a] = await plate("a");
    const saved = structuredClone(st().config!.support_settings);
    await autoSupports([a], { magic: true });
    await autoSupports([a], { magic: true });
    expect((await api.getConfig()).support_settings).toEqual(saved);
    expect(backend.lastCall("auto_supports")!.settings).toMatchObject({ spacing: 2.25, overhang_angle: 55 });
  });

  it("does nothing without objects or config", async () => {
    await autoSupports([]);
    st().setConfig(null as never);
    await autoSupports([1]);
    expect(backend.callsOf("auto_supports")).toEqual([]);
  });

  it("clears supports", async () => {
    const [a] = await plate("a");
    await autoSupports([a]);
    await clearSupports([a]);
    expect(backend.lastCall("clear_supports")).toEqual({ id: a });
    expect(st().objects[0].support_points).toBe(0);
    expect(st().supportPoints[a]).toEqual([]);
    expect(view.objects.get(a)?.supports).toBeNull();
  });

  it("adds a manual support with the default size", async () => {
    const [a] = await plate("a");
    st().setConfig({ ...st().config!, support_settings: { ...st().config!.support_settings, default_size: "heavy" } });
    await addSupportAt(a, [1, 2, 3], [0, 0, -1]);
    expect(backend.lastCall("set_support_points")).toEqual({ id: a, points: [{ position: [1, 2, 3], normal: [0, 0, -1], size: "heavy" }] });
    await addSupportAt(a, [4, 5, 6], [0, 0, -1]);
    expect((backend.lastCall("set_support_points")!.points as unknown[]).length).toBe(2);
    expect(st().objects[0].support_points).toBe(2);
  });

  it("removes the closest support and ignores clicks far away", async () => {
    const [a] = await plate("a");
    await addSupportAt(a, [0, 0, 5], [0, 0, -1]);
    await addSupportAt(a, [10, 0, 5], [0, 0, -1]);
    await removeSupportNear(a, [30, 30, 5]);
    expect(st().objects[0].support_points).toBe(2);
    await removeSupportNear(a, [9, 1, 5]);
    expect(backend.lastCall("set_support_points")!.points).toEqual([{ position: [0, 0, 5], normal: [0, 0, -1], size: "medium" }]);
    expect(st().objects[0].support_points).toBe(1);
  });
});

describe("slicing", () => {
  it("slices with a thumbnail and switches to the preview", async () => {
    await plate("a");
    expect(await sliceNow(null)).toBe(true);
    expect(backend.lastCall("slice")).toEqual({ format: null, thumbnail: "data:image/png;base64,iVBORw0KGgo=" });
    expect(st().mode).toBe("preview");
    expect(st().slice?.layer_count).toBe(400); // 20 mm / 0.05 mm
    expect(st().slice?.extension).toBe("goo");
    expect(st().layer).toBe(0);
    expect(st().sliceProgress).toBeNull();
  });

  it("passes the chosen output format", async () => {
    await plate("a");
    await sliceNow("ctb");
    expect(backend.lastCall("slice")!.format).toBe("ctb");
    expect(st().slice?.extension).toBe("ctb");
  });

  it("asks for a model first on an empty plate", async () => {
    expect(await sliceNow(null)).toBe(false);
    expect(backend.callsOf("slice")).toEqual([]);
    expect(toasts()).toEqual(["info: Add a model to the plate first"]);
  });

  it("shows slicing errors and stays on the plate", async () => {
    await plate("a");
    useStore.setState({ toasts: [] });
    backend.fail("slice", "model is outside the build volume");
    expect(await sliceNow(null)).toBe(false);
    expect(toasts()).toEqual(["error: model is outside the build volume"]);
    expect(st().mode).toBe("model");
    expect(st().slice).toBeNull();
    expect(st().sliceProgress).toBeNull();
  });

  it("does not report a cancelled slice as an error", async () => {
    await plate("a");
    useStore.setState({ toasts: [] });
    backend.fail("slice", "cancelled");
    await sliceNow(null);
    expect(toasts()).toEqual([]);
  });

  it("shows the progress while slicing", async () => {
    await plate("a");
    const release = backend.hold("slice");
    const p = sliceNow(null);
    await settle(1);
    expect(st().sliceProgress).toEqual({ done: 0, total: 1 });
    release();
    await p;
    expect(st().sliceProgress).toBeNull();
  });

  it("shows slicer warnings", async () => {
    await plate("a");
    const orig = backend.handle;
    backend.handle = async (cmd, args) => {
      const r = await orig(cmd, args);
      return cmd === "slice" ? { ...(r as object), warnings: ["Layer 12 has islands"] } : r;
    };
    (await import("@tauri-apps/api/mocks")).mockIPC(backend.handle, { shouldMockEvents: true });
    await sliceNow(null);
    expect(toasts()).toContain("info: Layer 12 has islands");
  });

  it("saves the sliced file with the suggested name and extension", async () => {
    await plate("a");
    await sliceNow(null);
    backend.saveResults.push("C:\\out\\plate.goo");
    await saveSliced();
    expect(backend.lastCall("plugin:dialog|save")!.options).toEqual({ defaultPath: "plate.goo", filters: [{ name: "GOO", extensions: ["goo"] }] });
    expect(backend.lastCall("save_slice")).toEqual({ path: "C:\\out\\plate.goo" });
    expect(toasts()).toContain("success: File saved");
  });

  it("does not open a save dialog without a slice", async () => {
    await saveSliced();
    expect(backend.callsOf("plugin:dialog|save")).toEqual([]);
  });

  it("opens sliced files from disk", async () => {
    backend.openResults.push("C:\\jobs\\old.ctb");
    await openSliced();
    expect(backend.lastCall("open_sliced_file")).toEqual({ path: "C:\\jobs\\old.ctb" });
    expect(st().mode).toBe("preview");
  });
});

describe("projects and export", () => {
  it("saves and reopens a project", async () => {
    await plate("a", "b");
    await saveProjectTo("C:\\p\\two.orproj");
    expect(backend.lastCall("save_project")).toEqual({ path: "C:\\p\\two.orproj" });
    expect(backend.lastCall("plugin:dialog|save")!.options).toEqual({
      filters: [{ name: "OpenResin project", extensions: ["orproj"] }],
      defaultPath: "plate.orproj",
    });
    expect(toasts()).toContain("success: Project saved");
    backend.objects = [];
    await syncScene();
    expect(st().objects).toEqual([]);
    await openProject("C:\\p\\two.orproj");
    expect(st().objects.map((o) => o.name)).toEqual(["a", "b"]);
    expect(backend.callsOf("get_config").length).toBeGreaterThanOrEqual(2);
  });

  it("does not save when the dialog is cancelled", async () => {
    await saveProject();
    expect(backend.callsOf("save_project")).toEqual([]);
  });

  it("shows an error for a project that cannot be opened", async () => {
    await openProject("C:\\missing.orproj");
    expect(toasts()).toEqual(["error: could not open C:\\missing.orproj"]);
  });

  it("starts a new plate after confirmation", async () => {
    const [a] = await plate("a");
    backend.askAnswers.push(false);
    await newProject();
    expect(backend.asked).toEqual(["Remove all models from the plate?"]);
    expect(st().objects).toHaveLength(1);
    backend.askAnswers.push(true);
    await newProject();
    expect(backend.callsOf("new_project")).toHaveLength(1);
    expect(st().objects).toEqual([]);
    expect(view.hasObject(a)).toBe(false);
  });

  it("does not ask when the plate is empty", async () => {
    await newProject();
    expect(backend.asked).toEqual([]);
    expect(backend.callsOf("new_project")).toHaveLength(1);
  });

  it("exports the plate with supports as STL", async () => {
    await plate("a");
    backend.saveResults.push("C:\\out\\plate.stl");
    await exportStl();
    expect(backend.lastCall("export_stl")).toEqual({ path: "C:\\out\\plate.stl", withSupports: true });
  });
});

describe("scene sync", () => {
  it("reloads meshes only for new or changed objects", async () => {
    const [a, b] = await plate("a", "b");
    const meshCalls = () => backend.callsOf("get_mesh").map((c) => c.id);
    expect(meshCalls()).toEqual([a, b]);
    await syncScene();
    expect(meshCalls()).toEqual([a, b]);
    await api.addPlatform(a, { margin: 1, thickness: 1, chamfer: 0 });
    await syncScene();
    expect(meshCalls()).toEqual([a, b, a]);
  });

  it("loads support and cutout meshes for objects that have them", async () => {
    const [a] = await plate("a");
    await autoSupports([a]);
    await api.hollowObject(a, { wall_thickness: 2, voxel_size: 0 });
    backend.calls = [];
    await syncScene();
    expect(backend.callsOf("get_support_points")).toEqual([{ id: a }]);
    expect(backend.callsOf("get_cutout_mesh")).toEqual([{ id: a }]);
    expect(view.objects.get(a)?.cutouts).not.toBeNull();
  });
});

describe("devices", () => {
  it("connects to a reachable printer", async () => {
    backend.addPrinter({ ip: "192.168.1.50", name: "Mars" });
    expect(await connectDevice("192.168.1.50")).toBe(true);
    const d = st().devices["192.168.1.50"];
    expect(d).toMatchObject({ connected: true, connecting: false, error: null });
    expect(d.status?.status.state).toBe("idle");
    expect(backend.lastCall("connect_printer")).toEqual({ ip: "192.168.1.50" });
  });

  it("stores the error of an unreachable printer", async () => {
    expect(await connectDevice("192.168.1.99")).toBe(false);
    expect(st().devices["192.168.1.99"]).toMatchObject({
      connected: false,
      connecting: false,
      error: "could not connect to 192.168.1.99: connection refused",
    });
  });

  it("marks the device as connecting while waiting", async () => {
    backend.addPrinter({ ip: "192.168.1.50" });
    const release = backend.hold("connect_printer");
    const p = connectDevice("192.168.1.50");
    expect(st().devices["192.168.1.50"].connecting).toBe(true);
    release();
    await p;
    expect(st().devices["192.168.1.50"].connecting).toBe(false);
  });

  it("discovers printers and keeps the names of saved ones", async () => {
    backend.addPrinter({ ip: "192.168.1.50", name: "Mars 5 Ultra" });
    backend.addPrinter({ ip: "192.168.1.51", name: "Saturn", model: "ELEGOO Saturn 4" });
    backend.config.devices = [{ ip: "192.168.1.50", name: "Workshop", model: null }];
    await loadSavedDevices();
    await discover();
    expect(backend.lastCall("discover_printers")).toEqual({ timeoutMs: 3000 });
    expect(st().devices["192.168.1.50"]).toMatchObject({ name: "Workshop", saved: true, model: "ELEGOO Mars 5 Ultra" });
    expect(st().devices["192.168.1.51"]).toMatchObject({ name: "Saturn", saved: false, model: "ELEGOO Saturn 4" });
    expect(st().devices["192.168.1.51"].discovered?.protocol).toBe("sdcp");
    expect(toasts()).toContain("success: Found 2 printers");
  });

  it("reports when no printer answers", async () => {
    await discover();
    expect(toasts()).toEqual(["info: No printers answered"]);
    expect(st().busy).toBeNull();
  });
});

describe("run()", () => {
  it("returns the result and clears the busy label", async () => {
    expect(await run("Working…", async () => 42)).toBe(42);
    expect(st().busy).toBeNull();
  });

  it("turns exceptions into an error toast instead of throwing", async () => {
    await expect(
      run("Working…", async () => {
        throw new Error("disk full");
      }),
    ).resolves.toBeUndefined();
    expect(toasts()).toEqual(["error: disk full"]);
    expect(st().busy).toBeNull();
  });
});

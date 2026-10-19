// WebGL-free stand-in for viewport/SceneView: records what the UI asks the 3D
// view to show, and exposes the callbacks so tests can simulate clicks/drags.
import type { MeshData, PrinterProfile, Transform, Vec3 } from "../api";
import type { SceneCallbacks, ViewTool } from "../viewport/SceneView";

interface Entry {
  geometry?: MeshData;
  transform?: Transform;
  subtract?: boolean;
  supports?: MeshData | null;
  cutouts?: MeshData | null;
}

export class FakeSceneView {
  static instances: FakeSceneView[] = [];
  static get last(): FakeSceneView | undefined {
    return FakeSceneView.instances[FakeSceneView.instances.length - 1];
  }

  objects = new Map<number, Entry>();
  log: [string, ...unknown[]][] = [];
  selection: number[] = [];
  tool: ViewTool = "select";
  printer: PrinterProfile | null = null;
  clip: number | null = null;
  cutPlane: unknown = null;
  measure: [Vec3 | null, Vec3 | null] = [null, null];
  overhang = { enabled: false, angle: 45 };
  camera = "iso";
  disposed = false;

  constructor(
    public container: HTMLElement | null = null,
    public cb: SceneCallbacks | null = null,
  ) {
    FakeSceneView.instances.push(this);
  }

  private entry(id: number): Entry {
    let e = this.objects.get(id);
    if (!e) {
      e = {};
      this.objects.set(id, e);
    }
    return e;
  }

  dispose() {
    this.disposed = true;
  }
  invalidate() {}
  setPrinter(p: PrinterProfile) {
    this.printer = p;
  }
  resetCamera(view: "iso" | "top" | "front" | "side" = "iso") {
    this.camera = view;
  }
  hasObject(id: number) {
    return this.objects.get(id)?.geometry != null;
  }
  setObjectGeometry(id: number, data: MeshData) {
    this.log.push(["setObjectGeometry", id]);
    this.entry(id).geometry = data;
  }
  setOverhang(enabled: boolean, angle: number) {
    this.overhang = { enabled, angle };
  }
  setClip(z: number | null) {
    this.clip = z;
  }
  setCutPlane(p: unknown) {
    this.cutPlane = p;
  }
  setMeasure(a: Vec3 | null, b: Vec3 | null) {
    this.measure = [a, b];
  }
  setCutouts(id: number, data: MeshData | null) {
    this.entry(id).cutouts = data;
  }
  setSubtract(id: number, subtract: boolean) {
    this.entry(id).subtract = subtract;
  }
  setObjectTransform(id: number, t: Transform) {
    this.entry(id).transform = t;
  }
  setSupports(id: number, data: MeshData | null) {
    this.log.push(["setSupports", id, data ? "mesh" : null]);
    this.entry(id).supports = data;
  }
  removeObject(id: number) {
    this.log.push(["removeObject", id]);
    this.objects.delete(id);
  }
  setSelection(ids: number[]) {
    this.selection = [...ids];
  }
  setTool(tool: ViewTool) {
    this.tool = tool;
  }
  captureThumbnail(_size = 400): string | null {
    return "data:image/png;base64,iVBORw0KGgo=";
  }
}

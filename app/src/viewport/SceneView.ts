// Imperative three.js scene: build plate, models, supports, gizmos and picking.
// Plate coordinates are used directly: millimetres, Z up, plate centred on the origin.
import * as THREE from "three";
import { OrbitControls } from "three/addons/controls/OrbitControls.js";
import { TransformControls } from "three/addons/controls/TransformControls.js";
import type { MeshData, PrinterProfile, Transform, Vec3 } from "../api";

export type ViewTool =
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

export interface SceneCallbacks {
  onSelect: (id: number | null, additive: boolean) => void;
  onTransformEnd: (id: number, t: Transform, mode: "translate" | "rotate" | "scale") => void;
  onMoveEnd: (moves: { id: number; transform: Transform }[]) => void;
  onMeasurePoint: (p: Vec3) => void;
  onSurfaceClick: (id: number, point: Vec3, normal: Vec3) => void;
  onSupportClick: (id: number, point: Vec3) => void;
  onDrillClick?: (id: number, point: Vec3, normal: Vec3) => void;
  onLabelClick?: (id: number, point: Vec3, normal: Vec3) => void;
  onFaceClick?: (id: number, faceIndex: number) => void;
}

interface ModelEntry {
  mesh: THREE.Mesh;
  supports: THREE.Mesh | null;
  cutouts: THREE.Mesh | null;
  outOfBounds: boolean;
  subtract: boolean;
}

const COLORS = {
  model: new THREE.Color("#4fb3e8"),
  selected: new THREE.Color("#f5a524"),
  outside: new THREE.Color("#e5484d"),
  support: new THREE.Color("#a9b4c2"),
  cutter: new THREE.Color("#ff6b6b"),
};

function buildGeometry(data: MeshData): THREE.BufferGeometry {
  const g = new THREE.BufferGeometry();
  g.setAttribute("position", new THREE.BufferAttribute(data.positions, 3));
  g.setIndex(new THREE.BufferAttribute(data.indices, 1));
  // flat shading: split vertices per face
  const flat = g.index && data.indices.length / 3 < 3_000_000 ? g.toNonIndexed() : g;
  flat.computeVertexNormals();
  flat.computeBoundingBox();
  flat.computeBoundingSphere();
  if (flat !== g) g.dispose();
  return flat;
}

export class SceneView {
  private renderer: THREE.WebGLRenderer;
  private scene = new THREE.Scene();
  private camera: THREE.PerspectiveCamera;
  private orbit: OrbitControls;
  private gizmo: TransformControls;
  private plate = new THREE.Group();
  private models = new Map<number, ModelEntry>();
  private selection = new Set<number>();
  private raycaster = new THREE.Raycaster();
  private needsRender = true;
  private frame = 0;
  private printer: PrinterProfile | null = null;
  private tool: ViewTool = "select";
  private gizmoMode: "translate" | "rotate" | "scale" | null = null;
  private drag: {
    plane: THREE.Plane;
    start: THREE.Vector3;
    objects: {
      id: number;
      mesh: THREE.Mesh;
      start: THREE.Vector3;
      supports: THREE.Mesh | null;
      sstart: THREE.Vector3 | null;
      cutouts: THREE.Mesh | null;
      cstart: THREE.Vector3 | null;
    }[];
    moved: boolean;
    pointer: number;
  } | null = null;
  private measureGroup = new THREE.Group();
  private cutPlane: THREE.Mesh | null = null;
  private overhang = { enabled: false, cos: Math.cos(THREE.MathUtils.degToRad(45)) };
  private overhangUniforms = new Map<THREE.Material, { uOverhang: { value: number }; uOverhangCos: { value: number } }>();
  /** Clipping plane used by the layer preview (keeps everything below the layer). */
  private clipPlane = new THREE.Plane(new THREE.Vector3(0, 0, -1), 1e6);
  private clipping = false;
  private resizeObserver: ResizeObserver;
  private downAt: { x: number; y: number } | null = null;
  private dragging = false;

  constructor(private container: HTMLElement, private cb: SceneCallbacks) {
    this.renderer = new THREE.WebGLRenderer({ antialias: true, preserveDrawingBuffer: true, alpha: false });
    this.renderer.setPixelRatio(Math.min(window.devicePixelRatio, 2));
    this.renderer.localClippingEnabled = true;
    this.renderer.setClearColor(getComputedStyle(document.documentElement).getPropertyValue("--viewport-bg").trim() || "#16181d");
    container.appendChild(this.renderer.domElement);

    this.camera = new THREE.PerspectiveCamera(40, 1, 1, 5000);
    this.camera.up.set(0, 0, 1);
    this.camera.position.set(0, -320, 260);

    this.orbit = new OrbitControls(this.camera, this.renderer.domElement);
    this.orbit.enableDamping = false;
    this.orbit.screenSpacePanning = true;
    this.orbit.addEventListener("change", () => this.invalidate());

    this.gizmo = new TransformControls(this.camera, this.renderer.domElement);
    this.gizmo.setSize(0.9);
    this.gizmo.addEventListener("dragging-changed", (e) => {
      const dragging = (e as unknown as { value: boolean }).value;
      this.orbit.enabled = !dragging;
      this.dragging = dragging;
      if (!dragging) this.commitGizmo();
    });
    this.gizmo.addEventListener("change", () => this.invalidate());
    this.scene.add(this.gizmo.getHelper());

    const hemi = new THREE.HemisphereLight(0xffffff, 0x404858, 1.6);
    hemi.position.set(0, 0, 1);
    this.scene.add(hemi);
    const key = new THREE.DirectionalLight(0xffffff, 1.6);
    key.position.set(-200, -300, 500);
    this.scene.add(key);
    const fill = new THREE.DirectionalLight(0xbfd4ff, 0.6);
    fill.position.set(300, 200, 200);
    this.scene.add(fill);
    this.scene.add(this.plate);

    this.scene.add(this.measureGroup);
    const el = this.renderer.domElement;
    // capture phase: runs before OrbitControls so a drag on a model moves the model
    container.addEventListener("pointerdown", (e) => this.onPointerDown(e), { capture: true });
    window.addEventListener("pointermove", this.onPointerMove);
    window.addEventListener("pointerup", this.onWindowPointerUp);
    el.addEventListener("pointerup", (e) => this.onPointerUp(e));
    el.addEventListener("contextmenu", (e) => e.preventDefault());

    this.resizeObserver = new ResizeObserver(() => this.resize());
    this.resizeObserver.observe(container);
    this.resize();
    const loop = () => {
      this.frame = requestAnimationFrame(loop);
      if (this.needsRender) {
        this.needsRender = false;
        this.renderer.render(this.scene, this.camera);
      }
    };
    loop();
  }

  dispose() {
    cancelAnimationFrame(this.frame);
    window.removeEventListener("pointermove", this.onPointerMove);
    window.removeEventListener("pointerup", this.onWindowPointerUp);
    this.resizeObserver.disconnect();
    this.gizmo.detach();
    this.gizmo.dispose();
    this.orbit.dispose();
    for (const id of [...this.models.keys()]) this.removeObject(id);
    this.renderer.dispose();
    this.renderer.domElement.remove();
  }

  invalidate() {
    this.needsRender = true;
  }

  private resize() {
    const w = this.container.clientWidth;
    const h = this.container.clientHeight;
    if (w === 0 || h === 0) return;
    this.renderer.setSize(w, h, false);
    this.renderer.domElement.style.width = "100%";
    this.renderer.domElement.style.height = "100%";
    this.camera.aspect = w / h;
    this.camera.updateProjectionMatrix();
    this.invalidate();
  }

  // ---------------------------------------------------------------- plate

  setPrinter(p: PrinterProfile) {
    const changed =
      !this.printer ||
      this.printer.size_x !== p.size_x ||
      this.printer.size_y !== p.size_y ||
      this.printer.size_z !== p.size_z;
    this.printer = p;
    for (const c of [...this.plate.children]) {
      this.plate.remove(c);
      const m = c as THREE.Mesh;
      m.geometry?.dispose();
      (m.material as THREE.Material | undefined)?.dispose?.();
    }
    const sx = p.size_x;
    const sy = p.size_y;
    const sz = p.size_z;
    const css = getComputedStyle(document.documentElement);
    const plateColor = new THREE.Color(css.getPropertyValue("--plate").trim() || "#2a2f39");
    const gridColor = new THREE.Color(css.getPropertyValue("--plate-grid").trim() || "#3b4250");
    const edgeColor = new THREE.Color(css.getPropertyValue("--plate-edge").trim() || "#7d8aa0");

    const plane = new THREE.Mesh(
      new THREE.PlaneGeometry(sx, sy),
      new THREE.MeshBasicMaterial({ color: plateColor, side: THREE.DoubleSide }),
    );
    plane.position.z = -0.05;
    plane.userData.kind = "plate";
    this.plate.add(plane);

    const pts: number[] = [];
    for (let x = 0; x <= sx / 2; x += 10) {
      for (const s of x === 0 ? [1] : [1, -1]) pts.push(s * x, -sy / 2, 0, s * x, sy / 2, 0);
    }
    for (let y = 0; y <= sy / 2; y += 10) {
      for (const s of y === 0 ? [1] : [1, -1]) pts.push(-sx / 2, s * y, 0, sx / 2, s * y, 0);
    }
    const grid = new THREE.BufferGeometry();
    grid.setAttribute("position", new THREE.Float32BufferAttribute(pts, 3));
    this.plate.add(new THREE.LineSegments(grid, new THREE.LineBasicMaterial({ color: gridColor })));

    const box = new THREE.Box3(new THREE.Vector3(-sx / 2, -sy / 2, 0), new THREE.Vector3(sx / 2, sy / 2, sz));
    const helper = new THREE.Box3Helper(box, edgeColor);
    (helper.material as THREE.LineBasicMaterial).transparent = true;
    (helper.material as THREE.LineBasicMaterial).opacity = 0.35;
    this.plate.add(helper);

    // front marker
    const tri = new THREE.Shape();
    tri.moveTo(-6, -sy / 2 - 4);
    tri.lineTo(6, -sy / 2 - 4);
    tri.lineTo(0, -sy / 2 - 12);
    tri.closePath();
    const marker = new THREE.Mesh(new THREE.ShapeGeometry(tri), new THREE.MeshBasicMaterial({ color: edgeColor }));
    this.plate.add(marker);

    if (changed) this.resetCamera();
    this.refreshBoundsColors();
    this.invalidate();
  }

  resetCamera(view: "iso" | "top" | "front" | "side" = "iso") {
    const p = this.printer;
    // fit the whole build volume into the view
    const sx = p?.size_x ?? 200;
    const sy = p?.size_y ?? 120;
    const sz = p?.size_z ?? 200;
    const radius = 0.5 * Math.sqrt(sx * sx + sy * sy + sz * sz);
    const target = new THREE.Vector3(0, 0, sz * 0.3);
    const fov = THREE.MathUtils.degToRad(this.camera.fov);
    const fitH = radius / Math.sin(fov / 2);
    const fitW = radius / Math.sin(Math.atan(Math.tan(fov / 2) * this.camera.aspect));
    const d = Math.max(fitH, fitW) * (view === "iso" ? 0.95 : 0.85);
    const dirs: Record<string, THREE.Vector3> = {
      iso: new THREE.Vector3(0.55, -0.85, 0.75),
      top: new THREE.Vector3(0, -0.0001, 1),
      front: new THREE.Vector3(0, -1, 0.12),
      side: new THREE.Vector3(1, 0, 0.12),
    };
    const dir = dirs[view].clone().normalize();
    this.camera.position.copy(target.clone().add(dir.multiplyScalar(d)));
    this.orbit.target.copy(target);
    this.orbit.update();
    this.invalidate();
  }

  // ---------------------------------------------------------------- models

  hasObject(id: number) {
    return this.models.has(id);
  }

  setObjectGeometry(id: number, data: MeshData) {
    const geometry = buildGeometry(data);
    const existing = this.models.get(id);
    if (existing) {
      existing.mesh.geometry.dispose();
      existing.mesh.geometry = geometry;
    } else {
      const mat = this.modelMaterial();
      const mesh = new THREE.Mesh(geometry, mat);
      mesh.userData = { kind: "model", id };
      this.scene.add(mesh);
      this.models.set(id, { mesh, supports: null, cutouts: null, outOfBounds: false, subtract: false });
    }
    this.invalidate();
  }

  private modelMaterial(): THREE.MeshStandardMaterial {
    const mat = new THREE.MeshStandardMaterial({
      color: COLORS.model,
      roughness: 0.55,
      metalness: 0.05,
      flatShading: true,
      clippingPlanes: [this.clipPlane],
      side: this.clipping ? THREE.DoubleSide : THREE.FrontSide,
    });
    const uniforms = {
      uOverhang: { value: this.overhang.enabled ? 1 : 0 },
      uOverhangCos: { value: this.overhang.cos },
    };
    this.overhangUniforms.set(mat, uniforms);
    mat.onBeforeCompile = (shader) => {
      Object.assign(shader.uniforms, uniforms);
      shader.vertexShader = shader.vertexShader
        .replace("#include <common>", "#include <common>\nvarying vec3 vWorldNormal;")
        .replace(
          "#include <beginnormal_vertex>",
          "#include <beginnormal_vertex>\nvWorldNormal = normalize(mat3(modelMatrix) * objectNormal);",
        );
      shader.fragmentShader = shader.fragmentShader
        .replace(
          "#include <common>",
          "#include <common>\nvarying vec3 vWorldNormal;\nuniform float uOverhang;\nuniform float uOverhangCos;",
        )
        .replace(
          "#include <color_fragment>",
          "#include <color_fragment>\nif (uOverhang > 0.5 && normalize(vWorldNormal).z < -uOverhangCos) { diffuseColor.rgb = mix(diffuseColor.rgb, vec3(0.93, 0.22, 0.2), 0.85); }\nif (!gl_FrontFacing) { diffuseColor.rgb = vec3(1.0, 0.62, 0.12); }",
        );
    };
    return mat;
  }

  /** Highlights downward facing surfaces flatter than `angleDeg` from horizontal. */
  setOverhang(enabled: boolean, angleDeg: number) {
    this.overhang = { enabled, cos: Math.cos(THREE.MathUtils.degToRad(angleDeg)) };
    for (const u of this.overhangUniforms.values()) {
      u.uOverhang.value = enabled ? 1 : 0;
      u.uOverhangCos.value = this.overhang.cos;
    }
    this.invalidate();
  }

  /** Cuts the view at height `z` (layer preview); `null` shows everything. */
  setClip(z: number | null) {
    this.clipping = z != null;
    this.clipPlane.constant = z ?? 1e6;
    for (const m of this.models.values()) {
      for (const mesh of [m.mesh, m.supports]) {
        if (!mesh) continue;
        const mat = mesh.material as THREE.MeshStandardMaterial;
        const side = this.clipping ? THREE.DoubleSide : THREE.FrontSide;
        if (mat.side !== side) {
          mat.side = side;
          mat.needsUpdate = true;
        }
      }
    }
    this.gizmo.getHelper().visible = !this.clipping;
    this.invalidate();
  }

  /** Shows a translucent plane where a cut will happen. */
  setCutPlane(p: { axis: "x" | "y" | "z"; position: number; bounds: { min: Vec3; max: Vec3 } } | null) {
    if (this.cutPlane) {
      this.scene.remove(this.cutPlane);
      this.cutPlane.geometry.dispose();
      (this.cutPlane.material as THREE.Material).dispose();
      this.cutPlane = null;
    }
    if (p) {
      const pad = 4;
      const size = [0, 1, 2].map((i) => p.bounds.max[i] - p.bounds.min[i] + pad * 2);
      const c = [0, 1, 2].map((i) => (p.bounds.max[i] + p.bounds.min[i]) / 2);
      const [w, h] = p.axis === "z" ? [size[0], size[1]] : p.axis === "x" ? [size[1], size[2]] : [size[0], size[2]];
      const mesh = new THREE.Mesh(
        new THREE.PlaneGeometry(w, h),
        new THREE.MeshBasicMaterial({ color: "#f5a524", transparent: true, opacity: 0.35, side: THREE.DoubleSide, depthWrite: false }),
      );
      if (p.axis === "z") mesh.position.set(c[0], c[1], p.position);
      if (p.axis === "x") {
        mesh.rotation.set(Math.PI / 2, Math.PI / 2, 0);
        mesh.position.set(p.position, c[1], c[2]);
      }
      if (p.axis === "y") {
        mesh.rotation.set(Math.PI / 2, 0, 0);
        mesh.position.set(c[0], p.position, c[2]);
      }
      mesh.renderOrder = 5;
      this.scene.add(mesh);
      this.cutPlane = mesh;
    }
    this.invalidate();
  }

  setMeasure(a: Vec3 | null, b: Vec3 | null) {
    for (const c of [...this.measureGroup.children]) {
      this.measureGroup.remove(c);
      const m = c as THREE.Mesh;
      m.geometry?.dispose();
      (m.material as THREE.Material | undefined)?.dispose?.();
    }
    const dot = (p: Vec3) => {
      const m = new THREE.Mesh(
        new THREE.SphereGeometry(0.8, 16, 12),
        new THREE.MeshBasicMaterial({ color: "#f5a524", depthTest: false }),
      );
      m.position.set(...p);
      m.renderOrder = 10;
      this.measureGroup.add(m);
    };
    if (a) dot(a);
    if (b) dot(b);
    if (a && b) {
      const g = new THREE.BufferGeometry().setFromPoints([new THREE.Vector3(...a), new THREE.Vector3(...b)]);
      const line = new THREE.Line(g, new THREE.LineBasicMaterial({ color: "#f5a524", depthTest: false }));
      line.renderOrder = 10;
      this.measureGroup.add(line);
    }
    this.invalidate();
  }

  /** Hollowing cavities / drain holes, drawn inside a translucent model. */
  setCutouts(id: number, data: MeshData | null) {
    const m = this.models.get(id);
    if (!m) return;
    if (m.cutouts) {
      this.scene.remove(m.cutouts);
      m.cutouts.geometry.dispose();
      (m.cutouts.material as THREE.Material).dispose();
      m.cutouts = null;
    }
    const has = !!data && data.indices.length > 0;
    if (has) {
      const mesh = new THREE.Mesh(
        buildGeometry(data!),
        new THREE.MeshStandardMaterial({
          color: "#ffb347",
          roughness: 0.8,
          flatShading: true,
          side: THREE.DoubleSide,
          clippingPlanes: [this.clipPlane],
        }),
      );
      mesh.userData = { kind: "cutout", id };
      this.scene.add(mesh);
      m.cutouts = mesh;
    }
    if (!m.subtract) {
      const mat = m.mesh.material as THREE.MeshStandardMaterial;
      mat.transparent = has;
      mat.opacity = has ? 0.55 : 1;
      mat.depthWrite = !has;
      mat.needsUpdate = true;
    }
    this.invalidate();
  }

  /** Cutter objects (boolean difference) are drawn as translucent volumes. */
  setSubtract(id: number, subtract: boolean) {
    const m = this.models.get(id);
    if (!m || m.subtract === subtract) return;
    m.subtract = subtract;
    const mat = m.mesh.material as THREE.MeshStandardMaterial;
    mat.transparent = subtract;
    mat.opacity = subtract ? 0.35 : 1;
    mat.depthWrite = !subtract;
    mat.needsUpdate = true;
    this.refreshBoundsColors();
  }

  setObjectTransform(id: number, t: Transform) {
    const m = this.models.get(id);
    if (!m || (this.dragging && this.gizmo.object === m.mesh)) return;
    m.mesh.position.set(...t.translation);
    m.mesh.quaternion.set(...t.rotation);
    m.mesh.scale.set(...t.scale);
    m.mesh.updateMatrixWorld(true);
    this.refreshBoundsColors();
    this.invalidate();
  }

  setSupports(id: number, data: MeshData | null) {
    const m = this.models.get(id);
    if (!m) return;
    if (m.supports) {
      this.scene.remove(m.supports);
      m.supports.geometry.dispose();
      (m.supports.material as THREE.Material).dispose();
      m.supports = null;
    }
    if (data && data.indices.length > 0) {
      const mesh = new THREE.Mesh(
        buildGeometry(data),
        new THREE.MeshStandardMaterial({
          color: COLORS.support,
          roughness: 0.7,
          flatShading: true,
          clippingPlanes: [this.clipPlane],
          side: this.clipping ? THREE.DoubleSide : THREE.FrontSide,
        }),
      );
      mesh.userData = { kind: "support", id };
      this.scene.add(mesh);
      m.supports = mesh;
    }
    this.invalidate();
  }

  removeObject(id: number) {
    const m = this.models.get(id);
    if (!m) return;
    if (this.gizmo.object === m.mesh) this.gizmo.detach();
    this.scene.remove(m.mesh);
    m.mesh.geometry.dispose();
    this.overhangUniforms.delete(m.mesh.material as THREE.Material);
    this.setCutouts(id, null);
    (m.mesh.material as THREE.Material).dispose();
    this.setSupports(id, null);
    this.models.delete(id);
    this.selection.delete(id);
    this.invalidate();
  }

  setSelection(ids: number[]) {
    this.selection = new Set(ids);
    this.updateGizmo();
    this.refreshBoundsColors();
  }

  setTool(tool: ViewTool) {
    this.tool = tool;
    this.gizmoMode = tool === "move" ? "translate" : tool === "rotate" ? "rotate" : tool === "scale" ? "scale" : null;
    if (tool !== "measure") this.setMeasure(null, null);
    this.updateGizmo();
  }

  private updateGizmo() {
    const id = this.selection.size === 1 ? [...this.selection][0] : null;
    const m = id != null ? this.models.get(id) : undefined;
    if (!m || !this.gizmoMode) {
      this.gizmo.detach();
    } else {
      this.gizmo.attach(m.mesh);
      this.gizmo.setMode(this.gizmoMode);
      this.gizmo.setSpace(this.gizmoMode === "scale" ? "local" : "world");
      this.gizmo.showZ = this.gizmoMode !== "translate";
    }
    const crosshair = ["measure", "layflat", "support_add", "support_remove", "drill", "label"].includes(this.tool);
    this.renderer.domElement.style.cursor = crosshair ? "crosshair" : "";
    this.invalidate();
  }

  private commitGizmo() {
    const obj = this.gizmo.object as THREE.Mesh | undefined;
    if (!obj || !this.gizmoMode) return;
    const id = obj.userData.id as number;
    this.cb.onTransformEnd(id, this.transformOf(obj), this.gizmoMode);
  }

  private transformOf(o: THREE.Object3D): Transform {
    return {
      translation: [o.position.x, o.position.y, o.position.z],
      rotation: [o.quaternion.x, o.quaternion.y, o.quaternion.z, o.quaternion.w],
      scale: [o.scale.x, o.scale.y, o.scale.z],
    };
  }

  private refreshBoundsColors() {
    const p = this.printer;
    for (const [id, m] of this.models) {
      const b = new THREE.Box3().setFromObject(m.mesh);
      const out =
        !!p &&
        (b.min.x < -p.size_x / 2 - 0.01 ||
          b.max.x > p.size_x / 2 + 0.01 ||
          b.min.y < -p.size_y / 2 - 0.01 ||
          b.max.y > p.size_y / 2 + 0.01 ||
          b.max.z > p.size_z + 0.01);
      m.outOfBounds = out;
      const mat = m.mesh.material as THREE.MeshStandardMaterial;
      mat.color.copy(
        this.selection.has(id) ? COLORS.selected : m.subtract ? COLORS.cutter : out ? COLORS.outside : COLORS.model,
      );
      mat.emissive.set(this.selection.has(id) ? "#3a2400" : "#000000");
    }
    this.invalidate();
  }

  // ---------------------------------------------------------------- picking

  private ndc(e: { clientX: number; clientY: number }): THREE.Vector2 {
    const rect = this.renderer.domElement.getBoundingClientRect();
    return new THREE.Vector2(
      ((e.clientX - rect.left) / rect.width) * 2 - 1,
      -((e.clientY - rect.top) / rect.height) * 2 + 1,
    );
  }

  private pickModels(e: { clientX: number; clientY: number }, withSupports: boolean) {
    this.raycaster.setFromCamera(this.ndc(e), this.camera);
    const targets: THREE.Object3D[] = [];
    for (const m of this.models.values()) {
      targets.push(m.mesh);
      if (withSupports && m.supports) targets.push(m.supports);
    }
    return this.raycaster.intersectObjects(targets, false)[0];
  }

  private onPointerDown(e: PointerEvent) {
    this.downAt = { x: e.clientX, y: e.clientY };
    if (e.button !== 0 || (this.tool !== "select" && this.tool !== "move")) return;
    // let the gizmo handle its own handles
    if (this.gizmo.object && (this.gizmo as unknown as { axis: string | null }).axis) return;
    const hit = this.pickModels(e, false);
    if (!hit || hit.object.userData.kind !== "model") return;
    const id = hit.object.userData.id as number;
    let ids = [...this.selection];
    if (!this.selection.has(id)) {
      const additive = e.shiftKey || e.ctrlKey || e.metaKey;
      this.cb.onSelect(id, additive);
      ids = additive ? [...ids, id] : [id];
    }
    this.drag = {
      plane: new THREE.Plane(new THREE.Vector3(0, 0, 1), -hit.point.z),
      start: hit.point.clone(),
      moved: false,
      pointer: e.pointerId,
      objects: ids
        .map((i) => ({ i, m: this.models.get(i) }))
        .filter((x) => !!x.m)
        .map(({ i, m }) => ({
          id: i,
          mesh: m!.mesh,
          start: m!.mesh.position.clone(),
          supports: m!.supports,
          sstart: m!.supports ? m!.supports.position.clone() : null,
          cutouts: m!.cutouts,
          cstart: m!.cutouts ? m!.cutouts.position.clone() : null,
        })),
    };
    this.orbit.enabled = false;
  }

  private onPointerMove = (e: PointerEvent) => {
    const d = this.drag;
    if (!d || e.pointerId !== d.pointer) return;
    if (!d.moved && this.downAt && Math.hypot(e.clientX - this.downAt.x, e.clientY - this.downAt.y) < 4) return;
    this.raycaster.setFromCamera(this.ndc(e), this.camera);
    const p = new THREE.Vector3();
    if (!this.raycaster.ray.intersectPlane(d.plane, p)) return;
    const delta = p.sub(d.start);
    delta.z = 0;
    d.moved = true;
    this.renderer.domElement.style.cursor = "grabbing";
    for (const o of d.objects) {
      o.mesh.position.copy(o.start).add(delta);
      o.mesh.updateMatrixWorld(true);
      if (o.supports && o.sstart) o.supports.position.copy(o.sstart).add(delta);
      if (o.cutouts && o.cstart) o.cutouts.position.copy(o.cstart).add(delta);
    }
    this.refreshBoundsColors();
  };

  private onWindowPointerUp = (e: PointerEvent) => {
    const d = this.drag;
    if (!d || e.pointerId !== d.pointer) return;
    this.drag = null;
    this.orbit.enabled = true;
    this.renderer.domElement.style.cursor = "";
    if (d.moved) {
      this.downAt = null; // a drag, not a click
      this.cb.onMoveEnd(d.objects.map((o) => ({ id: o.id, transform: this.transformOf(o.mesh) })));
    }
  };

  private onPointerUp(e: PointerEvent) {
    const d = this.downAt;
    this.downAt = null;
    if (!d || e.button !== 0) return;
    if (Math.hypot(e.clientX - d.x, e.clientY - d.y) > 4) return; // it was a drag
    if (this.gizmo.dragging) return;
    if (this.tool === "measure") {
      const hit = this.pickModels(e, true);
      let p: THREE.Vector3 | null = hit ? hit.point : null;
      if (!p) {
        p = new THREE.Vector3();
        if (!this.raycaster.ray.intersectPlane(new THREE.Plane(new THREE.Vector3(0, 0, 1), 0), p)) return;
      }
      this.cb.onMeasurePoint([p.x, p.y, p.z]);
      return;
    }
    const supportMode = this.tool === "support_add" || this.tool === "support_remove";
    const hit = this.pickModels(e, supportMode);
    if (!hit) {
      if (["select", "move", "rotate", "scale"].includes(this.tool)) this.cb.onSelect(null, false);
      return;
    }
    const id = hit.object.userData.id as number;
    if (this.tool === "layflat") {
      if (hit.object.userData.kind === "model" && hit.faceIndex != null) this.cb.onFaceClick?.(id, hit.faceIndex);
      return;
    }
    if (this.tool === "drill" || this.tool === "label") {
      if (hit.object.userData.kind === "model" && hit.face) {
        const n = hit.face.normal.clone().transformDirection(hit.object.matrixWorld).normalize();
        const p: Vec3 = [hit.point.x, hit.point.y, hit.point.z];
        if (this.tool === "drill") this.cb.onDrillClick?.(id, p, [n.x, n.y, n.z]);
        else this.cb.onLabelClick?.(id, p, [n.x, n.y, n.z]);
      }
      return;
    }
    if (this.tool === "support_remove") {
      if (hit.object.userData.kind === "support") this.cb.onSupportClick(id, [hit.point.x, hit.point.y, hit.point.z]);
      return;
    }
    if (this.tool === "support_add") {
      if (hit.object.userData.kind === "support") {
        this.cb.onSupportClick(id, [hit.point.x, hit.point.y, hit.point.z]);
      } else if (hit.face) {
        const n = hit.face.normal.clone().transformDirection(hit.object.matrixWorld).normalize();
        this.cb.onSurfaceClick(id, [hit.point.x, hit.point.y, hit.point.z], [n.x, n.y, n.z]);
      }
      return;
    }
    // select/move/rotate/scale: selection on pointerdown already happened for select/move
    if (this.tool === "rotate" || this.tool === "scale") this.cb.onSelect(id, e.shiftKey || e.ctrlKey || e.metaKey);
  }

  /** Renders an isometric thumbnail of the plate content as a PNG data URL. */
  captureThumbnail(size = 400): string | null {
    if (this.models.size === 0) return null;
    const box = new THREE.Box3();
    for (const m of this.models.values()) {
      box.expandByObject(m.mesh);
      if (m.supports) box.expandByObject(m.supports);
    }
    const center = box.getCenter(new THREE.Vector3());
    const radius = box.getSize(new THREE.Vector3()).length() * 0.5;
    const cam = new THREE.PerspectiveCamera(30, 1, 0.1, 10000);
    cam.up.set(0, 0, 1);
    const dir = new THREE.Vector3(0.6, -0.9, 0.7).normalize();
    cam.position.copy(center.clone().add(dir.multiplyScalar(radius / Math.sin(THREE.MathUtils.degToRad(15)) * 1.05)));
    cam.lookAt(center);
    const target = new THREE.WebGLRenderTarget(size, size, { samples: 4 });
    const helper = this.gizmo.getHelper();
    const helperVisible = helper.visible;
    helper.visible = false;
    this.plate.visible = false;
    const prevColors = new Map<number, THREE.Color>();
    for (const [id, m] of this.models) {
      const mat = m.mesh.material as THREE.MeshStandardMaterial;
      prevColors.set(id, mat.color.clone());
      mat.color.copy(COLORS.model);
      mat.emissive.set("#000000");
    }
    const prevClear = this.renderer.getClearColor(new THREE.Color());
    this.renderer.setClearColor("#101318");
    this.renderer.setRenderTarget(target);
    this.renderer.render(this.scene, cam);
    const pixels = new Uint8Array(size * size * 4);
    this.renderer.readRenderTargetPixels(target, 0, 0, size, size, pixels);
    this.renderer.setRenderTarget(null);
    this.renderer.setClearColor(prevClear);
    target.dispose();
    helper.visible = helperVisible;
    this.plate.visible = true;
    for (const [id, c] of prevColors) (this.models.get(id)!.mesh.material as THREE.MeshStandardMaterial).color.copy(c);
    this.refreshBoundsColors();
    // flip vertically into a canvas
    const canvas = document.createElement("canvas");
    canvas.width = size;
    canvas.height = size;
    const ctx = canvas.getContext("2d")!;
    const img = ctx.createImageData(size, size);
    for (let y = 0; y < size; y++) {
      img.data.set(pixels.subarray((size - 1 - y) * size * 4, (size - y) * size * 4), y * size * 4);
    }
    ctx.putImageData(img, 0, 0);
    this.invalidate();
    return canvas.toDataURL("image/png");
  }
}

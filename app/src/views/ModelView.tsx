import { useState } from "react";
import { api, type ObjectInfo, type SupportSettings, type Transform } from "../api";
import { applyObject, autoSupports, clearSupports, openModelsDialog, run, sceneRef, sliceNow } from "../actions";
import { align, arrangeAll, copies, fillPlate, mirror, refreshHistory, syncScene } from "../editActions";
import { fmtNum } from "../format";
import { useStore } from "../store";
import { Button, Icon, NumberField, Progress, Toggle } from "../ui";
import { Ribbon } from "./Ribbon";
import { Viewport } from "./Viewport";
import { toolPanels } from "./toolPanels";
import { PreviewView } from "./PreviewView";
import { StatusBar } from "./StatusBar";
import { t as tr } from "../i18n";

const HINTS: Partial<Record<string, string>> = {
  select: "Click to select · drag a model to move it · Ctrl+click for multiple · arrow keys nudge (Shift = 10 mm)",
  move: "Drag the model or the arrows · arrow keys nudge",
  rotate: "Drag the rings to rotate · the model drops back onto the plate",
  scale: "Drag the handles or type exact sizes on the right",
  layflat: "Click the face that should lie on the build plate",
  support_add: "Click the model to add a support · click a support to remove it",
  support_remove: "Click a support to remove it",
  measure: "Click two points to measure their distance",
  drill: "Click on the model surface to drill a drain hole",
  label: "Click on the model surface to place the text",
};

export function ModelView() {
  const tool = useStore((s) => s.tool);
  const panel = useStore((s) => s.panel);
  const objects = useStore((s) => s.objects);
  const measure = useStore((s) => s.measure);
  const extra = panel ? toolPanels[panel] : undefined;
  const mode = useStore((s) => s.mode);
  const slice = useStore((s) => s.slice);
  const preview = mode === "preview";
  // a sliced file opened from disk is unrelated to the plate: show only the 2D layers
  const show3d = !preview || (slice?.duration_ms ?? 0) > 0;

  return (
    <div className="model-view">
      {!preview && <Ribbon />}
      <div className="model-main">
        <div className={`viewport-wrap ${preview ? "preview-3d" : ""}`} hidden={!show3d}>
          <Viewport />
          {!preview && !objects.length && (
            <div className="empty-hint">
              <Icon name="open" size={36} />
              <p>{tr("Drop STL, OBJ or 3MF files here")}</p>
              <Button variant="primary" icon="open" onClick={() => void openModelsDialog()}>
                {tr("Open models")}
              </Button>
              <div className="row shapes">
                <span className="muted small">{tr("or add a test shape:")}</span>
                {(["cube", "cylinder", "sphere", "cone"] as const).map((k) => (
                  <Button
                    key={k}
                    small
                    variant="ghost"
                    onClick={() => void run(null, async () => syncScene(await api.addPrimitive(k)))}
                  >
                    {tr(k[0].toUpperCase() + k.slice(1))}
                  </Button>
                ))}
              </div>
            </div>
          )}
          {!preview && objects.length > 0 && HINTS[tool] && <div className="viewport-hint">{tr(HINTS[tool]!)}</div>}
          {tool === "measure" && measure && measure.b !== measure.a && (
            <div className="measure-readout">
              <strong>{fmtNum(Math.hypot(measure.b[0] - measure.a[0], measure.b[1] - measure.a[1], measure.b[2] - measure.a[2]), 2)} mm</strong>
              <span className="muted small">
                ΔX {fmtNum(Math.abs(measure.b[0] - measure.a[0]), 2)} · ΔY {fmtNum(Math.abs(measure.b[1] - measure.a[1]), 2)} · ΔZ{" "}
                {fmtNum(Math.abs(measure.b[2] - measure.a[2]), 2)}
              </span>
            </div>
          )}
          <div className="view-buttons">
            <Button icon="cube" small variant="ghost" title={tr("3D view")} onClick={() => sceneRef.current?.resetCamera("iso")} />
            <Button icon="eye" small variant="ghost" title={tr("Top view")} onClick={() => sceneRef.current?.resetCamera("top")} />
            <Button icon="printer" small variant="ghost" title={tr("Front view")} onClick={() => sceneRef.current?.resetCamera("front")} />
          </div>
        </div>
        {preview && <PreviewView />}
        <aside className="side-panel" hidden={preview}>
          <ObjectsPanel />
          {panel === "copy" && <CopyPanel />}
          {panel === "mirror" && <MirrorPanel />}
          {panel === "align" && <AlignPanel />}
          {panel === "layout" && <LayoutPanel />}
          {(panel === "support" || panel === "raft") && <SupportPanel raftOnly={panel === "raft"} />}
          {extra}
          {!panel && <SelectionPanel />}
          <SlicePanel />
        </aside>
      </div>
      {!preview && <StatusBar />}
    </div>
  );
}

function SelectionPanel() {
  const selection = useStore((s) => s.selection);
  const objects = useStore((s) => s.objects);
  const tool = useStore((s) => s.tool);
  const selected = objects.filter((o) => selection.includes(o.id));
  if (selected.length !== 1 || tool.startsWith("support")) return null;
  return <TransformPanel obj={selected[0]} />;
}

function ObjectsPanel() {
  const objects = useStore((s) => s.objects);
  const selection = useStore((s) => s.selection);
  const select = useStore((s) => s.select);
  const [editing, setEditing] = useState<number | null>(null);
  return (
    <section className="panel">
      <header className="panel-header">
        <h3>
          {tr("Models")} ({objects.length})
        </h3>
        <div className="row tight">
          <Button icon="select" small variant="ghost" title={tr("Select all (Ctrl+A)")} disabled={!objects.length} onClick={() => select(objects.map((o) => o.id))} />
          <Button icon="plus" small variant="ghost" title={tr("Add models")} onClick={() => void openModelsDialog()} />
        </div>
      </header>
      {objects.length === 0 ? (
        <p className="muted small">{tr("No models on the plate.")}</p>
      ) : (
        <ul className="object-list">
          {objects.map((o) => (
            <li
              key={o.id}
              className={selection.includes(o.id) ? "selected" : ""}
              onClick={(e) =>
                select(
                  e.ctrlKey || e.metaKey || e.shiftKey
                    ? selection.includes(o.id)
                      ? selection.filter((x) => x !== o.id)
                      : [...selection, o.id]
                    : [o.id],
                )
              }
              onDoubleClick={() => setEditing(o.id)}
            >
              {editing === o.id ? (
                <input
                  autoFocus
                  defaultValue={o.name}
                  onBlur={(e) => {
                    setEditing(null);
                    const n = e.target.value.trim();
                    if (n && n !== o.name) void api.renameObject(o.id, n).then((r) => useStore.getState().upsertObjects([r]));
                  }}
                  onKeyDown={(e) => e.key === "Enter" && (e.target as HTMLInputElement).blur()}
                />
              ) : (
                <span className="name" title={tr("{name} (double-click to rename)", { name: o.name })}>
                  {o.name}
                </span>
              )}
              <span className="muted small">
                {fmtNum(o.bounds.max[0] - o.bounds.min[0], 1)}×{fmtNum(o.bounds.max[1] - o.bounds.min[1], 1)}×
                {fmtNum(o.bounds.max[2] - o.bounds.min[2], 1)}
              </span>
              {o.support_points > 0 && (
                <span className="badge" title={tr("Support points")}>
                  {o.support_points}
                </span>
              )}
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}

function quatToEuler(q: [number, number, number, number]): [number, number, number] {
  const [x, y, z, w] = q;
  const rx = Math.atan2(2 * (w * x + y * z), 1 - 2 * (x * x + y * y));
  const sinp = 2 * (w * y - z * x);
  const ry = Math.abs(sinp) >= 1 ? (Math.sign(sinp) * Math.PI) / 2 : Math.asin(sinp);
  const rz = Math.atan2(2 * (w * z + x * y), 1 - 2 * (y * y + z * z));
  const d = 180 / Math.PI;
  return [rx * d, ry * d, rz * d];
}

function eulerToQuat(e: [number, number, number]): [number, number, number, number] {
  const [rx, ry, rz] = e.map((v) => (v * Math.PI) / 360);
  const cx = Math.cos(rx), sx = Math.sin(rx);
  const cy = Math.cos(ry), sy = Math.sin(ry);
  const cz = Math.cos(rz), sz = Math.sin(rz);
  return [
    sx * cy * cz - cx * sy * sz,
    cx * sy * cz + sx * cy * sz,
    cx * cy * sz - sx * sy * cz,
    cx * cy * cz + sx * sy * sz,
  ];
}

function TransformPanel({ obj }: { obj: ObjectInfo }) {
  const [uniform, setUniform] = useState(true);
  const t = obj.transform;
  const size = [0, 1, 2].map((i) => obj.bounds.max[i] - obj.bounds.min[i]);
  const euler = quatToEuler(t.rotation);
  const commit = (nt: Transform, keep: boolean) =>
    void run(null, async () => {
      const o = await api.setTransform(obj.id, nt, keep);
      applyObject(o);
      if (o.support_points === 0) sceneRef.current?.setSupports(obj.id, null);
    });
  const axes = ["X", "Y", "Z"] as const;
  return (
    <section className="panel">
      <header className="panel-header">
        <h3>{tr("Position, rotation & size")}</h3>
      </header>
      <div className="grid3">
        {axes.map((a, i) => (
          <NumberField
            key={a}
            label={i === 2 ? tr("Z (lift)") : a}
            unit="mm"
            step={1}
            digits={2}
            value={i === 2 ? obj.bounds.min[2] : t.translation[i]}
            onChange={(v) => {
              const pos = [...t.translation] as [number, number, number];
              if (i === 2) pos[2] += v - obj.bounds.min[2];
              else pos[i] = v;
              commit({ ...t, translation: pos }, false);
            }}
          />
        ))}
        {axes.map((a, i) => (
          <NumberField
            key={a}
            label={tr("Rotate {axis}", { axis: a })}
            unit="°"
            step={15}
            digits={1}
            value={euler[i]}
            onChange={(v) => {
              const e = [...euler] as [number, number, number];
              e[i] = v;
              commit({ ...t, rotation: eulerToQuat(e) }, true);
            }}
          />
        ))}
        {axes.map((a, i) => (
          <NumberField
            key={a}
            label={tr("Size {axis}", { axis: a })}
            unit="mm"
            step={1}
            digits={2}
            min={0.01}
            value={size[i]}
            onChange={(v) => {
              const f = v / size[i];
              const sc = [...t.scale] as [number, number, number];
              if (uniform) for (let k = 0; k < 3; k++) sc[k] *= f;
              else sc[i] *= f;
              commit({ ...t, scale: sc }, true);
            }}
          />
        ))}
      </div>
      <div className="row wrap">
        <Toggle label={tr("Uniform")} checked={uniform} onChange={setUniform} />
        <span className="spacer" />
        <Button small onClick={() => commit({ ...t, scale: t.scale.map((v) => Math.sign(v) || 1) as [number, number, number] }, true)}>
          100 %
        </Button>
        <Button small onClick={() => commit({ ...t, rotation: [0, 0, 0, 1] }, true)}>
          {tr("Reset rotation")}
        </Button>
        <Button small onClick={() => commit({ ...t, translation: [0, 0, t.translation[2]] }, false)}>
          {tr("Center")}
        </Button>
      </div>
      <div className="row wrap">
        {[-90, 90].map((deg) =>
          axes.map((a, i) => (
            <Button
              key={`${a}${deg}`}
              small
              onClick={() => {
                const e = [...euler] as [number, number, number];
                e[i] += deg;
                commit({ ...t, rotation: eulerToQuat(e) }, true);
              }}
            >
              {a} {deg > 0 ? "+" : ""}
              {deg}°
            </Button>
          )),
        )}
      </div>
      <p className="muted small">
        {tr("{triangles} triangles · {volume} ml", { triangles: obj.triangles.toLocaleString(), volume: fmtNum(obj.volume_mm3 / 1000, 2) })}
      </p>
    </section>
  );
}

function CopyPanel() {
  const [count, setCount] = useState(1);
  const [spacing, setSpacing] = useState(3);
  return (
    <section className="panel">
      <header className="panel-header">
        <h3>{tr("Copy")}</h3>
      </header>
      <div className="grid2">
        <NumberField label={tr("Copies")} step={1} min={1} max={100} value={count} onChange={(v) => setCount(Math.round(v))} />
        <NumberField label={tr("Spacing")} unit="mm" step={0.5} min={0} value={spacing} onChange={setSpacing} />
      </div>
      <div className="row">
        <Button variant="primary" icon="copy" onClick={() => void copies(count)}>
          {tr("Copy")}
        </Button>
        <Button icon="fill" onClick={() => void fillPlate(spacing)} title={tr("Add copies until the build plate is full")}>
          {tr("Fill plate")}
        </Button>
      </div>
      <p className="muted small">{tr("Copies are arranged automatically. “Fill plate” adds as many copies as fit.")}</p>
    </section>
  );
}

function MirrorPanel() {
  return (
    <section className="panel">
      <header className="panel-header">
        <h3>{tr("Mirror")}</h3>
      </header>
      <div className="row">
        <Button onClick={() => void mirror(0)}>{tr("Mirror X")}</Button>
        <Button onClick={() => void mirror(1)}>{tr("Mirror Y")}</Button>
        <Button onClick={() => void mirror(2)}>{tr("Mirror Z")}</Button>
      </div>
    </section>
  );
}

function AlignPanel() {
  const modes: [Parameters<typeof align>[0], string][] = [
    ["left", "Left"],
    ["center_x", "Center X"],
    ["right", "Right"],
    ["front", "Front"],
    ["center_y", "Center Y"],
    ["back", "Back"],
    ["plate_center", "Plate center"],
    ["drop", "Drop to plate"],
  ];
  return (
    <section className="panel">
      <header className="panel-header">
        <h3>{tr("Align")}</h3>
      </header>
      <div className="grid3">
        {modes.map(([m, l]) => (
          <Button key={m} small onClick={() => void align(m)}>
            {tr(l)}
          </Button>
        ))}
      </div>
      <p className="muted small">{tr("Aligns the selected models (or all) to their common bounds.")}</p>
    </section>
  );
}

function LayoutPanel() {
  return (
    <section className="panel">
      <header className="panel-header">
        <h3>{tr("Auto layout")}</h3>
      </header>
      <p className="muted small">{tr("Packs all models on the plate with a 3 mm gap and centers the group.")}</p>
      <Button variant="primary" icon="layout" onClick={() => void arrangeAll()}>
        {tr("Arrange all")}
      </Button>
    </section>
  );
}

function SupportPanel({ raftOnly }: { raftOnly: boolean }) {
  const config = useStore((s) => s.config);
  const setConfig = useStore((s) => s.setConfig);
  const selection = useStore((s) => s.selection);
  const objects = useStore((s) => s.objects);
  if (!config) return null;
  const ss = config.support_settings;
  const set = (patch: Partial<SupportSettings>) => {
    const next = { ...ss, ...patch };
    setConfig({ ...config, support_settings: next });
    void api.setSupportSettings(next);
  };
  const targets = selection.length ? selection : objects.map((o) => o.id);
  const sizes = ["light", "medium", "heavy"] as const;
  const si = sizes.indexOf(ss.default_size);
  return (
    <section className="panel">
      <header className="panel-header">
        <h3>{tr(raftOnly ? "Raft" : "Supports")}</h3>
      </header>
      {!raftOnly && (
        <>
          <div className="segmented">
            {sizes.map((s) => (
              <button key={s} className={ss.default_size === s ? "on" : ""} onClick={() => set({ default_size: s })}>
                {tr(s)}
              </button>
            ))}
          </div>
          <div className="grid2">
            <NumberField label={tr("Lift height")} unit="mm" step={0.5} min={0} value={ss.lift_height} onChange={(v) => set({ lift_height: v })} />
            <NumberField label={tr("Density (spacing)")} unit="mm" step={0.5} min={0.5} value={ss.spacing} onChange={(v) => set({ spacing: v })} />
            <NumberField label={tr("Overhang angle")} unit="°" step={5} min={0} max={89} value={ss.overhang_angle} onChange={(v) => set({ overhang_angle: v })} />
            <NumberField label={tr("Tip length")} unit="mm" step={0.5} min={0.5} value={ss.tip_length} onChange={(v) => set({ tip_length: v })} />
            <NumberField
              label={tr("Contact Ø")}
              unit="mm"
              step={0.05}
              min={0.1}
              value={ss.tip_diameter[si]}
              onChange={(v) => {
                const d = [...ss.tip_diameter] as [number, number, number];
                d[si] = v;
                set({ tip_diameter: d });
              }}
            />
            <NumberField
              label={tr("Shaft Ø")}
              unit="mm"
              step={0.1}
              min={0.2}
              value={ss.shaft_diameter[si]}
              onChange={(v) => {
                const d = [...ss.shaft_diameter] as [number, number, number];
                d[si] = v;
                set({ shaft_diameter: d });
              }}
            />
          </div>
          <Toggle label={tr("Allow supports on the model")} checked={ss.model_contacts} onChange={(v) => set({ model_contacts: v })} />
          <Toggle label={tr("Cross bracing")} checked={!!ss.cross_bracing} onChange={(v) => set({ cross_bracing: v })} />
          {ss.cross_bracing && (
            <div className="grid2">
              <NumberField label={tr("Brace distance")} unit="mm" step={1} min={2} max={40} value={ss.bracing_max_distance ?? 10} onChange={(v) => set({ bracing_max_distance: v })} />
              <NumberField label={tr("Brace Ø")} unit="mm" step={0.1} min={0.3} max={3} value={ss.bracing_diameter ?? 0.8} onChange={(v) => set({ bracing_diameter: v })} />
            </div>
          )}
          <Toggle label={tr("Detect islands")} checked={ss.detect_islands} onChange={(v) => set({ detect_islands: v })} />
        </>
      )}
      <Toggle label={tr("Raft")} checked={ss.raft} onChange={(v) => set({ raft: v })} />
      {ss.raft && (
        <div className="grid2">
          <NumberField label={tr("Raft thickness")} unit="mm" step={0.1} min={0.2} value={ss.raft_thickness} onChange={(v) => set({ raft_thickness: v })} />
          <NumberField label={tr("Raft margin")} unit="mm" step={0.5} min={0} value={ss.raft_margin} onChange={(v) => set({ raft_margin: v })} />
          <NumberField label={tr("Foot Ø")} unit="mm" step={0.5} min={0.5} value={ss.base_diameter} onChange={(v) => set({ base_diameter: v })} />
          <NumberField label={tr("Foot height")} unit="mm" step={0.1} min={0.1} value={ss.base_height} onChange={(v) => set({ base_height: v })} />
        </div>
      )}
      <div className="row">
        <Button variant="primary" icon="auto_support" disabled={!targets.length} onClick={() => void autoSupports(targets)}>
          {tr(raftOnly ? "Rebuild supports" : "Auto supports")}
        </Button>
        <Button icon="trash" disabled={!targets.length} onClick={() => void run(null, async () => { await clearSupports(targets); await refreshHistory(); })}>
          {tr("Clear")}
        </Button>
      </div>
      <p className="muted small">{tr("Applies to the selected models, or to all models when nothing is selected.")}</p>
    </section>
  );
}

function SlicePanel() {
  const config = useStore((s) => s.config);
  const progress = useStore((s) => s.sliceProgress);
  const objects = useStore((s) => s.objects);
  const devices = useStore((s) => s.devices);
  const ready = Object.values(devices).find(
    (d) => d.connected && !["printing", "paused", "pausing", "file_transfer"].includes(d.status?.status.state ?? ""),
  );
  const [format, setFormat] = useState<string | null>(null);
  if (!config) return null;
  const s = config.settings;
  const p = config.printer;
  const formats = config.formats.filter((f) => p.formats.includes(f.extension));
  const others = config.formats.filter((f) => !p.formats.includes(f.extension));
  return (
    <section className="panel slice-panel">
      <header className="panel-header">
        <h3>{tr("Print settings")}</h3>
        <Button icon="settings" small variant="ghost" title={tr("Edit settings")} onClick={() => window.dispatchEvent(new Event("open-settings"))} />
      </header>
      <dl className="kv">
        <dt>{tr("Resin")}</dt>
        <dd>{s.name}</dd>
        <dt>{tr("Layer height")}</dt>
        <dd>{s.layer_height} mm</dd>
        <dt>{tr("Exposure")}</dt>
        <dd>
          {tr("{exposure} s · bottom {bottom} s × {layers}", {
            exposure: s.normal.exposure,
            bottom: s.bottom.exposure,
            layers: s.bottom_layers,
          })}
        </dd>
        <dt>{tr("Anti-aliasing")}</dt>
        <dd>{s.anti_aliasing > 1 ? `${s.anti_aliasing}×` : tr("off")}</dd>
      </dl>
      <label className="field">
        <span className="field-label">{tr("Output format")}</span>
        <select value={format ?? ""} onChange={(e) => setFormat(e.target.value || null)}>
          <option value="">{tr("Printer default ({format})", { format: p.formats[0] ?? "zip" })}</option>
          {formats.map((f) => (
            <option key={f.id} value={f.id}>
              {f.description} (.{f.extension})
            </option>
          ))}
          {others.length > 0 && (
            <optgroup label={tr("Other formats")}>
              {others.map((f) => (
                <option key={f.id} value={f.id}>
                  {f.description} (.{f.extension})
                </option>
              ))}
            </optgroup>
          )}
        </select>
      </label>
      {progress ? (
        <div className="slice-progress">
          <Progress value={progress.done / Math.max(1, progress.total)} label={tr("Slicing {done}/{total}", { done: progress.done, total: progress.total })} />
          <Button small variant="danger" onClick={() => void api.cancelSlice()}>
            {tr("Cancel")}
          </Button>
        </div>
      ) : (
        <div className="column">
          <Button variant="primary" icon="slice" disabled={!objects.length} onClick={() => void sliceNow(format)}>
            {tr("Slice")}
          </Button>
          {ready && (
            <Button
              icon="wifi"
              disabled={!objects.length}
              title={`${tr("Slice and send to")} ${ready.name}`}
              onClick={async () => {
                // only send a fresh result (a failed re-slice must not send the previous one)
                if (await sliceNow(format)) useStore.getState().setSendTo(ready.ip);
              }}
            >
              {tr("Slice & send to")} {ready.name}
            </Button>
          )}
        </div>
      )}
    </section>
  );
}

import { useEffect, useMemo, useState } from "react";
import { api, errorText, type ObjectInfo } from "../api";
import { refreshCutouts, run, sceneRef } from "../actions";
import { autoOrient, refreshHistory, syncScene } from "../editActions";
import { t as tr } from "../i18n";
import { useStore } from "../store";
import { Button, NumberField, Toggle } from "../ui";

export interface MeshReport {
  vertices: number;
  triangles: number;
  shells: number;
  boundary_edges: number;
  holes: number;
  non_manifold_edges: number;
  inconsistent_edges: number;
  degenerate_triangles: number;
  duplicate_triangles: number;
  inverted_shells: number;
  cavities: number;
  volume: number;
  watertight: boolean;
}

interface RepairOptions {
  weld_tolerance: number;
  remove_degenerate: boolean;
  remove_duplicates: boolean;
  fix_orientation: boolean;
  fill_holes: boolean;
  max_hole_edges: number;
  min_shell_volume: number;
}

const DEFAULT_OPTIONS: RepairOptions = {
  weld_tolerance: 0.0001,
  remove_degenerate: true,
  remove_duplicates: true,
  fix_orientation: true,
  fill_holes: true,
  max_hole_edges: 0,
  min_shell_volume: 0,
};

function useTargets(): ObjectInfo[] {
  const objects = useStore((s) => s.objects);
  const selection = useStore((s) => s.selection);
  return selection.length ? objects.filter((o) => selection.includes(o.id)) : objects;
}

function issues(r: MeshReport): number {
  return (
    r.holes +
    r.non_manifold_edges +
    r.inconsistent_edges +
    r.degenerate_triangles +
    r.duplicate_triangles +
    r.inverted_shells
  );
}

/** Error detection and model repair (Repair tab). */
export function RepairPanel() {
  const targets = useTargets();
  const toast = useStore((s) => s.toast);
  const [reports, setReports] = useState<Record<number, MeshReport>>({});
  const [opts, setOpts] = useState<RepairOptions>(DEFAULT_OPTIONS);
  const [busy, setBusy] = useState(false);
  const key = targets.map((o) => `${o.id}:${o.mesh_key}`).join(",");

  useEffect(() => {
    let cancelled = false;
    (async () => {
      const out: Record<number, MeshReport> = {};
      for (const o of targets) {
        try {
          out[o.id] = (await api.analyzeObject(o.id)) as MeshReport;
        } catch {
          /* ignore */
        }
      }
      if (!cancelled) setReports(out);
    })();
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key]);

  const repairNow = async () => {
    setBusy(true);
    try {
      const [results, objs] = await api.repairObjects(
        targets.map((o) => o.id),
        opts,
      );
      await syncScene(objs);
      for (const r of results) {
        const rr = r.report as Record<string, number> & { after: MeshReport };
        const fixed = [
          rr.holes_filled ? `${rr.holes_filled} ${tr("holes filled")}` : "",
          rr.triangles_flipped ? `${rr.triangles_flipped} ${tr("triangles flipped")}` : "",
          rr.duplicates_removed ? `${rr.duplicates_removed} ${tr("duplicates removed")}` : "",
          rr.degenerate_removed ? `${rr.degenerate_removed} ${tr("degenerate removed")}` : "",
          rr.vertices_welded ? `${rr.vertices_welded} ${tr("vertices welded")}` : "",
        ].filter(Boolean);
        toast(
          rr.after.watertight ? "success" : "info",
          `${r.name}: ${fixed.length ? fixed.join(", ") : tr("nothing to repair")}${rr.after.watertight ? "" : ` – ${tr("still not watertight")}`}`,
        );
      }
    } catch (e) {
      toast("error", errorText(e));
    } finally {
      setBusy(false);
    }
  };

  const rows: [keyof MeshReport, string][] = [
    ["triangles", "Triangles"],
    ["shells", "Parts (shells)"],
    ["holes", "Holes"],
    ["boundary_edges", "Open edges"],
    ["non_manifold_edges", "Non-manifold edges"],
    ["inconsistent_edges", "Flipped normals (edges)"],
    ["inverted_shells", "Inside-out parts"],
    ["degenerate_triangles", "Degenerate triangles"],
    ["duplicate_triangles", "Duplicate triangles"],
    ["cavities", "Internal cavities"],
  ];

  return (
    <section className="panel">
      <header className="panel-header">
        <h3>{tr("Error Detection")}</h3>
      </header>
      {targets.length === 0 && <p className="muted small">{tr("No models on the plate.")}</p>}
      {targets.map((o) => {
        const r = reports[o.id];
        return (
          <div key={o.id} className="report">
            <div className="row">
              <strong className="name">{o.name}</strong>
              <span className="spacer" />
              {r && (
                <span className={`badge ${issues(r) === 0 && r.watertight ? "net" : "warn"}`}>
                  {issues(r) === 0 && r.watertight ? tr("OK") : `${issues(r)} ${tr("issues")}`}
                </span>
              )}
            </div>
            {r ? (
              <dl className="kv compact">
                {rows.map(([k, label]) => (
                  <div key={k} className="contents">
                    <dt>{tr(label)}</dt>
                    <dd className={k !== "triangles" && k !== "shells" && k !== "cavities" && Number(r[k]) > 0 ? "bad" : ""}>
                      {Number(r[k]).toLocaleString()}
                    </dd>
                  </div>
                ))}
                <dt>{tr("Watertight")}</dt>
                <dd className={r.watertight ? "" : "bad"}>{r.watertight ? tr("yes") : tr("no")}</dd>
              </dl>
            ) : (
              <p className="muted small">{tr("Analysing…")}</p>
            )}
          </div>
        );
      })}
      <h4>{tr("Model Repair")}</h4>
      <Toggle label={tr("Fill holes")} checked={opts.fill_holes} onChange={(v) => setOpts({ ...opts, fill_holes: v })} />
      <Toggle label={tr("Fix flipped normals")} checked={opts.fix_orientation} onChange={(v) => setOpts({ ...opts, fix_orientation: v })} />
      <Toggle label={tr("Remove duplicate triangles")} checked={opts.remove_duplicates} onChange={(v) => setOpts({ ...opts, remove_duplicates: v })} />
      <Toggle label={tr("Remove degenerate triangles")} checked={opts.remove_degenerate} onChange={(v) => setOpts({ ...opts, remove_degenerate: v })} />
      <div className="grid2">
        <NumberField label={tr("Weld tolerance")} unit="mm" step={0.0001} min={0} max={0.5} value={opts.weld_tolerance} onChange={(v) => setOpts({ ...opts, weld_tolerance: v })} />
        <NumberField label={tr("Remove parts below")} unit="mm³" step={1} min={0} value={opts.min_shell_volume} onChange={(v) => setOpts({ ...opts, min_shell_volume: v })} />
      </div>
      <Button variant="primary" icon="repair" disabled={!targets.length || busy} onClick={() => void repairNow()}>
        {busy ? tr("Repairing…") : tr("Repair")}
      </Button>
      <p className="muted small">{tr("Applies to the selected models, or to all models when nothing is selected.")}</p>
    </section>
  );
}

/** Plane cut (Prepare tab). */
export function CutPanel() {
  const objects = useStore((s) => s.objects);
  const selection = useStore((s) => s.selection);
  const obj = objects.find((o) => o.id === selection[0]);
  const [axis, setAxis] = useState<"x" | "y" | "z">("z");
  const i = axis === "x" ? 0 : axis === "y" ? 1 : 2;
  const range = useMemo(() => (obj ? [obj.bounds.min[i], obj.bounds.max[i]] : [0, 0]), [obj, i]);
  const [pos, setPos] = useState(0);

  useEffect(() => {
    setPos(+((range[0] + range[1]) / 2).toFixed(2));
  }, [range]);

  useEffect(() => {
    sceneRef.current?.setCutPlane(obj ? { axis, position: pos, bounds: obj.bounds } : null);
    return () => sceneRef.current?.setCutPlane(null);
  }, [obj, axis, pos]);

  if (selection.length !== 1 || !obj) {
    return (
      <section className="panel">
        <header className="panel-header">
          <h3>{tr("Cut")}</h3>
        </header>
        <p className="muted small">{tr("Select one model to cut.")}</p>
      </section>
    );
  }
  return (
    <section className="panel">
      <header className="panel-header">
        <h3>{tr("Cut")}</h3>
      </header>
      <div className="segmented">
        {(["x", "y", "z"] as const).map((a) => (
          <button key={a} className={axis === a ? "on" : ""} onClick={() => setAxis(a)}>
            {a.toUpperCase()}
          </button>
        ))}
      </div>
      <input
        type="range"
        min={range[0]}
        max={range[1]}
        step={0.05}
        value={pos}
        onChange={(e) => setPos(+e.target.value)}
        aria-label={tr("Cut position")}
      />
      <NumberField label={tr("Cut position")} unit="mm" step={0.5} min={range[0]} max={range[1]} digits={2} value={pos} onChange={setPos} />
      <Button
        variant="primary"
        icon="cut"
        onClick={() =>
          void run(tr("Cutting…"), async () => {
            await syncScene(await api.cutObject(obj.id, axis, pos));
            useStore.getState().select([]);
            useStore.getState().setPanel(null);
          })
        }
      >
        {tr("Cut")}
      </Button>
      <p className="muted small">{tr("Both halves are closed (capped) and placed on the plate.")}</p>
    </section>
  );
}

/** Hollowing and drain holes (Prepare tab). */
export function HollowPanel() {
  const objects = useStore((s) => s.objects);
  const selection = useStore((s) => s.selection);
  const tool = useStore((s) => s.tool);
  const setTool = useStore((s) => s.setTool);
  const drill = useStore((s) => s.drill);
  const setDrill = useStore((s) => s.setDrill);
  const toast = useStore((s) => s.toast);
  const [wall, setWall] = useState(2);
  const [precision, setPrecision] = useState<"fast" | "normal" | "fine">("normal");
  const targets = selection.length ? objects.filter((o) => selection.includes(o.id)) : objects;
  const voxel = { fast: wall / 3, normal: 0, fine: wall / 8 }[precision];

  const hollow = () =>
    void run(tr("Hollowing…"), async () => {
      let total = 0;
      for (const o of targets) {
        const [vol, info] = await api.hollowObject(o.id, { wall_thickness: wall, voxel_size: voxel });
        total += vol;
        useStore.getState().upsertObjects([info]);
        await refreshCutouts(o.id);
      }
      await refreshHistory();
      toast("success", `${tr("Hollowed")} – ${tr("saves")} ${(total / 1000).toFixed(1)} ml ${tr("resin")}`);
      setDrill({ ...drill, depth: Math.max(drill.depth, wall + 1.5) });
    });

  return (
    <section className="panel">
      <header className="panel-header">
        <h3>{tr("Hollow")}</h3>
      </header>
      <div className="grid2">
        <NumberField label={tr("Wall thickness")} unit="mm" step={0.5} min={0.8} max={10} value={wall} onChange={setWall} />
        <label className="field">
          <span className="field-label">{tr("Precision")}</span>
          <select value={precision} onChange={(e) => setPrecision(e.target.value as typeof precision)}>
            <option value="fast">{tr("Fast")}</option>
            <option value="normal">{tr("Normal")}</option>
            <option value="fine">{tr("Fine (slow)")}</option>
          </select>
        </label>
      </div>
      <div className="row">
        <Button variant="primary" icon="hollow" disabled={!targets.length} onClick={hollow}>
          {tr("Hollow")}
        </Button>
        <Button
          icon="trash"
          disabled={!targets.some((o) => o.cutouts > 0)}
          onClick={() =>
            void run(null, async () => {
              for (const o of targets) {
                const info = await api.clearCutouts(o.id);
                useStore.getState().upsertObjects([info]);
                await refreshCutouts(o.id);
              }
              await refreshHistory();
            })
          }
        >
          {tr("Remove")}
        </Button>
      </div>
      <h4>{tr("Drain holes")}</h4>
      <p className="muted small">{tr("Hollow prints need at least one hole near the plate so trapped resin can drain and suction is avoided.")}</p>
      <div className="grid2">
        <NumberField label={tr("Diameter")} unit="mm" step={0.5} min={0.5} max={20} value={drill.diameter} onChange={(v) => setDrill({ ...drill, diameter: v })} />
        <NumberField label={tr("Depth")} unit="mm" step={0.5} min={0.5} max={50} value={drill.depth} onChange={(v) => setDrill({ ...drill, depth: v })} />
      </div>
      <Button icon="drill" active={tool === "drill"} onClick={() => setTool(tool === "drill" ? "select" : "drill")}>
        {tool === "drill" ? tr("Click the model to drill… (Esc to finish)") : tr("Add drain holes")}
      </Button>
    </section>
  );
}

/** Auto orientation with a selectable goal. */
export function OrientPanel() {
  const [goal, setGoal] = useState<"balanced" | "fewest_supports" | "lowest_height">("balanced");
  const objects = useStore((s) => s.objects);
  const info: Record<typeof goal, string> = {
    balanced: "Tilts parts for supported printing: fewer suction forces and cleaner surfaces. Best for most models.",
    fewest_supports: "Puts the largest flat area on the plate so as little as possible needs support.",
    lowest_height: "Makes the print as low (fast) as possible.",
  };
  return (
    <section className="panel">
      <header className="panel-header">
        <h3>{tr("Auto Orient")}</h3>
      </header>
      <div className="choice-list">
        {(["balanced", "fewest_supports", "lowest_height"] as const).map((g) => (
          <label key={g} className={`choice ${goal === g ? "on" : ""}`}>
            <input type="radio" name="goal" checked={goal === g} onChange={() => setGoal(g)} />
            <span>
              <strong>{tr(g === "balanced" ? "Balanced" : g === "fewest_supports" ? "Fewest supports" : "Lowest height")}</strong>
              <span className="muted small">{tr(info[g])}</span>
            </span>
          </label>
        ))}
      </div>
      <Button variant="primary" icon="orient" disabled={!objects.length} onClick={() => void autoOrient(goal)}>
        {tr("Orient")}
      </Button>
      <p className="muted small">{tr("Applies to the selected models, or to all models when nothing is selected.")}</p>
    </section>
  );
}

/** Embossed or engraved text placed by clicking on a model. */
export function LabelPanel() {
  const label = useStore((s) => s.label);
  const setLabel = useStore((s) => s.setLabel);
  const tool = useStore((s) => s.tool);
  const setTool = useStore((s) => s.setTool);
  return (
    <section className="panel">
      <header className="panel-header">
        <h3>{tr("Add Label")}</h3>
      </header>
      <label className="field">
        <span className="field-label">{tr("Text")}</span>
        <input value={label.text} maxLength={40} onChange={(e) => setLabel({ ...label, text: e.target.value })} />
      </label>
      <div className="segmented">
        {(["emboss", "engrave"] as const).map((m) => (
          <button key={m} className={label.mode === m ? "on" : ""} onClick={() => setLabel({ ...label, mode: m })}>
            {tr(m === "emboss" ? "Raised" : "Engraved")}
          </button>
        ))}
      </div>
      <div className="grid2">
        <NumberField label={tr("Letter height")} unit="mm" step={0.5} min={1} max={50} value={label.height} onChange={(v) => setLabel({ ...label, height: v })} />
        <NumberField label={tr("Depth")} unit="mm" step={0.1} min={0.2} max={10} value={label.depth} onChange={(v) => setLabel({ ...label, depth: v })} />
      </div>
      <Button variant="primary" icon="label" active={tool === "label"} onClick={() => setTool(tool === "label" ? "select" : "label")}>
        {tool === "label" ? tr("Click the model to place the text… (Esc to finish)") : tr("Place on model")}
      </Button>
    </section>
  );
}

/** Solid pad under the model instead of supports. */
export function PlatformPanel() {
  const objects = useStore((s) => s.objects);
  const selection = useStore((s) => s.selection);
  const [opts, setOpts] = useState({ margin: 2, thickness: 1.5, chamfer: 1 });
  const targets = selection.length ? selection : objects.map((o) => o.id);
  return (
    <section className="panel">
      <header className="panel-header">
        <h3>{tr("Add Platform")}</h3>
      </header>
      <p className="muted small">{tr("Puts the model on a solid pad that follows its footprint – useful for flat-bottomed parts printed without supports.")}</p>
      <div className="grid3">
        <NumberField label={tr("Thickness")} unit="mm" step={0.5} min={0.5} max={10} value={opts.thickness} onChange={(v) => setOpts({ ...opts, thickness: v })} />
        <NumberField label={tr("Margin")} unit="mm" step={0.5} min={0} max={20} value={opts.margin} onChange={(v) => setOpts({ ...opts, margin: v })} />
        <NumberField label={tr("Chamfer")} unit="mm" step={0.5} min={0} max={10} value={opts.chamfer} onChange={(v) => setOpts({ ...opts, chamfer: v })} />
      </div>
      <Button
        variant="primary"
        icon="platform"
        disabled={!targets.length}
        onClick={() =>
          void run(tr("Adding platform…"), async () => {
            for (const id of targets) await api.addPlatform(id, opts);
            await syncScene();
          })
        }
      >
        {tr("Add Platform")}
      </Button>
    </section>
  );
}

/** Boolean difference: selected models become cutters. */
export function BooleanPanel() {
  const objects = useStore((s) => s.objects);
  const selection = useStore((s) => s.selection);
  const sel = objects.filter((o) => selection.includes(o.id));
  const apply = (subtract: boolean) =>
    void run(null, async () => {
      await syncScene(await api.setSubtract(selection, subtract));
    });
  return (
    <section className="panel">
      <header className="panel-header">
        <h3>{tr("Boolean Operation")}</h3>
      </header>
      <p className="muted small">
        {tr("Models marked as cutter are removed from every model they overlap when slicing (difference). Overlapping solid models are always merged (union).")}
      </p>
      {sel.length === 0 ? (
        <p className="muted small">{tr("Select the model(s) to use as cutter.")}</p>
      ) : (
        <div className="row wrap">
          <Button variant="primary" icon="boolean" onClick={() => apply(true)} disabled={sel.every((o) => o.subtract)}>
            {tr("Use as cutter (subtract)")}
          </Button>
          <Button onClick={() => apply(false)} disabled={sel.every((o) => !o.subtract)}>
            {tr("Make solid again")}
          </Button>
        </div>
      )}
    </section>
  );
}

export async function splitSelected() {
  const s = useStore.getState();
  if (s.selection.length !== 1) return s.toast("info", tr("Select one model to split"));
  await run(tr("Splitting…"), async () => {
    const [n, objs] = await api.splitObject(s.selection[0]);
    await syncScene(objs);
    useStore.getState().toast(n > 1 ? "success" : "info", n > 1 ? `${tr("Split into")} ${n} ${tr("parts")}` : tr("The model has only one part"));
  });
}

/** Checks freshly imported models and points out problems. */
export async function checkImported(ids: number[]) {
  for (const id of ids) {
    try {
      const r = (await api.analyzeObject(id)) as MeshReport;
      const n = issues(r);
      if (n > 0 || !r.watertight) {
        const o = useStore.getState().objects.find((x) => x.id === id);
        useStore
          .getState()
          .toast("info", `${o?.name ?? tr("Model")}: ${n} ${tr("mesh issues found – use Repair › Model Repair")}`);
      }
    } catch {
      /* ignore */
    }
  }
}

import { fmtNum } from "../format";
import { t as tr } from "../i18n";
import { useStore } from "../store";

/** Bottom status line: models, selection size, plate usage, printer facts. */
export function StatusBar() {
  const objects = useStore((s) => s.objects);
  const selection = useStore((s) => s.selection);
  const config = useStore((s) => s.config);
  const p = config?.printer;
  const sel = objects.filter((o) => selection.includes(o.id));
  const bounds = (list: typeof objects) => {
    if (!list.length) return null;
    const min = [Infinity, Infinity, Infinity];
    const max = [-Infinity, -Infinity, -Infinity];
    for (const o of list)
      for (let i = 0; i < 3; i++) {
        min[i] = Math.min(min[i], o.bounds.min[i]);
        max[i] = Math.max(max[i], o.bounds.max[i]);
      }
    return [max[0] - min[0], max[1] - min[1], max[2] - min[2]];
  };
  const sb = bounds(sel);
  const footprint = objects
    .filter((o) => !o.subtract)
    .reduce((a, o) => a + (o.bounds.max[0] - o.bounds.min[0]) * (o.bounds.max[1] - o.bounds.min[1]), 0);
  const usage = p ? Math.min(100, (footprint / (p.size_x * p.size_y)) * 100) : 0;
  const tallest = objects.reduce((a, o) => Math.max(a, o.bounds.max[2]), 0);
  const volume = objects.filter((o) => !o.subtract).reduce((a, o) => a + o.volume_mm3, 0);
  return (
    <footer className="statusbar">
      <span>
        {objects.length} {tr(objects.length === 1 ? "model" : "models")}
        {objects.length > 0 && ` · ≈${fmtNum(volume / 1000, 1)} ml`}
      </span>
      {sb && (
        <span>
          {sel.length} {tr("selected")}: {fmtNum(sb[0], 1)} × {fmtNum(sb[1], 1)} × {fmtNum(sb[2], 1)} mm
        </span>
      )}
      {objects.length > 0 && p && (
        <span title={tr("Share of the plate covered by model footprints")}>
          {tr("Plate")} {fmtNum(usage, 0)} % · {tr("height")} {fmtNum(tallest, 1)} / {fmtNum(p.size_z, 0)} mm
        </span>
      )}
      <span className="spacer" />
      {p && config && (
        <span className="muted">
          {p.name} · {p.resolution_x}×{p.resolution_y} · {fmtNum((p.size_x / p.resolution_x) * 1000, 1)} µm ·{" "}
          {config.settings.layer_height} mm
        </span>
      )}
    </footer>
  );
}

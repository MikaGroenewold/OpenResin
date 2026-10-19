import { useEffect, useMemo, useRef, useState } from "react";
import { api, errorText, type SliceSummary } from "../api";
import { saveSliced } from "../actions";
import { fmtBytes, fmtDuration, fmtNum } from "../format";
import { useStore } from "../store";
import { Button, Icon } from "../ui";
import { SendDialog } from "./SendDialog";
import { t as tr } from "../i18n";

// Layer images of the current slice result. A new result (even with the same name
// and file size, e.g. after moving the model) must never show old images.
const cache = new Map<string, string>();
let cacheOwner: SliceSummary | null = null;

/** Layer-by-layer preview of the sliced file. */
export function PreviewView() {
  const slice = useStore((s) => s.slice);
  const layer = useStore((s) => s.layer);
  const setLayer = useStore((s) => s.setLayer);
  const setMode = useStore((s) => s.setMode);
  const printer = useStore((s) => s.config?.printer);
  const [url, setUrl] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [zoom, setZoom] = useState(1);
  const [pan, setPan] = useState<{ x: number; y: number }>({ x: 0, y: 0 });
  const sendTo = useStore((s) => s.sendTo);
  const setSendTo = useStore((s) => s.setSendTo);
  const drag = useRef<{ x: number; y: number; px: number; py: number } | null>(null);
  const maxSize = zoom > 2 ? 0 : 2048;

  useEffect(() => {
    if (!slice) return;
    if (cacheOwner !== slice) {
      for (const u of cache.values()) URL.revokeObjectURL(u);
      cache.clear();
      cacheOwner = slice;
    }
    let cancelled = false;
    const key = `${layer}:${maxSize}`;
    const cached = cache.get(key);
    if (cached) {
      setUrl(cached);
      return;
    }
    const t = setTimeout(() => {
      api
        .getLayerImage(layer, maxSize)
        .then((buf) => {
          if (cancelled) return;
          const u = URL.createObjectURL(new Blob([buf], { type: "image/png" }));
          cache.set(key, u);
          if (cache.size > 64) {
            const first = cache.keys().next().value as string;
            URL.revokeObjectURL(cache.get(first)!);
            cache.delete(first);
          }
          setUrl(u);
          setError(null);
        })
        .catch((e) => !cancelled && setError(errorText(e)));
    }, 30);
    return () => {
      cancelled = true;
      clearTimeout(t);
    };
  }, [slice, layer, maxSize]);

  useEffect(() => {
    const k = (e: KeyboardEvent) => {
      if (!slice || (e.target as HTMLElement).tagName === "INPUT") return;
      if (e.key === "ArrowUp" || e.key === "PageUp") setLayer(Math.min(slice.layer_count - 1, layer + (e.key === "PageUp" ? 10 : 1)));
      if (e.key === "ArrowDown" || e.key === "PageDown") setLayer(Math.max(0, layer - (e.key === "PageDown" ? 10 : 1)));
    };
    window.addEventListener("keydown", k);
    return () => window.removeEventListener("keydown", k);
  }, [slice, layer, setLayer]);

  // Show the layer as seen from above, independent of how the LCD expects it.
  // Files flag LCD mirroring as "X mirror"; the standard LCD image is the top
  // view flipped vertically.
  const flip = useMemo(() => {
    if (!slice) return "";
    const i = slice.info;
    if (printer && i.machine_name && printer.mirror === "x") return "scale(-1, 1)";
    return i.mirror_x || i.mirror_y ? "scale(1, -1)" : "";
  }, [slice, printer]);

  if (!slice) {
    return (
      <div className="empty-view">
        <p>{tr("Nothing sliced yet.")}</p>
        <Button variant="primary" onClick={() => setMode("model")}>
          {tr("Back to prepare")}
        </Button>
      </div>
    );
  }
  const z = (layer + 1) * slice.info.layer_height;
  const area = slice.layer_areas[layer];
  const isBottom = layer < slice.info.bottom_layers;
  return (
    <div className="preview">
      <div
        className="layer-stage"
        onWheel={(e) => {
          // follow the wheel distance, so fast scrolling and touchpads zoom smoothly
          const px = e.deltaMode === 1 ? e.deltaY * 40 : e.deltaMode === 2 ? e.deltaY * 800 : e.deltaY;
          const z = Math.max(0.5, Math.min(40, zoom * Math.exp(-px * 0.0015)));
          const f = z / zoom;
          // keep the point under the cursor in place
          const r = e.currentTarget.getBoundingClientRect();
          const cx = e.clientX - r.left - r.width / 2;
          const cy = e.clientY - r.top - r.height / 2;
          setPan({ x: cx - (cx - pan.x) * f, y: cy - (cy - pan.y) * f });
          setZoom(z);
        }}
        onPointerDown={(e) => {
          drag.current = { x: e.clientX, y: e.clientY, px: pan.x, py: pan.y };
          (e.target as HTMLElement).setPointerCapture(e.pointerId);
        }}
        onPointerMove={(e) => {
          const d = drag.current;
          if (d) setPan({ x: d.px + e.clientX - d.x, y: d.py + e.clientY - d.y });
        }}
        onPointerUp={() => (drag.current = null)}
        onDoubleClick={() => {
          setZoom(1);
          setPan({ x: 0, y: 0 });
        }}
      >
        <div
          className="layer-frame"
          style={{
            aspectRatio: `${slice.info.size_x || slice.info.resolution_x} / ${slice.info.size_y || slice.info.resolution_y}`,
            transform: `translate(${pan.x}px, ${pan.y}px) scale(${zoom})`,
          }}
        >
          {url && <img src={url} alt={tr("Layer {n}", { n: layer + 1 })} draggable={false} style={{ transform: flip }} />}
        </div>
        {error && <div className="viewport-hint error">{error}</div>}
        <div className="zoom-hint muted small">{tr("Scroll to zoom · drag to pan · double-click to reset")}</div>
      </div>

      <div className="layer-slider">
        <span className="small">{slice.layer_count}</span>
        <input
          type="range"
          min={0}
          max={slice.layer_count - 1}
          value={layer}
          onChange={(e) => setLayer(+e.target.value)}
          aria-label={tr("Layer")}
          className="vertical"
        />
        <span className="small">1</span>
      </div>

      <aside className="side-panel">
        <section className="panel">
          <header className="panel-header">
            <h3>{tr("Layer {n}", { n: layer + 1 })}</h3>
            <span className={`badge ${isBottom ? "warn" : ""}`}>{tr(isBottom ? "bottom" : "normal")}</span>
          </header>
          <dl className="kv">
            <dt>{tr("Height")}</dt>
            <dd>{fmtNum(z, 3)} mm</dd>
            {area != null && (
              <>
                <dt>{tr("Cured area")}</dt>
                <dd>{fmtNum(area, 1)} mm²</dd>
              </>
            )}
          </dl>
          <div className="row">
            <Button small icon="download" onClick={() => setLayer(0)}>
              {tr("First")}
            </Button>
            <Button small onClick={() => setLayer(Math.max(0, layer - 1))}>
              −1
            </Button>
            <Button small onClick={() => setLayer(Math.min(slice.layer_count - 1, layer + 1))}>
              +1
            </Button>
            <Button small icon="upload" onClick={() => setLayer(slice.layer_count - 1)}>
              {tr("Last")}
            </Button>
          </div>
        </section>
        <section className="panel">
          <header className="panel-header">
            <h3>{tr("Job")}</h3>
            <span className="badge">.{slice.extension}</span>
          </header>
          <dl className="kv">
            <dt>{tr("Print time")}</dt>
            <dd>{fmtDuration(slice.print_time_s)}</dd>
            <dt>{tr("Layers")}</dt>
            <dd>
              {slice.layer_count} × {slice.info.layer_height} mm
            </dd>
            <dt>{tr("Height")}</dt>
            <dd>{fmtNum(slice.height_mm, 2)} mm</dd>
            <dt>{tr("Resin")}</dt>
            <dd>
              {fmtNum(slice.volume_ml, 1)} ml
              {slice.weight_g > 0 && ` · ${fmtNum(slice.weight_g, 1)} g`}
            </dd>
            {slice.cost > 0 && (
              <>
                <dt>{tr("Cost")}</dt>
                <dd>
                  {fmtNum(slice.cost, 2)} {slice.currency}
                </dd>
              </>
            )}
            <dt>{tr("File size")}</dt>
            <dd>{fmtBytes(slice.file_size)}</dd>
            {slice.duration_ms > 0 && (
              <>
                <dt>{tr("Sliced in")}</dt>
                <dd>{fmtNum(slice.duration_ms / 1000, 1)} s</dd>
              </>
            )}
          </dl>
          <div className="column">
            <Button variant="primary" icon="wifi" onClick={() => setSendTo("")}>
              {tr("Send to printer")}
            </Button>
            <Button icon="save" onClick={() => void saveSliced()}>
              {tr("Save file…")}
            </Button>
          </div>
        </section>
        <section className="panel">
          <p className="muted small">
            <Icon name="eye" size={14} />{" "}
            {tr("The preview shows the plate from above. The file stores the image the way your printer's LCD expects it.")}
          </p>
        </section>
      </aside>
      {sendTo !== null && <SendDialog initial={sendTo || null} onClose={() => setSendTo(null)} />}
    </div>
  );
}

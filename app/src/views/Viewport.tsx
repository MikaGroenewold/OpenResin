import { useEffect, useRef } from "react";
import { api, errorText } from "../api";
import { addSupportAt, applyObject, refreshCutouts, refreshSupports, removeSupportNear, run, sceneRef } from "../actions";
import { moveObjects, refreshHistory } from "../editActions";
import { useStore } from "../store";
import { SceneView } from "../viewport/SceneView";

/** Hosts the three.js scene and keeps it in sync with the store. */
export function Viewport() {
  const host = useRef<HTMLDivElement>(null);
  const objects = useStore((s) => s.objects);
  const selection = useStore((s) => s.selection);
  const tool = useStore((s) => s.tool);
  const printer = useStore((s) => s.config?.printer);
  const theme = useStore((s) => s.config?.theme);
  const overhang = useStore((s) => s.overhang);
  const overhangAngle = useStore((s) => s.config?.support_settings.overhang_angle ?? 45);
  const measure = useStore((s) => s.measure);
  const mode = useStore((s) => s.mode);
  const layer = useStore((s) => s.layer);
  const slice = useStore((s) => s.slice);

  useEffect(() => {
    const view = new SceneView(host.current!, {
      onSelect: (id, additive) => {
        const s = useStore.getState();
        if (id == null) return s.select([]);
        if (additive) {
          s.select(s.selection.includes(id) ? s.selection.filter((x) => x !== id) : [...s.selection, id]);
        } else s.select([id]);
      },
      onTransformEnd: (id, t, mode) => {
        void run(null, async () => {
          const o = await api.setTransform(id, t, mode !== "translate");
          applyObject(o);
          if (o.support_points === 0) view.setSupports(id, null);
          else if (mode === "translate") await refreshSupports(id);
          if (o.cutouts > 0) await refreshCutouts(id);
        });
      },
      onMoveEnd: (moves) => void moveObjects(moves),
      onMeasurePoint: (p) => {
        const s = useStore.getState();
        const m = s.measure;
        if (!m || (m.a && m.b && m.b !== m.a)) s.setMeasure({ a: p, b: p });
        else s.setMeasure({ a: m.a, b: p });
      },
      onSurfaceClick: (id, p, n) => void run(null, () => addSupportAt(id, p, n)),
      onSupportClick: (id, p) => void run(null, () => removeSupportNear(id, p)),
      onLabelClick: (id, p, n) =>
        void run(null, async () => {
          const s = useStore.getState();
          const { text, ...opts } = s.label;
          const o = await api.addLabel(id, text, opts, p, n);
          if (opts.mode === "emboss") {
            const { syncScene } = await import("../editActions");
            await syncScene();
          } else {
            applyObject(o);
            await refreshCutouts(id);
          }
        }),
      onDrillClick: (id, p, n) =>
        void run(null, async () => {
          const s = useStore.getState();
          const o = await api.addDrainHole(id, p, n, s.drill.diameter, s.drill.depth);
          applyObject(o);
          await refreshCutouts(id);
        }),
      onFaceClick: (id, face) => {
        void api
          .layFlat(id, face)
          .then((o) => {
            applyObject(o);
            view.setSupports(id, null);
            void refreshHistory();
          })
          .catch((e) => useStore.getState().toast("error", errorText(e)))
          .finally(() => useStore.getState().setTool("select"));
      },
    });
    sceneRef.current = view;
    return () => {
      sceneRef.current = null;
      view.dispose();
    };
  }, []);

  useEffect(() => {
    if (printer) sceneRef.current?.setPrinter(printer);
  }, [printer, theme]);

  useEffect(() => {
    const v = sceneRef.current;
    if (!v) return;
    for (const o of objects) v.setObjectTransform(o.id, o.transform);
  }, [objects]);

  useEffect(() => sceneRef.current?.setSelection(selection), [selection]);
  useEffect(() => sceneRef.current?.setTool(tool), [tool]);
  useEffect(() => sceneRef.current?.setOverhang(overhang, overhangAngle), [overhang, overhangAngle]);
  useEffect(() => {
    const v = sceneRef.current;
    if (!v) return;
    v.setClip(mode === "preview" && slice ? (layer + 1) * slice.info.layer_height : null);
  }, [mode, layer, slice]);

  useEffect(() => {
    if (!measure) sceneRef.current?.setMeasure(null, null);
    else sceneRef.current?.setMeasure(measure.a, measure.b === measure.a ? null : measure.b);
  }, [measure]);

  return <div className="viewport" ref={host} />;
}

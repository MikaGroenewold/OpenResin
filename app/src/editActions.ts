// Plate editing actions (history aware).
import { api, type AlignMode, type ObjectInfo, type Transform } from "./api";
import { refreshCutouts, refreshSupports, run, sceneRef } from "./actions";
import { useStore } from "./store";
import { t as tr } from "./i18n";

const st = () => useStore.getState();
const loadedMesh = new Map<number, number>();

export async function refreshHistory() {
  try {
    st().setHistory(await api.historyState());
  } catch {
    /* ignore */
  }
}

/** Brings the 3D view and the store in line with the backend scene. */
export async function syncScene(objs?: ObjectInfo[]) {
  const list = objs ?? (await api.sceneObjects());
  const view = sceneRef.current;
  const ids = new Set(list.map((o) => o.id));
  for (const o of st().objects) {
    if (!ids.has(o.id)) {
      view?.removeObject(o.id);
      loadedMesh.delete(o.id);
    }
  }
  for (const o of list) {
    if (!view?.hasObject(o.id) || loadedMesh.get(o.id) !== o.mesh_key) {
      view?.setObjectGeometry(o.id, await api.getMesh(o.id));
      loadedMesh.set(o.id, o.mesh_key);
    }
    view?.setObjectTransform(o.id, o.transform);
    view?.setSubtract(o.id, o.subtract);
  }
  st().replaceObjects(list);
  for (const o of list) {
    if (o.support_points > 0 || (st().supportPoints[o.id]?.length ?? 0) > 0) await refreshSupports(o.id);
    await refreshCutouts(o.id);
  }
  await refreshHistory();
}

export function markLoaded(o: ObjectInfo) {
  loadedMesh.set(o.id, o.mesh_key);
}

export async function undo() {
  await run(null, async () => syncScene(await api.undo()));
}

export async function redo() {
  await run(null, async () => syncScene(await api.redo()));
}

export async function moveObjects(moves: { id: number; transform: Transform }[]) {
  await run(null, async () => {
    for (const m of moves) {
      const o = await api.setTransform(m.id, m.transform, false);
      st().upsertObjects([o]);
      sceneRef.current?.setObjectTransform(o.id, o.transform);
      if (o.support_points > 0) await refreshSupports(o.id);
      if (o.cutouts > 0) await refreshCutouts(o.id);
    }
    await refreshHistory();
  });
}

export async function nudge(dx: number, dy: number) {
  const s = st();
  const moves = s.objects
    .filter((o) => s.selection.includes(o.id))
    .map((o) => ({
      id: o.id,
      transform: {
        ...o.transform,
        translation: [o.transform.translation[0] + dx, o.transform.translation[1] + dy, o.transform.translation[2]] as [
          number,
          number,
          number,
        ],
      },
    }));
  if (moves.length) await moveObjects(moves);
}

export async function mirror(axis: 0 | 1 | 2) {
  const s = st();
  await run(null, async () => {
    for (const o of s.objects.filter((x) => s.selection.includes(x.id))) {
      const scale = [...o.transform.scale] as [number, number, number];
      scale[axis] = -scale[axis];
      const r = await api.setTransform(o.id, { ...o.transform, scale }, true);
      st().upsertObjects([r]);
      sceneRef.current?.setObjectTransform(r.id, r.transform);
      // X/Y mirroring keeps (mirrored) supports; Z mirroring removes them
      if (r.support_points > 0) await refreshSupports(r.id);
      else sceneRef.current?.setSupports(r.id, null);
    }
    await refreshHistory();
  });
}

export async function copies(count: number) {
  const s = st();
  if (s.selection.length !== 1) return s.toast("info", tr("Select one model to copy"));
  await run(tr("Copying…"), async () => {
    await api.duplicateObject(s.selection[0], count);
    await syncScene();
  });
}

export async function fillPlate(spacing: number) {
  const s = st();
  if (s.selection.length !== 1) return s.toast("info", tr("Select one model to fill the plate with"));
  await run(tr("Filling the plate…"), async () => {
    const [n, objs] = await api.fillPlate(s.selection[0], spacing);
    await syncScene(objs);
    st().toast("success", n ? tr(n === 1 ? "Added {n} copy" : "Added {n} copies", { n }) : tr("No room for more copies"));
  });
}

export async function arrangeAll() {
  await run(tr("Arranging…"), async () => {
    const [fits, objs] = await api.arrange();
    await syncScene(objs);
    if (!fits) st().toast("error", tr("Not everything fits on the plate"));
  });
}

export async function align(mode: AlignMode) {
  const s = st();
  const ids = s.selection.length ? s.selection : s.objects.map((o) => o.id);
  if (!ids.length) return;
  await run(null, async () => syncScene(await api.alignObjects(ids, mode)));
}

export async function combine() {
  const s = st();
  if (s.selection.length < 2) return s.toast("info", tr("Select at least two models (Ctrl+click)"));
  await run(tr("Combining…"), async () => {
    await syncScene(await api.combineObjects(s.selection));
    st().select([]);
  });
}

export async function autoOrient(goal: "balanced" | "fewest_supports" | "lowest_height" = "balanced") {
  const s = st();
  const ids = s.selection.length ? s.selection : s.objects.map((o) => o.id);
  if (!ids.length) return;
  await run(tr("Finding the best orientation…"), async () => {
    await syncScene(await api.autoOrient(ids, { goal }));
    st().toast("success", tr(ids.length === 1 ? "Oriented {n} model" : "Oriented {n} models", { n: ids.length }));
  });
}

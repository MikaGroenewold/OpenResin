// Viewport.tsx: what the 3D view reports (picks, gizmo drags, clicks with tools)
// reaches the store and the backend, and store changes reach the 3D view.
import { act, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import type { Transform } from "../api";
import { sceneRef } from "../actions";
import { useStore } from "../store";
import { ModelView } from "../views/ModelView";
import { FakeSceneView } from "./fakeScene";
import { backend, renderWithConfig, seed, toasts } from "./harness";

const st = () => useStore.getState();
const view = () => FakeSceneView.last!;
const cb = () => view().cb!;

async function setup(...names: string[]) {
  const objs = seed(...(names.length ? names : ["a", "b"]));
  const r = await renderWithConfig(<ModelView />);
  return { objs, ...r };
}

describe("Viewport", () => {
  it("creates the 3D view, shows the printer and disposes it on unmount", async () => {
    const { unmount } = await setup();
    expect(sceneRef.current).toBe(view());
    expect(view().printer?.id).toBe("elegoo-mars-5-ultra");
    unmount();
    expect(view().disposed).toBe(true);
    expect(sceneRef.current).toBeNull();
  });

  it("picking selects, Ctrl+pick toggles, empty space clears", async () => {
    const { objs } = await setup();
    const [a, b] = objs.map((o) => o.id);
    act(() => cb().onSelect(a, false));
    expect(st().selection).toEqual([a]);
    act(() => cb().onSelect(b, true));
    expect(st().selection).toEqual([a, b]);
    act(() => cb().onSelect(a, true));
    expect(st().selection).toEqual([b]);
    expect(view().selection).toEqual([b]);
    act(() => cb().onSelect(null, false));
    expect(st().selection).toEqual([]);
  });

  it("gizmo rotations keep the model on the plate, translations do not", async () => {
    const { objs } = await setup();
    const t: Transform = { translation: [3, 4, 2], rotation: [0, 0, Math.SQRT1_2, Math.SQRT1_2], scale: [1, 1, 1] };
    await act(async () => cb().onTransformEnd(objs[0].id, t, "rotate"));
    await waitFor(() => expect(backend.lastCall("set_transform")).toEqual({ id: objs[0].id, transform: t, keepOnPlate: true }));
    await waitFor(() => expect(st().objects[0].bounds.min[2]).toBe(0));
    await act(async () => cb().onTransformEnd(objs[0].id, { ...t, translation: [9, 9, 2] }, "translate"));
    await waitFor(() => expect(backend.lastCall("set_transform")).toMatchObject({ keepOnPlate: false }));
    await waitFor(() => expect(st().objects[0].transform.translation).toEqual([9, 9, 2]));
  });

  it("dragging models moves them", async () => {
    const { objs } = await setup();
    const moves = objs.map((o, i) => ({ id: o.id, transform: { ...o.transform, translation: [i * 30, 5, 0] as [number, number, number] } }));
    await act(async () => cb().onMoveEnd(moves));
    await waitFor(() => expect(st().objects.map((o) => o.transform.translation)).toEqual([
      [0, 5, 0],
      [30, 5, 0],
    ]));
  });

  it("measures the distance between two picked points", async () => {
    await setup();
    act(() => st().setTool("measure"));
    act(() => cb().onMeasurePoint([0, 0, 0]));
    act(() => cb().onMeasurePoint([3, 4, 0]));
    expect(screen.getByText("5.00 mm")).toBeInTheDocument();
    expect(screen.getByText(/ΔX 3.00 · ΔY 4.00 · ΔZ/)).toBeInTheDocument();
    expect(view().measure).toEqual([[0, 0, 0], [3, 4, 0]]);
    // a third point starts a new measurement
    act(() => cb().onMeasurePoint([1, 1, 1]));
    expect(st().measure).toEqual({ a: [1, 1, 1], b: [1, 1, 1] });
    expect(view().measure).toEqual([[1, 1, 1], null]);
  });

  it("adds and removes manual supports where the model is clicked", async () => {
    const { objs } = await setup("a");
    const id = objs[0].id;
    act(() => st().setTool("support_add"));
    expect(view().tool).toBe("support_add");
    await act(async () => cb().onSurfaceClick(id, [2, 2, 5], [0, 0, -1]));
    await waitFor(() => expect(st().objects[0].support_points).toBe(1));
    expect(backend.lastCall("set_support_points")).toEqual({ id, points: [{ position: [2, 2, 5], normal: [0, 0, -1], size: "medium" }] });
    await act(async () => cb().onSupportClick(id, [2.5, 2, 5]));
    await waitFor(() => expect(st().objects[0].support_points).toBe(0));
  });

  it("lay flat sends the clicked face and returns to the select tool", async () => {
    const { objs } = await setup("a");
    act(() => st().setTool("layflat"));
    await act(async () => cb().onFaceClick!(objs[0].id, 17));
    await waitFor(() => expect(backend.lastCall("lay_flat")).toEqual({ id: objs[0].id, face: 17 }));
    await waitFor(() => expect(st().tool).toBe("select"));
    expect(st().objects[0].transform.rotation).toEqual([Math.SQRT1_2, 0, 0, Math.SQRT1_2]);
  });

  it("lay flat errors become a toast and still leave the tool", async () => {
    await setup("a");
    act(() => st().setTool("layflat"));
    await act(async () => cb().onFaceClick!(999, 0));
    await waitFor(() => expect(toasts()).toContain("error: object not found"));
    expect(st().tool).toBe("select");
  });

  it("passes overhang highlighting, tools and camera presets to the view", async () => {
    const user = userEvent.setup();
    await setup();
    act(() => st().setOverhang(true));
    expect(view().overhang).toEqual({ enabled: true, angle: 45 });
    act(() => st().setConfig({ ...st().config!, support_settings: { ...st().config!.support_settings, overhang_angle: 30 } }));
    expect(view().overhang).toEqual({ enabled: true, angle: 30 });
    act(() => st().setTool("scale"));
    expect(view().tool).toBe("scale");
    await user.click(screen.getByRole("button", { name: "Top view" }));
    expect(view().camera).toBe("top");
    await user.click(screen.getByRole("button", { name: "Front view" }));
    expect(view().camera).toBe("front");
    await user.click(screen.getByRole("button", { name: "3D view" }));
    expect(view().camera).toBe("iso");
  });
});

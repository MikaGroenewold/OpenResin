import { act, fireEvent, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { api } from "../api";
import { useStore, type Panel } from "../store";
import { ModelView } from "../views/ModelView";
import { cleanReport, defaultSupportSettings } from "./fakeBackend";
import { FakeSceneView } from "./fakeScene";
import { backend, inPanel, panel, renderWithConfig, seed, setNumber, settle, toasts } from "./harness";
import { rustStructFields } from "./rust";

const st = () => useStore.getState();

async function open(p: Panel, opts: { select?: boolean; models?: string[] } = {}) {
  const objs = seed(...(opts.models ?? ["cube"]));
  if (opts.select) st().select([objs[0].id]);
  useStore.setState({ panel: p });
  await renderWithConfig(<ModelView />);
  return objs;
}

const field = (scope: ReturnType<typeof within>, label: string) => scope.getByLabelText(new RegExp(`^${label.replace(/[()]/g, "\\$&")}`));

describe("OrientPanel", () => {
  it("passes the chosen goal to auto_orient for all models", async () => {
    const user = userEvent.setup();
    const objs = await open("orient", { models: ["a", "b"] });
    const p = inPanel("Auto Orient");
    expect(p.getByRole("radio", { name: /Balanced/ })).toBeChecked();
    expect(p.getByText(/Tilts parts for supported printing/)).toBeInTheDocument();
    await user.click(p.getByRole("radio", { name: /Lowest height/ }));
    expect(p.getByRole("radio", { name: /Lowest height/ })).toBeChecked();
    await user.click(p.getByRole("button", { name: "Orient" }));
    await waitFor(() => expect(backend.callsOf("auto_orient")).toHaveLength(1));
    expect(backend.lastCall("auto_orient")).toEqual({ ids: objs.map((o) => o.id), options: { goal: "lowest_height" } });
    await waitFor(() => expect(st().objects[0].transform.rotation).toEqual([0, Math.SQRT1_2, 0, Math.SQRT1_2]));
  });

  it("orients only the selection with Fewest supports", async () => {
    const user = userEvent.setup();
    const objs = await open("orient", { models: ["a", "b"], select: true });
    const p = inPanel("Auto Orient");
    await user.click(p.getByRole("radio", { name: /Fewest supports/ }));
    await user.click(p.getByRole("button", { name: "Orient" }));
    await waitFor(() => expect(backend.lastCall("auto_orient")).toEqual({ ids: [objs[0].id], options: { goal: "fewest_supports" } }));
  });
});

describe("LabelPanel", () => {
  it("edits text, mode and sizes in the store", async () => {
    const user = userEvent.setup();
    await open("label");
    const p = inPanel("Add Label");
    const text = p.getByLabelText("Text");
    await user.clear(text);
    await user.type(text, "Benchy #1");
    expect(st().label.text).toBe("Benchy #1");
    await user.click(p.getByRole("button", { name: "Engraved" }));
    expect(st().label.mode).toBe("engrave");
    expect(p.getByRole("button", { name: "Engraved" })).toHaveClass("on");
    setNumber(field(p, "Letter height"), "8");
    setNumber(field(p, "Depth"), "1,2"); // decimal comma is accepted
    expect(st().label).toEqual({ text: "Benchy #1", mode: "engrave", height: 8, depth: 1.2 });
    setNumber(field(p, "Letter height"), "500"); // clamped to the maximum
    expect(st().label.height).toBe(50);
    setNumber(field(p, "Letter height"), "abc"); // invalid input is reverted
    expect(st().label.height).toBe(50);
    expect(field(p, "Letter height")).toHaveValue("50");
  });

  it("Place on model toggles the label tool", async () => {
    const user = userEvent.setup();
    await open("label");
    const p = inPanel("Add Label");
    await user.click(p.getByRole("button", { name: "Place on model" }));
    expect(st().tool).toBe("label");
    expect(FakeSceneView.last!.tool).toBe("label");
    const active = p.getByRole("button", { name: "Click the model to place the text… (Esc to finish)" });
    expect(active).toHaveClass("active");
    await user.click(active);
    expect(st().tool).toBe("select");
  });

  it("clicking the model with the label tool places the label with the panel settings", async () => {
    const [o] = await open("label");
    useStore.setState({ tool: "label", label: { text: "OR", height: 6, depth: 1, mode: "engrave" } });
    await act(async () => FakeSceneView.last!.cb!.onLabelClick!(o.id, [1, 2, 3], [0, -1, 0]));
    await waitFor(() => expect(backend.callsOf("add_label")).toHaveLength(1));
    expect(backend.lastCall("add_label")).toEqual({
      id: o.id,
      text: "OR",
      options: { height: 6, depth: 1, mode: "engrave" },
      position: [1, 2, 3],
      normal: [0, -1, 0],
    });
    // engraved text is a cutout: the cutout mesh is shown
    await waitFor(() => expect(backend.callsOf("get_cutout_mesh")).toEqual([{ id: o.id }]));
    for (const k of Object.keys(backend.lastCall("add_label")!.options as object))
      expect(rustStructFields("LabelOptions").map((f) => f.name)).toContain(k);
  });

  it("embossed labels reload the model mesh", async () => {
    const [o] = await open("label");
    useStore.setState({ tool: "label" });
    const meshCalls = backend.callsOf("get_mesh").length;
    await act(async () => FakeSceneView.last!.cb!.onLabelClick!(o.id, [0, 0, 10], [0, 0, 1]));
    await waitFor(() => expect(backend.callsOf("get_mesh").length).toBe(meshCalls + 1));
    expect(backend.lastCall("add_label")!.options).toEqual({ height: 5, depth: 0.8, mode: "emboss" });
  });

  it("shows backend errors (e.g. empty text) as a toast", async () => {
    const [o] = await open("label");
    useStore.setState({ tool: "label", label: { ...st().label, text: " " } });
    await act(async () => FakeSceneView.last!.cb!.onLabelClick!(o.id, [0, 0, 0], [0, 0, 1]));
    await waitFor(() => expect(toasts()).toContain("error: the label needs text"));
  });
});

describe("PlatformPanel", () => {
  it("adds a platform under every model with the given options", async () => {
    const user = userEvent.setup();
    const objs = await open("platform", { models: ["a", "b"] });
    const p = inPanel("Add Platform");
    setNumber(field(p, "Thickness"), 2);
    setNumber(field(p, "Margin"), 3);
    setNumber(field(p, "Chamfer"), 0.5);
    await user.click(p.getByRole("button", { name: "Add Platform" }));
    await waitFor(() => expect(backend.callsOf("add_platform")).toHaveLength(2));
    expect(backend.callsOf("add_platform")).toEqual(objs.map((o) => ({ id: o.id, options: { margin: 3, thickness: 2, chamfer: 0.5 } })));
    // the pad adds to the model height and is on the plate
    await waitFor(() => expect(st().objects[0].bounds.max[2]).toBe(22));
    expect(st().objects[0].bounds.min[2]).toBe(0);
  });

  it("only treats the selection when there is one", async () => {
    const user = userEvent.setup();
    const objs = await open("platform", { models: ["a", "b"], select: true });
    await user.click(inPanel("Add Platform").getByRole("button", { name: "Add Platform" }));
    await waitFor(() => expect(backend.callsOf("add_platform")).toEqual([{ id: objs[0].id, options: { margin: 2, thickness: 1.5, chamfer: 1 } }]));
  });
});

describe("SupportPanel", () => {
  it("cross bracing toggle sends the updated settings and reveals its options", async () => {
    const user = userEvent.setup();
    await open("support");
    const p = inPanel("Supports");
    expect(p.queryByLabelText(/^Brace distance/)).toBeNull();
    await user.click(p.getByRole("checkbox", { name: "Cross bracing" }));
    await waitFor(() => expect(backend.callsOf("set_support_settings")).toHaveLength(1));
    const sent = backend.lastCall("set_support_settings")!.settings as Record<string, unknown>;
    expect(sent).toEqual({ ...defaultSupportSettings(), cross_bracing: true });
    expect(st().config!.support_settings.cross_bracing).toBe(true);
    setNumber(field(p, "Brace distance"), 15);
    setNumber(field(p, "Brace Ø"), 1.2);
    expect(backend.lastCall("set_support_settings")!.settings).toMatchObject({ cross_bracing: true, bracing_max_distance: 15, bracing_diameter: 1.2 });
    expect(backend.config.support_settings).toMatchObject({ cross_bracing: true, bracing_max_distance: 15, bracing_diameter: 1.2 });
    // generating uses the same settings
    await user.click(p.getByRole("button", { name: "Auto supports" }));
    await waitFor(() => expect(backend.callsOf("auto_supports")).toHaveLength(1));
    expect(backend.lastCall("auto_supports")!.settings).toMatchObject({ cross_bracing: true, bracing_max_distance: 15 });
    // auto_supports does not save settings: the panel's set_support_settings calls did
    expect(backend.callsOf("set_support_settings")).toHaveLength(3);
    expect((await api.getConfig()).support_settings).toMatchObject({ cross_bracing: true, bracing_max_distance: 15, bracing_diameter: 1.2 });
  });

  it("support size selects which diameters are edited", async () => {
    const user = userEvent.setup();
    await open("support");
    const p = inPanel("Supports");
    await user.click(p.getByRole("button", { name: "heavy" }));
    expect(st().config!.support_settings.default_size).toBe("heavy");
    expect(field(p, "Contact Ø")).toHaveValue("0.65");
    setNumber(field(p, "Contact Ø"), 0.7);
    setNumber(field(p, "Shaft Ø"), 2);
    expect(backend.lastCall("set_support_settings")!.settings).toMatchObject({ tip_diameter: [0.3, 0.45, 0.7], shaft_diameter: [0.8, 1.1, 2] });
    setNumber(field(p, "Overhang angle"), 120);
    expect(st().config!.support_settings.overhang_angle).toBe(89);
  });

  it("raft settings are shown only with a raft", async () => {
    const user = userEvent.setup();
    await open("support");
    const p = inPanel("Supports");
    expect(field(p, "Raft thickness")).toBeInTheDocument();
    await user.click(p.getByRole("checkbox", { name: "Raft" }));
    expect(backend.lastCall("set_support_settings")!.settings).toMatchObject({ raft: false });
    expect(p.queryByLabelText(/^Raft thickness/)).toBeNull();
  });

  it("raft panel only shows raft settings and rebuilds supports", async () => {
    const user = userEvent.setup();
    const [o] = await open("raft");
    const p = inPanel("Raft");
    expect(p.queryByRole("checkbox", { name: "Cross bracing" })).toBeNull();
    await user.click(p.getByRole("button", { name: "Rebuild supports" }));
    await waitFor(() => expect(backend.lastCall("auto_supports")).toMatchObject({ ids: [o.id] }));
  });

  it("clears supports of all models", async () => {
    const user = userEvent.setup();
    const objs = await open("support", { models: ["a", "b"] });
    await user.click(inPanel("Supports").getByRole("button", { name: "Clear" }));
    await waitFor(() => expect(backend.callsOf("clear_supports")).toEqual(objs.map((o) => ({ id: o.id }))));
  });
});

describe("Copy, mirror, align and layout panels", () => {
  it("copies and fills the plate", async () => {
    const user = userEvent.setup();
    const [o] = await open("copy", { select: true });
    const p = inPanel("Copy");
    setNumber(field(p, "Copies"), "2.6"); // rounded to whole copies
    await user.click(p.getByRole("button", { name: "Copy" }));
    await waitFor(() => expect(backend.lastCall("duplicate_object")).toEqual({ id: o.id, count: 3 }));
    await waitFor(() => expect(st().objects).toHaveLength(4));
    setNumber(field(p, "Spacing"), 1);
    await user.click(p.getByRole("button", { name: "Fill plate" }));
    await waitFor(() => expect(backend.lastCall("fill_plate")).toEqual({ id: o.id, spacing: 1 }));
  });

  it("mirrors the selection", async () => {
    const user = userEvent.setup();
    const [o] = await open("mirror", { select: true });
    await user.click(inPanel("Mirror").getByRole("button", { name: "Mirror Y" }));
    await waitFor(() => expect(backend.lastCall("set_transform")).toMatchObject({ id: o.id, keepOnPlate: true, transform: { scale: [1, -1, 1] } }));
  });

  it("every align button sends a mode the backend accepts", async () => {
    const user = userEvent.setup();
    await open("align", { models: ["a", "b"] });
    const p = inPanel("Align");
    for (const name of ["Left", "Center X", "Right", "Front", "Center Y", "Back", "Plate center", "Drop to plate"])
      await user.click(p.getByRole("button", { name }));
    await waitFor(() => expect(backend.callsOf("align_objects")).toHaveLength(8));
    expect(backend.callsOf("align_objects").map((c) => c.mode)).toEqual(["left", "center_x", "right", "front", "center_y", "back", "plate_center", "drop"]);
    expect(toasts().filter((t) => t.startsWith("error"))).toEqual([]);
  });

  it("arranges all models", async () => {
    const user = userEvent.setup();
    await open("layout", { models: ["a", "b"] });
    await user.click(inPanel("Auto layout").getByRole("button", { name: "Arrange all" }));
    await waitFor(() => expect(backend.callsOf("arrange")).toHaveLength(1));
  });
});

describe("HollowPanel", () => {
  it("hollows with the wall thickness and precision, then deepens drain holes", async () => {
    const user = userEvent.setup();
    const [o] = await open("hollow");
    const p = inPanel("Hollow");
    setNumber(field(p, "Wall thickness"), 4);
    await user.selectOptions(p.getByLabelText(/^Precision/), "fine");
    await user.click(p.getByRole("button", { name: "Hollow" }));
    await waitFor(() => expect(backend.callsOf("hollow_object")).toHaveLength(1));
    expect(backend.lastCall("hollow_object")).toEqual({ id: o.id, options: { wall_thickness: 4, voxel_size: 0.5 } });
    await waitFor(() => expect(toasts()).toContain("success: Hollowed – saves 1.7 ml resin"));
    expect(st().drill.depth).toBe(5.5);
    expect(st().objects[0].cutouts).toBe(1);
    await user.click(p.getByRole("button", { name: "Remove" }));
    await waitFor(() => expect(backend.lastCall("clear_cutouts")).toEqual({ id: o.id }));
    await waitFor(() => expect(st().objects[0].cutouts).toBe(0));
  });

  it("drills drain holes where the model is clicked", async () => {
    const user = userEvent.setup();
    const [o] = await open("hollow");
    const p = panel("Hollow");
    setNumber(field(within(p), "Diameter"), 2.5);
    await user.click(within(p).getByRole("button", { name: "Add drain holes" }));
    expect(st().tool).toBe("drill");
    await act(async () => FakeSceneView.last!.cb!.onDrillClick!(o.id, [0, 0, 1], [0, 0, -1]));
    await waitFor(() => expect(backend.lastCall("add_drain_hole")).toEqual({ id: o.id, position: [0, 0, 1], normal: [0, 0, -1], diameter: 2.5, depth: 4 }));
  });
});

describe("CutPanel", () => {
  it("asks for one model", async () => {
    await open("cut");
    expect(screen.getByText("Select one model to cut.")).toBeInTheDocument();
  });

  it("cuts along the chosen axis at the chosen position", async () => {
    const user = userEvent.setup();
    const [o] = await open("cut", { select: true });
    const p = inPanel("Cut");
    await user.click(p.getByRole("button", { name: "Y" }));
    const pos = p.getByRole("textbox", { name: /Cut position/ });
    expect(pos).toHaveValue("0.00"); // middle of the model
    setNumber(pos, 4);
    expect(FakeSceneView.last!.cutPlane).toMatchObject({ axis: "y", position: 4 });
    await user.click(p.getByRole("button", { name: "Cut" }));
    await waitFor(() => expect(backend.lastCall("cut_object")).toEqual({ id: o.id, axis: "y", position: 4 }));
    await waitFor(() => expect(st().objects.map((x) => x.name)).toEqual(["cube (cut 1)", "cube (cut 2)"]));
    expect(st().panel).toBeNull();
    expect(st().selection).toEqual([]);
  });

  it("shows backend errors for a plane outside the model", async () => {
    const user = userEvent.setup();
    await open("cut", { select: true });
    const p = inPanel("Cut");
    await user.click(p.getByRole("button", { name: "X" }));
    fireEvent.change(p.getByRole("slider", { name: "Cut position" }), { target: { value: "-10" } });
    await user.click(p.getByRole("button", { name: "Cut" }));
    await waitFor(() => expect(toasts()).toContain("error: the plane does not cut the model"));
  });
});

describe("BooleanPanel", () => {
  it("turns the selection into a cutter and back", async () => {
    const user = userEvent.setup();
    const [o] = await open("boolean", { select: true, models: ["a", "b"] });
    const p = inPanel("Boolean Operation");
    expect(p.getByRole("button", { name: "Make solid again" })).toBeDisabled();
    await user.click(p.getByRole("button", { name: "Use as cutter (subtract)" }));
    await waitFor(() => expect(backend.lastCall("set_subtract")).toEqual({ ids: [o.id], subtract: true }));
    await waitFor(() => expect(p.getByRole("button", { name: "Use as cutter (subtract)" })).toBeDisabled());
    expect(FakeSceneView.last!.objects.get(o.id)?.subtract).toBe(true);
    await user.click(p.getByRole("button", { name: "Make solid again" }));
    await waitFor(() => expect(backend.lastCall("set_subtract")).toEqual({ ids: [o.id], subtract: false }));
  });

  it("explains how to pick cutters without a selection", async () => {
    await open("boolean");
    expect(screen.getByText("Select the model(s) to use as cutter.")).toBeInTheDocument();
  });
});

describe("RepairPanel", () => {
  it("shows the analysis and repairs with options the backend understands", async () => {
    const user = userEvent.setup();
    backend.reports[1] = { ...cleanReport(), holes: 3, boundary_edges: 12, watertight: false };
    const [o] = await open("repair", { models: ["broken"] });
    const p = inPanel("Error Detection");
    expect(await p.findByText("3 issues")).toBeInTheDocument();
    const holes = p.getByText("Holes").nextElementSibling!;
    expect(holes).toHaveTextContent("3");
    expect(holes).toHaveClass("bad");
    expect(p.getByText("Watertight").nextElementSibling).toHaveTextContent("no");

    await user.click(p.getByRole("checkbox", { name: "Remove degenerate triangles" }));
    await user.click(p.getByRole("button", { name: "Repair" }));
    await waitFor(() => expect(backend.callsOf("repair_objects")).toHaveLength(1));
    const call = backend.lastCall("repair_objects")!;
    expect(call.ids).toEqual([o.id]);
    expect(call.options).toEqual({
      weld_tolerance: 0.0001,
      remove_degenerate: false,
      remove_duplicates: true,
      fix_orientation: true,
      fill_holes: true,
      max_hole_edges: 0,
      min_shell_volume: 0,
    });
    // the options object matches RepairOptions field by field
    expect(Object.keys(call.options as object).sort()).toEqual(rustStructFields("RepairOptions").map((f) => f.name).sort());
    await waitFor(() => expect(toasts()).toContain("success: broken: 3 holes filled"));
    await waitFor(() => expect(p.getByText("OK")).toBeInTheDocument());
  });
});

describe("Transform panel", () => {
  it("rotates, centres and resizes the selected model", async () => {
    const user = userEvent.setup();
    const [o] = await open(null, { select: true });
    const p = inPanel("Position, rotation & size");
    await user.click(p.getByRole("button", { name: "X +90°" }));
    await waitFor(() => expect(backend.callsOf("set_transform")).toHaveLength(1));
    const rot = backend.lastCall("set_transform")!;
    expect(rot.keepOnPlate).toBe(true);
    const q = (rot.transform as { rotation: number[] }).rotation;
    [Math.SQRT1_2, 0, 0, Math.SQRT1_2].forEach((v, i) => expect(q[i]).toBeCloseTo(v, 6));
    await waitFor(() => expect(field(p, "Rotate X")).toHaveValue("90.0"));

    setNumber(field(p, "Size X"), 40);
    await waitFor(() => expect(backend.callsOf("set_transform")).toHaveLength(2));
    expect((backend.lastCall("set_transform")!.transform as { scale: number[] }).scale).toEqual([2, 2, 2]);

    await user.click(p.getByRole("checkbox", { name: "Uniform" }));
    setNumber(field(p, "Size Y"), 20);
    await waitFor(() => expect(backend.callsOf("set_transform")).toHaveLength(3));
    expect((backend.lastCall("set_transform")!.transform as { scale: number[] }).scale).toEqual([2, 1, 2]);

    await user.click(p.getByRole("button", { name: "100 %" }));
    await waitFor(() => expect((backend.lastCall("set_transform")!.transform as { scale: number[] }).scale).toEqual([1, 1, 1]));

    setNumber(field(p, "X"), 12.5);
    await waitFor(() => expect(backend.lastCall("set_transform")).toMatchObject({ keepOnPlate: false, transform: { translation: [12.5, 0, 0] } }));
    setNumber(field(p, "Z (lift)"), 3);
    await waitFor(() => expect(st().objects[0].bounds.min[2]).toBe(3));
    await user.click(p.getByRole("button", { name: "Center" }));
    await waitFor(() => expect(backend.lastCall("set_transform")).toMatchObject({ transform: { translation: [0, 0, 3] } }));
    await user.click(p.getByRole("button", { name: "Reset rotation" }));
    await waitFor(() => expect(backend.lastCall("set_transform")).toMatchObject({ transform: { rotation: [0, 0, 0, 1] } }));
    expect(o.id).toBe(st().objects[0].id);
  });

  it("is hidden for multiple selections and while editing supports", async () => {
    const objs = await open(null, { models: ["a", "b"] });
    st().select(objs.map((o) => o.id));
    await settle(1);
    expect(screen.queryByRole("heading", { name: "Position, rotation & size" })).toBeNull();
    st().select([objs[0].id]);
    await settle(1);
    expect(screen.getByRole("heading", { name: "Position, rotation & size" })).toBeInTheDocument();
    st().setTool("support_add");
    await settle(1);
    expect(screen.queryByRole("heading", { name: "Position, rotation & size" })).toBeNull();
  });
});

describe("Objects panel and status bar", () => {
  it("lists models, selects with click / Ctrl+click and renames by double-click", async () => {
    const user = userEvent.setup();
    const objs = await open(null, { models: ["benchy", "cube"] });
    const p = inPanel("Models (2)");
    await user.click(p.getByText("benchy"));
    expect(st().selection).toEqual([objs[0].id]);
    await user.keyboard("{Control>}");
    await user.click(p.getByText("cube"));
    await user.keyboard("{/Control}");
    expect(st().selection).toEqual([objs[0].id, objs[1].id]);
    expect(screen.getByText(/2 selected: /)).toHaveTextContent("2 selected: 20.0 × 20.0 × 20.0 mm");
    await user.dblClick(p.getByText("cube"));
    const input = p.getByDisplayValue("cube");
    await user.clear(input);
    await user.type(input, "Calibration cube{Enter}");
    await waitFor(() => expect(backend.lastCall("rename_object")).toEqual({ id: objs[1].id, name: "Calibration cube" }));
    await waitFor(() => expect(p.getByText("Calibration cube")).toBeInTheDocument());
    await user.click(p.getByRole("button", { name: "Select all (Ctrl+A)" }));
    expect(st().selection).toEqual(objs.map((o) => o.id));
  });

  it("status bar sums models, volume and plate usage", async () => {
    await open(null, { models: ["a", "b"] });
    const bar = screen.getByRole("contentinfo");
    expect(bar).toHaveTextContent("2 models · ≈16.0 ml");
    expect(bar).toHaveTextContent("Plate 7 % · height 20.0 / 165 mm");
    expect(bar).toHaveTextContent("Mars 5 Ultra · 9024×5120 · 17.0 µm · 0.05 mm");
  });

  it("adds test shapes on an empty plate", async () => {
    const user = userEvent.setup();
    await renderWithConfig(<ModelView />);
    expect(screen.getByText("Drop STL, OBJ or 3MF files here")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Cylinder" }));
    await waitFor(() => expect(st().objects.map((o) => o.name)).toEqual(["Cylinder 20 mm"]));
    expect(backend.lastCall("add_primitive")).toEqual({ kind: "cylinder" });
  });
});

describe("Slice panel", () => {
  it("shows the print settings and slices with the chosen format", async () => {
    const user = userEvent.setup();
    await open(null);
    const p = inPanel("Print settings");
    expect(p.getByText("Standard resin")).toBeInTheDocument();
    expect(p.getByText("2.5 s · bottom 30 s × 4")).toBeInTheDocument();
    const fmt = p.getByLabelText(/^Output format/);
    expect(within(fmt).getByRole("option", { name: "Printer default (goo)" })).toBeInTheDocument();
    expect(within(fmt).getByRole("group", { name: "Other formats" })).toBeInTheDocument();
    await user.selectOptions(fmt, "ctb");
    await user.click(p.getByRole("button", { name: "Slice" }));
    await waitFor(() => expect(st().mode).toBe("preview"));
    expect(backend.lastCall("slice")!.format).toBe("ctb");
  });

  it("shows progress with a cancel button while slicing", async () => {
    const user = userEvent.setup();
    await open(null);
    const release = backend.hold("slice");
    await user.click(inPanel("Print settings").getByRole("button", { name: "Slice" }));
    const bar = await screen.findByRole("progressbar");
    expect(bar).toHaveTextContent("Slicing 0/1");
    await user.click(inPanel("Print settings").getByRole("button", { name: "Cancel" }));
    expect(backend.callsOf("cancel_slice")).toHaveLength(1);
    release();
    await waitFor(() => expect(screen.queryByRole("progressbar")).toBeNull());
  });

  it("offers slice & send for a connected idle printer and opens the send dialog", async () => {
    const user = userEvent.setup();
    backend.addPrinter({ ip: "192.168.1.50", name: "Mars" });
    await open(null);
    expect(screen.queryByRole("button", { name: /Slice & send to/ })).toBeNull();
    await act(async () => {
      const { connectDevice } = await import("../actions");
      await connectDevice("192.168.1.50");
    });
    useStore.getState().updateDevice("192.168.1.50", { name: "Mars" });
    const send = await screen.findByRole("button", { name: "Slice & send to Mars" });
    await user.click(send);
    await waitFor(() => expect(st().sendTo).toBe("192.168.1.50"));
    expect(st().slice).not.toBeNull();
    // the preview opens with the send dialog for that printer: upload it
    const dialog = within(await screen.findByRole("dialog", { name: "Send to printer" }));
    expect(dialog.getByRole("radio", { name: /192.168.1.50/ })).toBeChecked();
    await user.click(dialog.getByRole("button", { name: "Upload" }));
    await waitFor(() => expect(backend.lastCall("upload_to_printer")).toEqual({ ip: "192.168.1.50", start: false, name: "plate.goo" }));
    await waitFor(() => expect(st().sendTo).toBeNull());
    expect(toasts()).toContain("success: Uploaded plate.goo");
    expect(backend.network[0].files["/local"]!.map((f) => f.path)).toContain("/local/plate.goo");
  });

  it("does not send a previous slice when slicing again fails", async () => {
    const user = userEvent.setup();
    backend.addPrinter({ ip: "192.168.1.50", name: "Mars" });
    await open(null);
    const { connectDevice, sliceNow } = await import("../actions");
    await act(async () => {
      await connectDevice("192.168.1.50");
      await sliceNow(null);
    });
    useStore.getState().setMode("model");
    const stale = st().slice;
    expect(stale).not.toBeNull();
    backend.fail("slice", "the CTB encoder failed");
    await user.click(await screen.findByRole("button", { name: /Slice & send to/ }));
    await waitFor(() => expect(toasts()).toContain("error: the CTB encoder failed"));
    expect(st().sendTo).toBeNull();
  });

  it("hides slice & send while the printer is printing", async () => {
    backend.addPrinter({ ip: "192.168.1.50", name: "Mars", state: "printing" });
    await open(null);
    const { connectDevice } = await import("../actions");
    await act(async () => void (await connectDevice("192.168.1.50")));
    expect(screen.queryByRole("button", { name: /Slice & send to/ })).toBeNull();
  });
});

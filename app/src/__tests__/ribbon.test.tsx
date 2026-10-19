import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { useStore } from "../store";
import { ModelView } from "../views/ModelView";
import { TopBar } from "../views/TopBar";
import { backend, renderWithConfig, seed } from "./harness";

const st = () => useStore.getState();

async function renderModel() {
  return renderWithConfig(
    <>
      <TopBar onPickPrinter={() => {}} />
      <ModelView />
    </>,
  );
}

const ribbon = () => screen.getByRole("toolbar");
const item = (name: string) => within(ribbon()).getByRole("button", { name });

describe("Ribbon", () => {
  it("switches tabs from the workflow bar", async () => {
    const user = userEvent.setup();
    await renderModel();
    expect(ribbon()).toHaveAccessibleName("Prepare tools");
    const tabs = within(screen.getByRole("navigation", { name: "Workflow" }));
    await user.click(tabs.getByRole("button", { name: "Support" }));
    expect(st().ribbon).toBe("support");
    expect(ribbon()).toHaveAccessibleName("Support tools");
    expect(item("Auto Support")).toBeInTheDocument();
    expect(item("Magic Support")).toBeInTheDocument();
    await user.click(tabs.getByRole("button", { name: "Repair" }));
    expect(item("Model Repair")).toBeInTheDocument();
    await user.click(tabs.getByRole("button", { name: "Export" }));
    expect(item("Slice")).toBeInTheDocument();
    expect(item("Save Sliced File")).toBeInTheDocument();
    expect(tabs.getByRole("button", { name: "Export" })).toHaveClass("on");
  });

  it("disables tools that need models and offers to open models on an empty plate", async () => {
    await renderModel();
    for (const name of ["Move", "Rotate", "Scale", "Measure", "Lay Flat", "Auto Orient", "Add Platform", "Hollow", "Align"])
      expect(item(name)).toBeDisabled();
    expect(item("Select")).toBeEnabled();
    expect(item("Open Models")).toBeEnabled();
  });

  it("enables model tools when there are models, selection tools only with a selection", async () => {
    seed("cube");
    await renderModel();
    expect(item("Move")).toBeEnabled();
    expect(item("Auto Orient")).toBeEnabled();
    for (const name of ["Copy / Fill", "Mirror", "Split Model", "Cut", "Combine Models"]) expect(item(name)).toBeDisabled();
    expect(within(ribbon()).queryByRole("button", { name: "Open Models" })).toBeNull();
    useStore.getState().select([st().objects[0].id]);
    await waitFor(() => expect(item("Copy / Fill")).toBeEnabled());
    for (const name of ["Mirror", "Split Model", "Cut", "Combine Models"]) expect(item(name)).toBeEnabled();
  });

  it("tool items set the tool and highlight the button", async () => {
    const user = userEvent.setup();
    seed("cube");
    await renderModel();
    await user.click(item("Rotate"));
    expect(st().tool).toBe("rotate");
    expect(item("Rotate")).toHaveClass("on");
    expect(item("Select")).not.toHaveClass("on");
    expect(screen.getByText("Drag the rings to rotate · the model drops back onto the plate")).toBeInTheDocument();
  });

  it("items with a tool and a panel open both, and keep the panel open on a second click", async () => {
    const user = userEvent.setup();
    seed("cube");
    await renderModel();
    await user.click(item("Add Label"));
    expect(st().tool).toBe("label");
    expect(st().panel).toBe("label");
    expect(screen.getByRole("heading", { name: "Add Label" })).toBeInTheDocument();
    await user.click(item("Add Label"));
    expect(st().panel).toBe("label");

    await user.click(item("Drill Holes"));
    expect(st().tool).toBe("drill");
    expect(st().panel).toBe("hollow");
    expect(screen.getByRole("heading", { name: "Drain holes" })).toBeInTheDocument();
  });

  it("panel-only items toggle their panel", async () => {
    const user = userEvent.setup();
    seed("cube");
    await renderModel();
    await user.click(item("Auto Orient"));
    expect(st().panel).toBe("orient");
    expect(item("Auto Orient")).toHaveClass("on");
    expect(screen.getByRole("radio", { name: /Balanced/ })).toBeChecked();
    await user.click(item("Auto Orient"));
    expect(st().panel).toBeNull();
    expect(screen.queryByRole("radio", { name: /Balanced/ })).toBeNull();
    await user.click(item("Auto Layout"));
    expect(screen.getByRole("button", { name: "Arrange all" })).toBeInTheDocument();
  });

  it("opens the panel matching each Prepare item", async () => {
    const user = userEvent.setup();
    const [o] = seed("cube");
    useStore.getState().select([o.id]);
    await renderModel();
    const expected: [string, string][] = [
      ["Add Platform", "Add Platform"],
      ["Hollow", "Hollow"],
      ["Copy / Fill", "Copy"],
      ["Mirror", "Mirror"],
      ["Cut", "Cut"],
      ["Boolean Operation", "Boolean Operation"],
      ["Align", "Align"],
    ];
    for (const [button, heading] of expected) {
      await user.click(item(button));
      expect(screen.getByRole("heading", { name: heading })).toBeInTheDocument();
    }
  });

  it("support tools close the support panel; Auto Support opens it", async () => {
    const user = userEvent.setup();
    seed("cube");
    await renderModel();
    useStore.getState().setRibbon("support");
    await user.click(await screen.findByRole("button", { name: "Auto Support" }));
    expect(st().panel).toBe("support");
    expect(screen.getByRole("heading", { name: "Supports" })).toBeInTheDocument();
    await user.click(item("Manual Support"));
    expect(st().tool).toBe("support_add");
    expect(st().panel).toBeNull();
    expect(screen.getByText("Click the model to add a support · click a support to remove it")).toBeInTheDocument();
    await user.click(item("Remove Support"));
    expect(st().tool).toBe("support_remove");
    await user.click(item("Raft"));
    expect(st().panel).toBe("raft");
    expect(screen.getByRole("heading", { name: "Raft" })).toBeInTheDocument();
  });

  it("Overhang Detection toggles the overhang view", async () => {
    const user = userEvent.setup();
    seed("cube");
    await renderModel();
    useStore.getState().setRibbon("support");
    await user.click(await screen.findByRole("button", { name: "Overhang Detection" }));
    expect(st().overhang).toBe(true);
    expect(item("Overhang Detection")).toHaveClass("on");
    await user.click(item("Overhang Detection"));
    expect(st().overhang).toBe(false);
  });

  it("Magic Support generates supports for all models", async () => {
    const user = userEvent.setup();
    const objs = seed("a", "b");
    await renderModel();
    useStore.getState().setRibbon("support");
    await user.click(await screen.findByRole("button", { name: "Magic Support" }));
    await waitFor(() => expect(backend.callsOf("auto_supports")).toHaveLength(1));
    expect(backend.lastCall("auto_supports")!.ids).toEqual(objs.map((o) => o.id));
    await waitFor(() => expect(st().objects.every((o) => o.support_points > 0)).toBe(true));
  });

  it("Split Model splits the selected model", async () => {
    const user = userEvent.setup();
    const [o] = seed("multi part");
    useStore.getState().select([o.id]);
    await renderModel();
    await user.click(item("Split Model"));
    await waitFor(() => expect(st().objects.map((x) => x.name)).toEqual(["multi part (part 1)", "multi part (part 2)"]));
    expect(backend.lastCall("split_object")).toEqual({ id: o.id });
  });

  it("Export tab: slice, settings, save and export", async () => {
    const user = userEvent.setup();
    seed("cube");
    await renderModel();
    useStore.getState().setRibbon("export");
    const onSettings = vi.fn();
    window.addEventListener("open-settings", onSettings);
    await user.click(await screen.findByRole("button", { name: "Slice Settings" }));
    expect(onSettings).toHaveBeenCalledTimes(1);
    window.removeEventListener("open-settings", onSettings);

    expect(item("Save Sliced File")).toBeDisabled();
    await user.click(item("Slice"));
    await waitFor(() => expect(st().mode).toBe("preview"));
    expect(backend.lastCall("slice")!.format).toBeNull();
    useStore.getState().setMode("model");
    await waitFor(() => expect(item("Save Sliced File")).toBeEnabled());

    backend.saveResults.push("C:\\out\\plate.stl");
    await user.click(item("Export Model"));
    await waitFor(() => expect(backend.lastCall("export_stl")).toEqual({ path: "C:\\out\\plate.stl", withSupports: true }));
  });

  it("Repair tab: Error Detection and Model Repair open the repair panel", async () => {
    const user = userEvent.setup();
    seed("cube");
    await renderModel();
    useStore.getState().setRibbon("repair");
    await user.click(await screen.findByRole("button", { name: "Error Detection" }));
    expect(st().panel).toBe("repair");
    expect(await screen.findByText("Watertight")).toBeInTheDocument();
  });
});

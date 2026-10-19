import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { sliceNow } from "../actions";
import { useStore } from "../store";
import { ModelView } from "../views/ModelView";
import { PreviewView } from "../views/PreviewView";
import { FakeSceneView } from "./fakeScene";
import { backend, loadConfig, renderWithConfig, seed, toasts } from "./harness";

const st = () => useStore.getState();

async function sliced(size: [number, number, number] = [20, 20, 2]) {
  await loadConfig();
  seed({ name: "coin", size });
  await sliceNow(null);
}

describe("PreviewView", () => {
  it("offers to go back when nothing is sliced", async () => {
    const user = userEvent.setup();
    st().setMode("preview");
    await renderWithConfig(<PreviewView />);
    expect(screen.getByText("Nothing sliced yet.")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Back to prepare" }));
    expect(st().mode).toBe("model");
  });

  it("shows the job summary and the first layer image", async () => {
    await sliced();
    await renderWithConfig(<PreviewView />);
    expect(screen.getByRole("heading", { name: "Layer 1" })).toBeInTheDocument();
    expect(screen.getByText("bottom")).toHaveClass("warn");
    const job = within(screen.getByRole("heading", { name: "Job" }).closest("section") as HTMLElement);
    expect(job.getByText(".goo")).toBeInTheDocument();
    expect(job.getByText("40 × 0.05 mm")).toBeInTheDocument(); // 2 mm / 0.05 mm
    expect(job.getByText("7 min 30 s")).toBeInTheDocument(); // 4 × (30 + 6) + 36 × (2.5 + 6) s
    expect(job.getByText("0.8 ml · 0.9 g")).toBeInTheDocument();
    expect(job.getByText("0.02 €")).toBeInTheDocument();
    expect(job.getByText("1.2 MB")).toBeInTheDocument();
    expect(job.getByText("0.8 s")).toBeInTheDocument(); // sliced in
    await waitFor(() => expect(screen.getByRole("img", { name: "Layer 1" })).toHaveAttribute("src", expect.stringMatching(/^blob:/)));
    expect(backend.lastCall("get_layer_image")).toEqual({ index: 0, maxSize: 2048 });
    // LCD images are mirrored: the preview flips them back to the top view
    expect(screen.getByRole("img", { name: "Layer 1" }).style.transform).toBe("scale(1, -1)");
  });

  it("moves through the layers with the slider, buttons and keys", async () => {
    const user = userEvent.setup();
    await sliced();
    await renderWithConfig(<PreviewView />);
    fireEvent.change(screen.getByRole("slider", { name: "Layer" }), { target: { value: "9" } });
    expect(screen.getByRole("heading", { name: "Layer 10" })).toBeInTheDocument();
    expect(screen.getByText("normal")).toBeInTheDocument();
    expect(screen.getByText("0.500 mm")).toBeInTheDocument();
    expect(screen.getByText("400.0 mm²")).toBeInTheDocument();
    await waitFor(() => expect(backend.lastCall("get_layer_image")).toEqual({ index: 9, maxSize: 2048 }));
    await user.click(screen.getByRole("button", { name: "+1" }));
    expect(st().layer).toBe(10);
    await user.click(screen.getByRole("button", { name: "−1" }));
    expect(st().layer).toBe(9);
    await user.click(screen.getByRole("button", { name: "Last" }));
    expect(st().layer).toBe(39);
    await user.click(screen.getByRole("button", { name: "+1" }));
    expect(st().layer).toBe(39); // clamped
    await user.keyboard("{PageDown}");
    expect(st().layer).toBe(29);
    await user.keyboard("{ArrowUp}");
    expect(st().layer).toBe(30);
    await user.click(screen.getByRole("button", { name: "First" }));
    expect(st().layer).toBe(0);
    await user.keyboard("{ArrowDown}");
    expect(st().layer).toBe(0);
  });

  it("reloads layer images after re-slicing even when name and file size are unchanged", async () => {
    await sliced();
    const { unmount } = await renderWithConfig(<PreviewView />);
    await waitFor(() => expect(backend.callsOf("get_layer_image")).toHaveLength(1));
    const first = screen.getByRole("img", { name: "Layer 1" }).getAttribute("src");
    unmount();
    // move the model and slice again: same suggested name and size, different content
    const { nudge } = await import("../editActions");
    st().select([st().objects[0].id]);
    await nudge(5, 0);
    await sliceNow(null);
    await renderWithConfig(<PreviewView />);
    await waitFor(() => expect(backend.callsOf("get_layer_image")).toHaveLength(2));
    await waitFor(() => expect(screen.getByRole("img", { name: "Layer 1" }).getAttribute("src")).not.toBe(first));
  });

  it("zooms by the wheel distance towards the cursor and resets on double-click", async () => {
    await sliced();
    await renderWithConfig(<PreviewView />);
    const img = await screen.findByRole("img", { name: "Layer 1" });
    const frame = img.parentElement as HTMLElement;
    const stage = frame.parentElement as HTMLElement;
    stage.getBoundingClientRect = () => ({ left: 0, top: 0, width: 800, height: 600, right: 800, bottom: 600, x: 0, y: 0, toJSON: () => ({}) });
    const scale = () => Number(/scale\(([\d.]+)\)/.exec(frame.style.transform)?.[1]);
    const pan = () => /translate\((-?[\d.]+)px, (-?[\d.]+)px\)/.exec(frame.style.transform)!.slice(1).map(Number);

    // one notch zooms in by about 20 %, three notches in a single event by about 70 %
    fireEvent.wheel(stage, { deltaY: -120, clientX: 400, clientY: 300 });
    expect(scale()).toBeCloseTo(Math.exp(0.18), 3);
    fireEvent.wheel(stage, { deltaY: -360, clientX: 400, clientY: 300 });
    expect(scale()).toBeCloseTo(Math.exp(0.72), 3);
    // zooming at the centre doesn't move the image
    expect(pan()).toEqual([0, 0]);

    // zooming at a corner keeps that point under the cursor
    const before = scale();
    fireEvent.wheel(stage, { deltaY: -120, clientX: 600, clientY: 400 });
    const f = scale() / before;
    const [px, py] = pan();
    expect(px).toBeCloseTo(200 - 200 * f, 3);
    expect(py).toBeCloseTo(100 - 100 * f, 3);

    // zoom is limited, and double-click resets it
    fireEvent.wheel(stage, { deltaY: 100000, clientX: 400, clientY: 300 });
    expect(scale()).toBe(0.5);
    fireEvent.doubleClick(stage);
    expect(scale()).toBe(1);
    expect(pan()).toEqual([0, 0]);
  });

  it("shows layer image errors in the view", async () => {
    await sliced();
    backend.fail("get_layer_image", "layer data missing", true);
    await renderWithConfig(<PreviewView />);
    expect(await screen.findByText("layer data missing")).toBeInTheDocument();
  });

  it("opens the send dialog and saves the file", async () => {
    const user = userEvent.setup();
    await sliced();
    await renderWithConfig(<PreviewView />);
    await user.click(screen.getByRole("button", { name: "Send to printer" }));
    expect(st().sendTo).toBe("");
    expect(screen.getByRole("dialog", { name: "Send to printer" })).toBeInTheDocument();
    await user.click(within(screen.getByRole("dialog")).getAllByRole("button", { name: "Close" })[0]);
    expect(st().sendTo).toBeNull();
    backend.saveResults.push("C:\\out\\coin.goo");
    await user.click(screen.getByRole("button", { name: "Save file…" }));
    await waitFor(() => expect(backend.lastCall("save_slice")).toEqual({ path: "C:\\out\\coin.goo" }));
    expect(toasts()).toContain("success: File saved");
  });

  it("clips the 3D view at the current layer height in the preview", async () => {
    await sliced();
    st().setMode("preview");
    await renderWithConfig(<ModelView />);
    st().setLayer(19);
    await waitFor(() => expect(FakeSceneView.last!.clip).toBeCloseTo(1.0, 6));
    st().setMode("model");
    await waitFor(() => expect(FakeSceneView.last!.clip).toBeNull());
  });

  it("hides the 3D view for a sliced file opened from disk", async () => {
    await loadConfig();
    const { openSliced } = await import("../actions");
    await openSliced("C:\\jobs\\old.goo");
    const { container } = await renderWithConfig(<ModelView />);
    expect(container.querySelector(".viewport-wrap")).not.toBeVisible();
    expect(screen.queryByRole("toolbar")).toBeNull();
  });
});

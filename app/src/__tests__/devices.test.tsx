import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { useStore } from "../store";
import { DevicesView } from "../views/DevicesView";
import { SendDialog } from "../views/SendDialog";
import type { FakePrinter } from "./fakeBackend";
import { backend, loadConfig, renderWithConfig, seed, settle, toasts } from "./harness";

const st = () => useStore.getState();
const IP = "192.168.1.50";

async function renderDevices(p: Partial<FakePrinter> = {}, saved = true) {
  const ip = p.ip ?? IP;
  backend.addPrinter({ name: "Mars 5 Ultra", ...p, ip });
  if (saved) backend.config.devices = [{ ip, name: "Workshop", model: "ELEGOO Mars 5 Ultra" }];
  const r = await renderWithConfig(<DevicesView />);
  await settle();
  return r;
}

async function openFiles() {
  const user = userEvent.setup();
  await renderDevices();
  await user.click(await screen.findByRole("button", { name: "Files" }));
  await screen.findByText("benchy.goo");
  return user;
}

describe("DevicesView", () => {
  it("lists saved printers, selects the first and connects to it", async () => {
    await renderDevices();
    expect(backend.callsOf("saved_devices").length).toBeGreaterThan(0);
    expect(screen.getByRole("heading", { level: 2, name: "Workshop" })).toBeInTheDocument();
    await waitFor(() => expect(backend.lastCall("connect_printer")).toEqual({ ip: IP }));
    expect(await screen.findByText("Ready to print.")).toBeInTheDocument();
    expect(screen.getByText("31.5 °C")).toBeInTheDocument(); // UV LED
    expect(screen.queryByText("Enclosure")).toBeNull(); // 0 °C = no sensor
    expect(screen.getByText("3.7 GB")).toBeInTheDocument(); // free storage
    expect(screen.getByText("V1.2.3")).toBeInTheDocument();
    expect(screen.getByText("WiFi")).toBeInTheDocument();
    expect(screen.getAllByText("Idle").length).toBeGreaterThan(0);
  });

  it("shows the empty state and searches the network", async () => {
    const user = userEvent.setup();
    backend.addPrinter({ ip: "192.168.1.60", name: "Saturn 4" });
    await renderWithConfig(<DevicesView />);
    expect(screen.getByText("No printers yet. Search the network or add a printer by its IP address.")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Search network" }));
    expect(await screen.findByRole("heading", { level: 2, name: "Saturn 4" })).toBeInTheDocument();
    expect(backend.lastCall("discover_printers")).toEqual({ timeoutMs: 3000 });
    // discovered, not saved: offers to remember it
    await user.click(screen.getByRole("button", { name: "Remember" }));
    await waitFor(() =>
      expect(backend.lastCall("save_device")).toEqual({ device: { ip: "192.168.1.60", name: "Saturn 4", model: "ELEGOO Mars 5 Ultra" } }),
    );
    expect(await screen.findByRole("button", { name: "Forget" })).toBeInTheDocument();
  });

  it("adds a printer by IP, connects and takes over its reported name", async () => {
    const user = userEvent.setup();
    backend.addPrinter({ ip: "10.0.0.7", name: "Shop printer", model: "ELEGOO Saturn 4 Ultra" });
    await renderWithConfig(<DevicesView />);
    const input = screen.getByRole("textbox", { name: "Printer IP address" });
    expect(screen.getByRole("button", { name: "Add" })).toBeDisabled();
    await user.type(input, " 10.0.0.7 {Enter}");
    await waitFor(() => expect(backend.callsOf("save_device")).toHaveLength(2));
    expect(backend.callsOf("save_device")).toEqual([
      { device: { ip: "10.0.0.7", name: "10.0.0.7" } },
      { device: { ip: "10.0.0.7", name: "Shop printer", model: "ELEGOO Saturn 4 Ultra" } },
    ]);
    expect(await screen.findByRole("heading", { level: 2, name: "Shop printer" })).toBeInTheDocument();
    expect(input).toHaveValue("");
  });

  it("rejects an invalid IP with the backend error", async () => {
    const user = userEvent.setup();
    await renderWithConfig(<DevicesView />);
    await user.type(screen.getByRole("textbox", { name: "Printer IP address" }), "printer.local{Enter}");
    await waitFor(() => expect(toasts()).toContain("error: invalid IP address: printer.local"));
  });

  it("explains connection failures", async () => {
    await renderDevices({ reachable: false });
    expect(await screen.findByText("Could not connect.")).toBeInTheDocument();
    expect(screen.getByText(/connection refused/)).toBeInTheDocument();
    expect(screen.getByText("Not connected.")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Connect" })).toBeEnabled();
  });

  it("pauses and stops a running print (stop needs confirmation)", async () => {
    const user = userEvent.setup();
    await renderDevices({ state: "printing" });
    await user.click(await screen.findByRole("button", { name: "Pause" }));
    await waitFor(() => expect(backend.lastCall("printer_command")).toEqual({ ip: IP, command: "pause" }));
    backend.askAnswers.push(false);
    await user.click(screen.getByRole("button", { name: "Stop" }));
    await waitFor(() => expect(backend.asked).toContain("Stop the current print? This cannot be undone."));
    expect(backend.commands.map((c) => c.command)).toEqual(["pause"]);
    backend.askAnswers.push(true);
    await user.click(screen.getByRole("button", { name: "Stop" }));
    await waitFor(() => expect(backend.commands.map((c) => c.command)).toEqual(["pause", "stop"]));
  });

  it("shows print progress, elapsed and remaining time", async () => {
    await renderDevices({ state: "printing" });
    await screen.findByRole("button", { name: "Pause" });
    act(() =>
      st().updateDevice(IP, {
        status: {
          ...st().devices[IP].status!,
          status: { state: "printing", filename: "/local/benchy.goo", current_layer: 150, total_layers: 600, elapsed_s: 1800, total_s: 7200 },
        },
      }),
    );
    expect(screen.getByText("25 %")).toBeInTheDocument();
    expect(screen.getByText("150 / 600")).toBeInTheDocument();
    expect(screen.getByText("30 min 00 s")).toBeInTheDocument();
    expect(screen.getByText("1 h 30 min")).toBeInTheDocument();
    expect(screen.getByText("benchy.goo")).toBeInTheDocument();
  });

  it("toggles time-lapse and the camera", async () => {
    const user = userEvent.setup();
    await renderDevices();
    await user.click(await screen.findByRole("button", { name: "Time-lapse off" }));
    await waitFor(() => expect(backend.lastCall("printer_set_time_lapse")).toEqual({ ip: IP, enable: true }));
    await user.click(screen.getByRole("button", { name: "Show camera" }));
    const img = await screen.findByRole("img", { name: "Camera Workshop" });
    expect(img).toHaveAttribute("src", `http://127.0.0.1:9999/${IP}/stream.mjpg`);
    expect(backend.lastCall("camera_start")).toEqual({ ip: IP });
    backend.saveResults.push("C:\\snap.jpg");
    await user.click(screen.getByRole("button", { name: "Save snapshot" }));
    await waitFor(() => expect(backend.lastCall("camera_snapshot")).toEqual({ ip: IP, path: "C:\\snap.jpg" }));
    expect(backend.lastCall("plugin:dialog|save")!.options).toMatchObject({ defaultPath: "Workshop-snapshot.jpg" });
    await user.click(screen.getByRole("button", { name: "Stop camera" }));
    await waitFor(() => expect(backend.lastCall("camera_stop")).toEqual({ ip: IP }));
    expect(screen.queryByRole("img", { name: "Camera Workshop" })).toBeNull();
  });

  it("renames a connected printer by double-click", async () => {
    const user = userEvent.setup();
    await renderDevices();
    await screen.findByText("Ready to print.");
    await user.dblClick(screen.getByRole("heading", { level: 2, name: "Workshop" }));
    const input = screen.getByRole("textbox", { name: "Printer name" });
    await user.clear(input);
    await user.type(input, "Garage{Enter}");
    await waitFor(() => expect(backend.lastCall("printer_rename")).toEqual({ ip: IP, name: "Garage" }));
    expect(await screen.findByRole("heading", { level: 2, name: "Garage" })).toBeInTheDocument();
  });

  it("disconnects and forgets a printer", async () => {
    const user = userEvent.setup();
    await renderDevices({}, true);
    await user.click(await screen.findByRole("button", { name: "Disconnect" }));
    await waitFor(() => expect(backend.lastCall("disconnect_printer")).toEqual({ ip: IP }));
    expect(await screen.findByText("Not connected.")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Forget" }));
    await waitFor(() => expect(backend.lastCall("forget_device")).toEqual({ ip: IP }));
    await waitFor(() => expect(st().devices[IP]).toBeUndefined());
    expect(screen.getByText("Connect your printer")).toBeInTheDocument();
  });

  it("shows the print history", async () => {
    const user = userEvent.setup();
    await renderDevices({
      history: [
        { TaskName: "/local/benchy.goo", BeginTime: 1_700_000_000, EndTime: 1_700_003_600, TaskStatus: 1, AlreadyPrintLayer: 600, SliceInformation: { total_layer_numbers: 600 }, CurrentLayerTalVolume: 12.34 },
        { TaskName: "/local/fail.goo", BeginTime: 1_700_010_000, EndTime: 1_700_010_060, TaskStatus: 2, AlreadyPrintLayer: 3 },
      ],
    });
    await user.click(await screen.findByRole("button", { name: "History" }));
    const rows = await screen.findAllByRole("row");
    expect(rows).toHaveLength(3);
    expect(within(rows[1]).getByText("benchy.goo")).toBeInTheDocument();
    expect(within(rows[1]).getByText("1 h 00 min")).toBeInTheDocument();
    expect(within(rows[1]).getByText("600 / 600")).toBeInTheDocument();
    expect(within(rows[1]).getByText("12.3 ml")).toBeInTheDocument();
    expect(within(rows[1]).getByText("Completed")).toBeInTheDocument();
    expect(within(rows[2]).getByText("Error")).toBeInTheDocument();
  });
});

describe("File manager", () => {
  it("lists files and folders with the free space", async () => {
    await openFiles();
    expect(backend.lastCall("printer_files")).toEqual({ ip: IP, dir: "/local" });
    expect(screen.getByText("cube.goo")).toBeInTheDocument();
    expect(screen.getByText("minis")).toBeInTheDocument();
    expect(screen.getByText("2 files · 3.7 GB free")).toBeInTheDocument();
  });

  it("multi-selects files and deletes them after confirmation", async () => {
    const user = await openFiles();
    await user.click(screen.getByRole("checkbox", { name: "benchy.goo" }));
    await user.click(screen.getByRole("checkbox", { name: "cube.goo" }));
    expect(screen.getByText("2 selected")).toBeInTheDocument();
    const bulk = screen.getByText("2 selected").closest(".fm-bulk") as HTMLElement;

    backend.askAnswers.push(false);
    await user.click(within(bulk).getByRole("button", { name: "Delete" }));
    await waitFor(() => expect(backend.asked).toEqual(["Delete 2 files from the printer?"]));
    expect(backend.callsOf("printer_delete")).toEqual([]);

    backend.askAnswers.push(true);
    await user.click(within(bulk).getByRole("button", { name: "Delete" }));
    await waitFor(() => expect(backend.lastCall("printer_delete")).toEqual({ ip: IP, files: ["/local/benchy.goo", "/local/cube.goo"] }));
    await waitFor(() => expect(screen.queryByText("benchy.goo")).toBeNull());
    expect(screen.queryByText("2 selected")).toBeNull();
    expect(screen.getByText("0 files · 3.7 GB free")).toBeInTheDocument();
  });

  it("deletes a single file from its tile and reports files that could not be deleted", async () => {
    const user = await openFiles();
    const tile = screen.getByText("cube.goo").closest(".fm-tile") as HTMLElement;
    await user.click(within(tile).getByRole("button", { name: "Delete" }));
    await waitFor(() => expect(backend.asked).toEqual(["Delete cube.goo?"]));
    await waitFor(() => expect(backend.lastCall("printer_delete")).toEqual({ ip: IP, files: ["/local/cube.goo"] }));

    // the file disappeared on the printer meanwhile
    backend.network[0].files["/local"] = backend.network[0].files["/local"]!.filter((f) => f.path !== "/local/benchy.goo");
    const tile2 = screen.getByText("benchy.goo").closest(".fm-tile") as HTMLElement;
    await user.click(within(tile2).getByRole("button", { name: "Delete" }));
    await waitFor(() => expect(toasts()).toContain("error: Could not delete: benchy.goo"));
  });

  it("list view selects all files (not folders) at once", async () => {
    const user = await openFiles();
    await user.click(screen.getByRole("button", { name: "List view" }));
    const table = screen.getByRole("table");
    expect(within(table).getByText("11.8 MB")).toBeInTheDocument();
    await user.click(within(table).getByRole("checkbox", { name: "Select all" }));
    expect(screen.getByText("2 selected")).toBeInTheDocument();
    await user.click(within(table).getByRole("checkbox", { name: "Select all" }));
    expect(screen.queryByText(/selected/)).toBeNull();
    await user.click(screen.getByRole("button", { name: "Grid view" }));
    expect(screen.queryByRole("table")).toBeNull();
  });

  it("filters by name", async () => {
    const user = await openFiles();
    const search = screen.getByRole("textbox", { name: "Search files" });
    await user.type(search, "CUBE");
    expect(screen.getByText("cube.goo")).toBeInTheDocument();
    expect(screen.queryByText("benchy.goo")).toBeNull();
    await user.clear(search);
    await user.type(search, "zzz");
    expect(screen.getByText("No files match your search.")).toBeInTheDocument();
  });

  it("opens folders and goes back via the breadcrumb", async () => {
    const user = await openFiles();
    await user.click(screen.getByRole("button", { name: "minis" }));
    expect(await screen.findByText("knight.goo")).toBeInTheDocument();
    expect(screen.getByText("1 file · 3.7 GB free")).toBeInTheDocument();
    expect(backend.lastCall("printer_files")).toEqual({ ip: IP, dir: "/local/minis" });
    const crumbs = within(screen.getByRole("navigation", { name: "Folder" }));
    expect(crumbs.getByRole("button", { name: "minis" })).toBeDisabled();
    await user.click(crumbs.getByRole("button", { name: "Internal storage" }));
    expect(await screen.findByText("benchy.goo")).toBeInTheDocument();
  });

  it("reports a missing USB drive", async () => {
    const user = await openFiles();
    await user.click(screen.getByRole("button", { name: "USB drive" }));
    expect(await screen.findByText("No USB drive inserted.")).toBeInTheDocument();
    expect(backend.lastCall("printer_files")).toEqual({ ip: IP, dir: "/usb" });
  });

  it("starts a print from the file list after confirmation", async () => {
    const user = await openFiles();
    const tile = screen.getByText("benchy.goo").closest(".fm-tile") as HTMLElement;
    await user.click(within(tile).getByRole("button", { name: "Print" }));
    await waitFor(() => expect(backend.lastCall("printer_command")).toEqual({ ip: IP, command: "start", file: "/local/benchy.goo" }));
    expect(backend.asked).toEqual(["Start printing benchy.goo?"]);
    await waitFor(() => expect(toasts()).toContain("success: Print started"));
    expect(st().deviceTab).toBe("overview");
  });

  it("uploads picked files one after another through a queue", async () => {
    const user = await openFiles();
    backend.openResults.push(["C:\\jobs\\ring.goo", "C:\\jobs\\vase.ctb"]);
    const release = backend.hold("upload_to_printer");
    await user.click(screen.getByRole("button", { name: "Upload" }));
    const queue = await waitFor(() => document.querySelector(".fm-queue") as HTMLElement);
    await waitFor(() => expect(within(queue).getByText("Waiting")).toBeInTheDocument());
    expect(within(queue).getByText("Uploading…")).toBeInTheDocument();
    expect(within(queue).getByRole("button", { name: "Cancel upload" })).toBeInTheDocument();
    expect(backend.lastCall("plugin:dialog|open")!.options).toMatchObject({ multiple: true });
    release();
    await waitFor(() => expect(within(queue).getAllByText("Uploaded")).toHaveLength(2));
    expect(backend.callsOf("upload_to_printer")).toEqual([
      { ip: IP, start: false, name: "ring.goo", path: "C:\\jobs\\ring.goo" },
      { ip: IP, start: false, name: "vase.ctb", path: "C:\\jobs\\vase.ctb" },
    ]);
    // the list is refreshed with the new files
    expect(await screen.findByText("ring.goo", { selector: ".fm-tile .name" })).toBeInTheDocument();
    await user.click(within(queue).getByRole("button", { name: "Clear list" }));
    expect(document.querySelector(".fm-queue")).toBeNull();
  });

  it("shows upload errors per file and continues with the next one", async () => {
    const user = await openFiles();
    backend.openResults.push(["C:\\a.goo", "C:\\b.goo"]);
    backend.fail("upload_to_printer", "not enough space on the printer");
    await user.click(screen.getByRole("button", { name: "Upload" }));
    expect(await screen.findByText("not enough space on the printer")).toBeInTheDocument();
    await waitFor(() => expect(screen.getByText("Uploaded")).toBeInTheDocument());
    expect(backend.uploads.map((u) => u.name)).toEqual(["b.goo"]);
  });

  it("uploads sliced files dropped on the window and rejects other files", async () => {
    await openFiles();
    expect(st().dropTarget).toBe(IP);
    act(() => void window.dispatchEvent(new CustomEvent("printer-drop", { detail: ["C:\\model.stl", "C:\\jobs\\Drop.GOO"] })));
    await waitFor(() => expect(backend.uploads).toEqual([{ ip: IP, start: false, name: "Drop.GOO", path: "C:\\jobs\\Drop.GOO" }]));
    act(() => void window.dispatchEvent(new CustomEvent("printer-drop", { detail: ["C:\\model.stl"] })));
    expect(toasts()).toContain("info: Only sliced printer files can be uploaded.");
  });

  it("downloads a file", async () => {
    const user = await openFiles();
    backend.saveResults.push("C:\\Downloads\\cube.goo");
    const tile = screen.getByText("cube.goo").closest(".fm-tile") as HTMLElement;
    await user.click(within(tile).getByRole("button", { name: "Download" }));
    await waitFor(() => expect(backend.lastCall("printer_download")).toEqual({ ip: IP, path: "/local/cube.goo", dest: "C:\\Downloads\\cube.goo" }));
    await waitFor(() => expect(toasts()).toContain("success: Downloaded cube.goo"));
  });

  it("loads thumbnails of .goo/.ctb files and refreshes them after a re-upload", async () => {
    const user = userEvent.setup();
    const ip = "192.168.1.77"; // thumbnails are cached per session: use a printer no other test used
    await renderDevices({
      ip,
      files: {
        "/local": [
          { path: "/local/ring.goo", is_dir: false, size: 5000 },
          { path: "/local/vase.ctb", is_dir: false, size: 6000 },
          { path: "/local/old.sl1", is_dir: false, size: 7000 },
        ],
      },
    });
    await user.click(await screen.findByRole("button", { name: "Files" }));
    await waitFor(() => expect(backend.callsOf("printer_thumbnail").map((c) => c.path).sort()).toEqual(["/local/ring.goo", "/local/vase.ctb"]));
    await waitFor(() => expect(document.querySelectorAll(".fm-thumb img")).toHaveLength(2));
    expect(screen.getByText(".sl1")).toBeInTheDocument();
    // same name, new content
    act(() => void window.dispatchEvent(new CustomEvent("printer-drop", { detail: ["C:\\new\\ring.goo"] })));
    await waitFor(() => expect(backend.callsOf("printer_thumbnail").map((c) => c.path)).toHaveLength(3));
    expect(backend.lastCall("printer_thumbnail")).toEqual({ ip, path: "/local/ring.goo" });
  });

  it("disables printing while the printer is busy", async () => {
    const user = userEvent.setup();
    await renderDevices({ state: "printing" });
    await user.click(await screen.findByRole("button", { name: "Files" }));
    await screen.findByText("benchy.goo");
    const tile = screen.getByText("benchy.goo").closest(".fm-tile") as HTMLElement;
    expect(within(tile).getByRole("button", { name: "Printer is busy" })).toBeDisabled();
  });
});

describe("SendDialog", () => {
  async function sliced() {
    await loadConfig();
    seed("cube");
    const { sliceNow } = await import("../actions");
    await sliceNow(null);
  }

  it("uploads the slice to the connected printer and can start the print", async () => {
    const user = userEvent.setup();
    backend.addPrinter({ ip: IP, name: "Mars" });
    await sliced();
    const { connectDevice } = await import("../actions");
    await connectDevice(IP);
    const onClose = vi.fn();
    render(<SendDialog onClose={onClose} />);
    const dialog = within(screen.getByRole("dialog", { name: "Send to printer" }));
    await waitFor(() => expect(dialog.getByRole("radio", { name: new RegExp(IP) })).toBeChecked());
    expect(dialog.getByRole("textbox", { name: "File name on the printer" })).toHaveValue("plate.goo");
    expect(dialog.getByText("1.2 MB · 400 layers · .goo")).toBeInTheDocument();
    expect(dialog.getByText("Idle")).toHaveClass("badge"); // translated state, not the raw id
    await user.click(dialog.getByRole("checkbox", { name: "Start printing after upload" }));
    await user.click(dialog.getByRole("button", { name: "Upload & print" }));
    await waitFor(() => expect(onClose).toHaveBeenCalled());
    expect(backend.lastCall("upload_to_printer")).toEqual({ ip: IP, start: true, name: "plate.goo" });
    expect(toasts()).toContain("success: Uploaded plate.goo and started printing");
    expect(backend.network[0].state).toBe("printing");
  });

  it("connects first when the printer is not connected and uses the edited name", async () => {
    const user = userEvent.setup();
    backend.addPrinter({ ip: IP, name: "Mars" });
    await sliced();
    st().updateDevice(IP, { name: "Mars", saved: true });
    render(<SendDialog onClose={() => {}} initial={IP} />);
    const name = screen.getByRole("textbox", { name: "File name on the printer" });
    await user.clear(name);
    await user.type(name, "ring");
    await user.click(screen.getByRole("button", { name: "Upload" }));
    await waitFor(() => expect(backend.lastCall("upload_to_printer")).toEqual({ ip: IP, start: false, name: "ring" }));
    expect(backend.callsOf("connect_printer")).toEqual([{ ip: IP }]);
    expect(toasts()).toContain("success: Uploaded ring");
  });

  it("keeps the dialog open and shows the error when the printer cannot be reached", async () => {
    const user = userEvent.setup();
    await sliced();
    st().updateDevice("10.9.9.9", { name: "Gone", saved: true });
    const onClose = vi.fn();
    render(<SendDialog onClose={onClose} />);
    await user.click(screen.getByRole("button", { name: "Upload" }));
    await waitFor(() => expect(toasts()).toContain("error: could not connect to 10.9.9.9: connection refused"));
    expect(onClose).not.toHaveBeenCalled();
    expect(backend.callsOf("upload_to_printer")).toEqual([]);
  });

  it("does not offer to start a print on a busy printer", async () => {
    backend.addPrinter({ ip: IP, name: "Mars", state: "printing" });
    await sliced();
    const { connectDevice } = await import("../actions");
    await connectDevice(IP);
    render(<SendDialog onClose={() => {}} />);
    await waitFor(() => expect(screen.getByRole("checkbox", { name: "Start printing after upload" })).toBeDisabled());
    expect(screen.getByText("The printer is busy; the file will only be uploaded.")).toBeInTheDocument();
  });

  it("searches the network when no printer is known", async () => {
    backend.addPrinter({ ip: IP, name: "Mars" });
    await sliced();
    render(<SendDialog onClose={() => {}} />);
    expect(screen.getByText("No printers known yet. Searching the local network…")).toBeInTheDocument();
    expect(await screen.findByRole("radio", { name: new RegExp(IP) })).toBeChecked();
    expect(backend.callsOf("discover_printers")).toHaveLength(1);
  });

  it("shows upload progress and can cancel", async () => {
    const user = userEvent.setup();
    backend.addPrinter({ ip: IP, name: "Mars" });
    await sliced();
    const { connectDevice } = await import("../actions");
    await connectDevice(IP);
    render(<SendDialog onClose={() => {}} />);
    const release = backend.hold("upload_to_printer");
    await user.click(await screen.findByRole("button", { name: "Upload" }));
    act(() => st().updateDevice(IP, { upload: { ip: IP, sent: 512 * 1024, total: 1024 * 1024, done: false } }));
    expect(screen.getByRole("progressbar")).toHaveTextContent("512.0 KB / 1.0 MB");
    await user.click(screen.getByRole("button", { name: "Cancel upload" }));
    expect(backend.lastCall("cancel_upload")).toEqual({ ip: IP });
    release();
  });
});

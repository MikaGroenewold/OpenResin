#!/usr/bin/env python3
"""Builds profiles/printers.json from a local CHITUBOX installation.

Only factual machine data is extracted (vendor, model name, LCD resolution,
exposure area, build height, image mirroring, supported output formats and
whether the machine accepts files over the network). Nothing else is copied.

Usage:
    python -I tools/import_chitubox_profiles.py "C:/Program Files/CHITUBOX/machinecfg" profiles/printers.json
"""

import json
import re
import sys
from pathlib import Path

# Anycubic Photon Workshop extensions written by crates/formats/src/anycubic.rs.
# (.pwsz, .pm7, .pm7m, .pm4u, .pp1 and .pp1m are a different, zip based
# container and are not supported.)
ANYCUBIC = [
    "pwmx", "pwma", "pwmb", "pwmo", "pwms", "pmsq", "pmx2", "px6s", "pm3", "pm3m", "pm3r",
    "pm3n", "pm4n", "pm5", "pm5s", "m5sp", "dlp", "dl2p", "pwx", "pw0", "pws",
]

# Formats OpenResin can write, in order of preference: the printer vendor's own
# format first, then CHITUBOX CTB, generic formats last.
SUPPORTED = ["goo", "cxdlpv4", "cxdlp", *ANYCUBIC, "ctb", "sl1", "zip"]


def value(info, key, default=None):
    v = info.get(key)
    if isinstance(v, dict):
        return v.get("currentvalue", default)
    return default if v is None else v


def extensions(spec):
    return [e.lower() for e in re.findall(r"\*\.([A-Za-z0-9]+)", spec or "")]


def slug(text):
    return re.sub(r"[^a-z0-9]+", "-", text.lower()).strip("-")


def protocol(vendor, name, net_exts):
    """Maps a network capable machine to the LAN protocol it speaks."""
    n = name.lower()
    v = vendor.lower()
    if v == "elegoo" and "ultra" in n:
        return "sdcp"
    if v in ("anycubic",):
        return "anycubic"
    if "halot" in n or (v in ("creality", "piocreat") and any(e.startswith("cxdlp") for e in net_exts)):
        return "halot"
    if any(e in ("ctb", "prz") for e in net_exts):
        return "chitu-udp"
    return None


# CHITUBOX spells some vendors differently from profile to profile; use one
# display name per vendor so they group together in the printer picker.
VENDOR_NAMES = {
    "elegoo": "ELEGOO",
    "epax": "EPAX",
    "anycubic": "Anycubic",
    "creality": "Creality",
    "halot-mage": "Creality",
    "nova3d": "Nova3D",
    "x1.cfg": "X1",
}


def display_vendor(vendor, name):
    if vendor == "Other" and name.split()[0] in ("Carima",):
        return name.split()[0]
    return VENDOR_NAMES.get(vendor.lower(), vendor)


def main(src, dst):
    out = []
    seen = set()
    for f in sorted(Path(src).glob("*/*.cfgx")):
        try:
            data = json.loads(f.read_text(encoding="utf-8"))
        except (ValueError, UnicodeDecodeError) as e:
            print(f"skip {f}: {e}", file=sys.stderr)
            continue
        info = data.get("printerinfo") or {}
        vendor = info.get("vendor") or f.parent.name
        name = value(info, "machinename") or f.stem
        machine_type = value(info, "machinetype") or name
        try:
            rx, ry = int(value(info, "resolutionx")), int(value(info, "resolutiony"))
            sx, sy, sz = (round(float(value(info, k)), 3) for k in ("machinewidth", "machinedepth", "machineheight"))
        except (TypeError, ValueError):
            print(f"skip {f}: incomplete geometry", file=sys.stderr)
            continue
        out_exts = extensions(info.get("output_slice_suffixs"))
        formats = [e for e in SUPPORTED if e in out_exts]
        # every printer can at least take the generic formats for offline use
        if not formats:
            formats = ["zip"]
        projection = int(value(info, "projecttype", 1) or 0)
        # 1 = "LCD mirror" (the default for LCD printers), 0/2 = unmirrored
        mirror = {0: "none", 1: "lcd", 2: "none"}.get(projection, "lcd")
        net_exts = extensions(info.get("net_send_slice_suffixs"))
        network = protocol(vendor, name, net_exts) if value(info, "netsending", False) else None
        pid = slug(f"{vendor} {name}" if not name.lower().startswith(vendor.lower()) else name)
        if pid in seen or f.parent.name == "Custom Printer":
            continue
        seen.add(pid)
        entry = {
            "id": pid,
            "vendor": display_vendor(vendor, name),
            "name": name,
            "resolution_x": rx,
            "resolution_y": ry,
            "size_x": sx,
            "size_y": sy,
            "size_z": sz,
            "mirror": mirror,
            "formats": formats,
            "native_formats": out_exts,
        }
        if network:
            entry["network"] = network
        if machine_type != name:
            entry["machine_type"] = machine_type
        out.append(entry)
    out.sort(key=lambda e: (e["vendor"].lower(), e["name"].lower()))
    Path(dst).parent.mkdir(parents=True, exist_ok=True)
    Path(dst).write_text(json.dumps(out, indent=1, ensure_ascii=False) + "\n", encoding="utf-8")
    print(f"wrote {len(out)} printers to {dst}")


if __name__ == "__main__":
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    main(sys.argv[1], sys.argv[2])

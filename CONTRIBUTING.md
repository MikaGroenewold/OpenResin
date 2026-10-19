# Contributing to OpenResin

Thanks for helping! Bug reports, printer compatibility reports, translations
and code are all welcome.

## Before you start

- **Bugs and ideas:** search the
  [issues](https://github.com/MikaGroenewold/OpenResin/issues) first, then
  open one with the matching template.
- **Printer or file format reports:** these are especially valuable. Use the
  "Printer / file format compatibility" template and say which printer,
  firmware and file format you tried.
- **Larger changes:** open an issue first so we can agree on the approach
  before you spend time on it.
- **Security problems:** do not open a public issue – see
  [SECURITY.md](SECURITY.md).

## Development setup

You need [Rust](https://rustup.rs) (stable, 1.85 or newer) and
[Node.js](https://nodejs.org) 20 or newer. On Linux, install the webview
libraries listed in the [README](README.md#build-from-source).

```bash
git clone https://github.com/MikaGroenewold/OpenResin.git
cd OpenResin/app
npm ci
npm run tauri dev
```

| Path | What lives there |
| --- | --- |
| `crates/core` | geometry, slicer, supports, repair and other model tools |
| `crates/formats` | printer file writers and readers |
| `crates/net` | printer discovery, network protocols, camera |
| `crates/cli` | the `openresin` command line tool |
| `app/src-tauri` | desktop app backend (Tauri commands) |
| `app/src` | desktop app frontend (React + three.js) |

## Tests

Every change should keep the tests green, and new behaviour should come with
tests that check real results (pixels, geometry, bytes on the wire) rather
than only "it doesn't crash".

```bash
cargo test --workspace                                # all Rust crates and the app backend
cargo clippy --workspace --all-targets -- -D warnings
cd app
npm test                                              # frontend
npm run typecheck:test
npx tsc --noEmit
```

Network tests must only talk to simulated printers on `127.0.0.1`. Never
point tests at real printers.

## Guidelines

- Match the style of the surrounding code; keep changes focused.
- User-facing text goes through `t()` / `tr()` in the frontend, with a German
  entry in `app/src/i18n.ts`. The i18n test fails when a translation is
  missing.
- Do not add telemetry, update checks, cloud services or anything else that
  contacts the internet. OpenResin only talks to printers the user connects
  to.
- Protocol and file format work must be clean-room: based on your own
  observation of devices and files you own, or on public documentation. Do
  not copy code or embed keys from proprietary software. Encrypted vendor
  formats stay unsupported.
- When testing against a real printer, never send destructive commands
  (delete, stop, upload) to a printer that isn't yours.
- Printer profiles live in `profiles/printers.json` and contain machine facts
  only (resolution, build size, mirroring, formats).

## Pull requests

1. Fork the repository and create a branch from `main`.
2. Make your change with tests.
3. Run the checks above.
4. Open a pull request and fill in the template.

By contributing you agree that your contributions are licensed under the
[GPL-3.0-or-later](LICENSE), like the rest of the project.

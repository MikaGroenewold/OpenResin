// Reads the Rust sources of the backend (never modifies them) so the tests can
// check that the frontend speaks the same IPC contract: command names, argument
// names (Tauri 2 maps camelCase JS keys to snake_case Rust parameters) and the
// fields of structs/enums that are deserialized from JS objects.

const appSources = import.meta.glob("../../src-tauri/src/*.rs", { query: "?raw", import: "default", eager: true }) as Record<
  string,
  string
>;
const coreSources = import.meta.glob("../../../crates/core/src/**/*.rs", { query: "?raw", import: "default", eager: true }) as Record<
  string,
  string
>;

const allSources = { ...appSources, ...coreSources };

export interface RustParam {
  name: string;
  /** Key the frontend has to use for this parameter. */
  jsName: string;
  type: string;
  optional: boolean;
}

export interface RustCommand {
  name: string;
  file: string;
  params: RustParam[];
  /** Source of the function (signature and body). */
  body: string;
}

export const snakeToCamel = (s: string) => s.replace(/_([a-z0-9])/g, (_, c: string) => c.toUpperCase());
export const pascalToSnake = (s: string) => s.replace(/(?<!^)([A-Z])/g, "_$1").toLowerCase();

/** Splits at top-level commas (ignores commas inside <>, (), []). */
function splitTopLevel(s: string): string[] {
  const out: string[] = [];
  let depth = 0;
  let cur = "";
  for (const ch of s) {
    if ("<([".includes(ch)) depth++;
    if (">)]".includes(ch)) depth--;
    if (ch === "," && depth === 0) {
      out.push(cur);
      cur = "";
    } else cur += ch;
  }
  if (cur.trim()) out.push(cur);
  return out.map((x) => x.trim()).filter(Boolean);
}

const INJECTED = /^(State<|tauri::State<|AppHandle|tauri::AppHandle|Window|tauri::Window|Webview|tauri::Webview|WebviewWindow)/;

export function rustCommands(): Map<string, RustCommand> {
  const out = new Map<string, RustCommand>();
  for (const [file, src] of Object.entries(appSources)) {
    const re = /#\[tauri::command(?:\(([^)]*)\))?\]\s*pub\s+(?:async\s+)?fn\s+(\w+)\s*(?:<[^>{]*>)?\s*\(/g;
    let m: RegExpExecArray | null;
    while ((m = re.exec(src))) {
      const attr = m[1] ?? "";
      const name = m[2];
      // parameter list: up to the matching ')'
      let i = re.lastIndex;
      let depth = 1;
      while (depth > 0 && i < src.length) {
        if (src[i] === "(") depth++;
        if (src[i] === ")") depth--;
        i++;
      }
      const paramSrc = src.slice(re.lastIndex, i - 1);
      const next = src.indexOf("#[tauri::command", i);
      const body = src.slice(m.index, next < 0 ? src.length : next);
      const snake = /rename_all\s*=\s*"snake_case"/.test(attr);
      const params = splitTopLevel(paramSrc)
        .map((p) => {
          const [n, ...rest] = p.split(":");
          return { name: n.replace(/^mut\s+/, "").trim(), type: rest.join(":").trim() };
        })
        .filter((p) => !INJECTED.test(p.type))
        .map((p) => ({ ...p, jsName: snake ? p.name : snakeToCamel(p.name), optional: /^Option</.test(p.type) }));
      out.set(name, { name, file, params, body });
    }
  }
  return out;
}

/** Commands registered in `tauri::generate_handler![...]` (lib.rs). */
export function registeredCommands(): string[] {
  const lib = Object.entries(appSources).find(([f]) => f.endsWith("/lib.rs"))?.[1] ?? "";
  const m = /generate_handler!\[([\s\S]*?)\]/.exec(lib);
  if (!m) throw new Error("generate_handler! not found in app/src-tauri/src/lib.rs");
  return m[1]
    .split(",")
    .map((s) => s.trim())
    .filter(Boolean)
    .map((s) => s.split("::").pop()!);
}

function findItem(kind: "struct" | "enum", name: string): { body: string; attrs: string; file: string } {
  for (const [file, src] of Object.entries(allSources)) {
    const re = new RegExp(`((?:#\\[[^\\]]*\\]\\s*|///[^\\n]*\\n\\s*)*)pub ${kind} ${name}\\s*(?:<[^>]*>)?\\s*\\{`, "g");
    const m = re.exec(src);
    if (!m) continue;
    let i = re.lastIndex;
    let depth = 1;
    while (depth > 0 && i < src.length) {
      if (src[i] === "{") depth++;
      if (src[i] === "}") depth--;
      i++;
    }
    return { body: src.slice(re.lastIndex, i - 1), attrs: m[1], file };
  }
  throw new Error(`pub ${kind} ${name} not found in the Rust sources`);
}

export interface RustField {
  name: string;
  /** May be left out by the frontend (Option, #[serde(default)] on field or struct). */
  optional: boolean;
}

/** Serde field names of a `pub struct` (renames via rename_all are applied). */
export function rustStructFields(name: string): RustField[] {
  const { body, attrs } = findItem("struct", name);
  const structDefault = /#\[serde\([^\]]*\bdefault\b/.test(attrs);
  const rename = /rename_all\s*=\s*"camelCase"/.test(attrs) ? snakeToCamel : (s: string) => s;
  const fields: RustField[] = [];
  const lines = body.split("\n");
  let pendingDefault = false;
  for (const raw of lines) {
    const line = raw.trim();
    if (line.startsWith("///") || line.startsWith("//")) continue;
    if (line.startsWith("#[")) {
      if (/serde\([^)]*\bdefault\b/.test(line)) pendingDefault = true;
      continue;
    }
    const m = /^pub\s+(\w+)\s*:\s*(.+?),?$/.exec(line);
    if (m) {
      fields.push({ name: rename(m[1]), optional: structDefault || pendingDefault || /^Option</.test(m[2]) });
      pendingDefault = false;
    }
  }
  return fields;
}

/** Serialized names of the unit variants of a `pub enum` (snake_case if rename_all says so). */
export function rustEnumVariants(name: string): string[] {
  const { body, attrs } = findItem("enum", name);
  const snake = /rename_all\s*=\s*"snake_case"/.test(attrs);
  return body
    .split("\n")
    .map((l) => l.trim())
    .filter((l) => /^[A-Z]\w*\s*,?$/.test(l))
    .map((l) => l.replace(/,$/, "").trim())
    .map((v) => (snake ? pascalToSnake(v) : v));
}

/** String literals matched by a command's `match x.as_str()` arms ("left" => …). */
export function rustMatchArms(command: string): string[] {
  const cmd = rustCommands().get(command);
  if (!cmd) throw new Error(`command ${command} not found`);
  return [...cmd.body.matchAll(/"([a-z0-9_]+)"\s*=>/g)].map((m) => m[1]);
}

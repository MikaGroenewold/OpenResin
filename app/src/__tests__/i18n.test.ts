// Static check of the translation table: every English string the UI passes to
// t()/tr() (directly, through conditionals, or through lookup tables like the
// ribbon items) must have a German entry, and the table must not contain
// duplicates or dead entries.
import * as ts from "typescript";
import { describe, expect, it } from "vitest";
import { t } from "../i18n";

const raw = import.meta.glob(["../**/*.{ts,tsx}", "!../__tests__/**"], { query: "?raw", import: "default", eager: true }) as Record<
  string,
  string
>;
const files = new Map<string, ts.SourceFile>();
for (const [path, text] of Object.entries(raw)) {
  const name = path.replace(/^\.\.\//, "");
  files.set(name, ts.createSourceFile(name, text, ts.ScriptTarget.Latest, true, name.endsWith(".tsx") ? ts.ScriptKind.TSX : ts.ScriptKind.TS));
}

// ---------------------------------------------------------------- dictionary

interface Entry {
  key: string;
  value: string;
  line: number;
}

function dictionary(): Entry[] {
  const sf = files.get("i18n.ts")!;
  let obj: ts.ObjectLiteralExpression | undefined;
  sf.forEachChild(function walk(n): void {
    if (ts.isVariableDeclaration(n) && n.name.getText() === "de" && n.initializer && ts.isObjectLiteralExpression(n.initializer)) obj = n.initializer;
    else n.forEachChild(walk);
  });
  if (!obj) throw new Error("`const de = { … }` not found in i18n.ts");
  return obj.properties.map((p) => {
    if (!ts.isPropertyAssignment(p) || !ts.isStringLiteralLike(p.initializer)) throw new Error(`unexpected entry ${p.getText()}`);
    const key = ts.isIdentifier(p.name) || ts.isStringLiteralLike(p.name) ? p.name.text : p.name.getText();
    return { key, value: p.initializer.text, line: sf.getLineAndCharacterOfPosition(p.getStart()).line + 1 };
  });
}

// ---------------------------------------------------------------- usage scan

const strip = (e: ts.Expression): ts.Expression => {
  while (ts.isParenthesizedExpression(e) || ts.isAsExpression(e) || ts.isNonNullExpression(e) || ts.isSatisfiesExpression(e))
    e = e.expression;
  return e;
};

/** Initializer of a `const name = …` in the file (or the file it is imported from). */
function constInit(sf: ts.SourceFile, name: string): { init: ts.Expression; sf: ts.SourceFile } | undefined {
  let found: { init: ts.Expression; sf: ts.SourceFile } | undefined;
  let importedFrom: string | undefined;
  sf.forEachChild(function walk(n): void {
    if (found) return;
    if (ts.isVariableDeclaration(n) && ts.isIdentifier(n.name) && n.name.text === name && n.initializer) found = { init: strip(n.initializer), sf };
    else if (ts.isImportDeclaration(n) && n.importClause?.namedBindings && ts.isNamedImports(n.importClause.namedBindings)) {
      if (n.importClause.namedBindings.elements.some((e) => e.name.text === name)) importedFrom = (n.moduleSpecifier as ts.StringLiteral).text;
    } else n.forEachChild(walk);
  });
  if (found || !importedFrom) return found;
  const dir = sf.fileName.includes("/") ? sf.fileName.slice(0, sf.fileName.lastIndexOf("/")) : "";
  const parts = [...(dir ? dir.split("/") : []), ...importedFrom.split("/")];
  const norm: string[] = [];
  for (const p of parts) {
    if (p === "..") norm.pop();
    else if (p !== ".") norm.push(p);
  }
  const target = files.get(`${norm.join("/")}.tsx`) ?? files.get(`${norm.join("/")}.ts`);
  return target ? constInit(target, name) : undefined;
}

/** Elements a `.map(cb)` callback parameter can take, for the identifier `name` bound in `cb`. */
function mapBinding(node: ts.Node, name: string): { values: ts.Expression[]; sf: ts.SourceFile } | undefined {
  for (let p: ts.Node | undefined = node.parent; p; p = p.parent) {
    if (!(ts.isArrowFunction(p) || ts.isFunctionExpression(p))) continue;
    const param = p.parameters[0];
    if (!param) continue;
    let index: number | null = null;
    if (ts.isIdentifier(param.name) && param.name.text === name) index = -1;
    else if (ts.isArrayBindingPattern(param.name))
      param.name.elements.forEach((el, i) => {
        if (ts.isBindingElement(el) && ts.isIdentifier(el.name) && el.name.text === name) index = i;
      });
    if (index === null) continue;
    const call = p.parent;
    if (!call || !ts.isCallExpression(call) || !ts.isPropertyAccessExpression(call.expression) || call.expression.name.text !== "map") return undefined;
    let arr: ts.Expression = strip(call.expression.expression);
    let sf = node.getSourceFile();
    if (ts.isIdentifier(arr)) {
      const c = constInit(sf, arr.text);
      if (!c) return undefined;
      arr = c.init;
      sf = c.sf;
    }
    if (!ts.isArrayLiteralExpression(arr)) return undefined;
    const i = index as number;
    const values = arr.elements.map((el) => {
      const e = strip(el);
      return i < 0 ? e : ts.isArrayLiteralExpression(e) ? strip(e.elements[i]) : e;
    });
    return { values, sf };
  }
  return undefined;
}

interface Resolved {
  strings: string[];
  unresolved: ts.Expression[];
}

function resolve(e: ts.Expression): Resolved {
  e = strip(e);
  if (ts.isStringLiteralLike(e)) return { strings: [e.text], unresolved: [] };
  const both = (a: ts.Expression, b: ts.Expression): Resolved => {
    const x = resolve(a);
    const y = resolve(b);
    return { strings: [...x.strings, ...y.strings], unresolved: [...x.unresolved, ...y.unresolved] };
  };
  if (ts.isConditionalExpression(e)) return both(e.whenTrue, e.whenFalse);
  if (ts.isBinaryExpression(e) && (e.operatorToken.kind === ts.SyntaxKind.QuestionQuestionToken || e.operatorToken.kind === ts.SyntaxKind.BarBarToken))
    return both(e.left, e.right);
  // TABLE[key] → every value of the object literal TABLE
  if (ts.isElementAccessExpression(e) && ts.isIdentifier(strip(e.expression))) {
    const id = strip(e.expression) as ts.Identifier;
    const c = constInit(e.getSourceFile(), id.text);
    if (c && ts.isObjectLiteralExpression(c.init)) {
      const out: Resolved = { strings: [], unresolved: [] };
      for (const p of c.init.properties) {
        if (ts.isPropertyAssignment(p)) {
          const r = resolve(p.initializer);
          out.strings.push(...r.strings);
          out.unresolved.push(...r.unresolved);
        }
      }
      return out;
    }
    // info[g] where info is declared inside the component
    const local = localConst(e, id.text);
    if (local && ts.isObjectLiteralExpression(local)) return resolveObjectValues(local);
  }
  // callback parameter of `[…].map((x) => …)` / `.map(([a, b]) => …)`
  if (ts.isIdentifier(e)) {
    const b = mapBinding(e, e.text);
    if (b) return b.values.map(resolve).reduce((a, r) => ({ strings: [...a.strings, ...r.strings], unresolved: [...a.unresolved, ...r.unresolved] }), { strings: [], unresolved: [] } as Resolved);
  }
  // x.label where x iterates over an array of object literals
  if (ts.isPropertyAccessExpression(e) && ts.isIdentifier(e.expression)) {
    const b = mapBinding(e, e.expression.text);
    if (b) {
      const out: Resolved = { strings: [], unresolved: [] };
      for (const v of b.values) {
        if (!ts.isObjectLiteralExpression(v)) return { strings: [], unresolved: [e] };
        const prop = v.properties.find((p) => ts.isPropertyAssignment(p) && p.name.getText() === e.name.text) as ts.PropertyAssignment | undefined;
        if (prop) {
          const r = resolve(prop.initializer);
          out.strings.push(...r.strings);
          out.unresolved.push(...r.unresolved);
        }
      }
      return out;
    }
  }
  return { strings: [], unresolved: [e] };
}

function resolveObjectValues(o: ts.ObjectLiteralExpression): Resolved {
  const out: Resolved = { strings: [], unresolved: [] };
  for (const p of o.properties) {
    if (!ts.isPropertyAssignment(p)) continue;
    const r = resolve(p.initializer);
    out.strings.push(...r.strings);
    out.unresolved.push(...r.unresolved);
  }
  return out;
}

/** `const name = …` declared in an enclosing function body. */
function localConst(node: ts.Node, name: string): ts.Expression | undefined {
  for (let p: ts.Node | undefined = node.parent; p; p = p.parent) {
    if (!ts.isBlock(p)) continue;
    for (const s of p.statements) {
      if (!ts.isVariableStatement(s)) continue;
      for (const d of s.declarationList.declarations)
        if (ts.isIdentifier(d.name) && d.name.text === name && d.initializer) return strip(d.initializer);
    }
  }
  return undefined;
}

/** Call sites the generic resolver cannot follow, resolved by hand (file|expression). */
const SPECIAL: Record<string, (call: ts.CallExpression) => string[] | "ignore"> = {
  // items(tab) builds the list at runtime: every `label:` of an item object in the file
  "views/Ribbon.tsx|it.label": (call) => labelProps(call.getSourceFile()),
  // the toolbar's accessible name uses the label of the active RIBBON_TABS entry
  "views/Ribbon.tsx|tabLabel": (call) => labelProps(call.getSourceFile()),
  // test shapes: ["cube", …].map((k) => tr(k[0].toUpperCase() + k.slice(1)))
  "views/ModelView.tsx|k[0].toUpperCase() + k.slice(1)": (call) => {
    const b = mapBinding(call, "k")!;
    return b.values.map((v) => (v as ts.StringLiteral).text).map((k) => k[0].toUpperCase() + k.slice(1));
  },
  // raw machine state as fallback for states the UI does not know yet
  "views/DevicesView.tsx|state": () => "ignore",
};

function labelProps(sf: ts.SourceFile): string[] {
  const out: string[] = [];
  sf.forEachChild(function walk(n): void {
    if (ts.isPropertyAssignment(n) && n.name.getText() === "label" && ts.isStringLiteralLike(n.initializer)) out.push(n.initializer.text);
    n.forEachChild(walk);
  });
  return out;
}

interface Use {
  key: string;
  where: string;
  /** Names passed in the second argument of t(text, { … }); null = no second argument. */
  vars: string[] | null;
}

function varNames(call: ts.CallExpression): string[] | null {
  const a = call.arguments[1];
  if (!a) return null;
  if (!ts.isObjectLiteralExpression(a)) throw new Error(`t() variables must be an object literal: ${a.getText()}`);
  return a.properties.map((p) => p.name?.getText() ?? "");
}

function scan(): { uses: Use[]; unresolved: string[]; specialsUsed: Set<string> } {
  const uses: Use[] = [];
  const unresolved: string[] = [];
  const specialsUsed = new Set<string>();
  for (const [name, sf] of files) {
    if (name === "i18n.ts") continue;
    // local names of the translation function (t, or `t as tr`)
    const fns = new Set<string>();
    sf.forEachChild((n) => {
      if (ts.isImportDeclaration(n) && /\/i18n$/.test((n.moduleSpecifier as ts.StringLiteral).text)) {
        const nb = n.importClause?.namedBindings;
        if (nb && ts.isNamedImports(nb))
          for (const el of nb.elements) if ((el.propertyName ?? el.name).text === "t") fns.add(el.name.text);
      }
    });
    if (!fns.size) continue;
    sf.forEachChild(function walk(n): void {
      if (ts.isCallExpression(n) && ts.isIdentifier(n.expression) && fns.has(n.expression.text) && n.arguments.length) {
        const where = `${name}:${sf.getLineAndCharacterOfPosition(n.getStart()).line + 1}`;
        const r = resolve(n.arguments[0]);
        const vars = varNames(n);
        for (const s of r.strings) uses.push({ key: s, where, vars });
        for (const u of r.unresolved) {
          const id = `${name}|${u.getText()}`;
          const special = SPECIAL[id];
          if (!special) {
            unresolved.push(`${where}  ${u.getText()}`);
            continue;
          }
          specialsUsed.add(id);
          const v = special(n);
          if (v !== "ignore") for (const s of v) uses.push({ key: s, where, vars });
        }
      }
      n.forEachChild(walk);
    });
  }
  return { uses, unresolved, specialsUsed };
}

// ---------------------------------------------------------------- visible text outside t()

/** JSX attributes whose value is shown to (or read out for) the user. */
const TEXT_ATTRS = new Set(["title", "aria-label", "placeholder", "alt", "label"]);
/** Words that read the same in every language of the UI (units, names, language codes). */
const NEUTRAL_WORDS = new Set(["mm", "ml", "px", "SDCP", "EN", "DE", "English", "Deutsch"]);

const placeholders = (s: string) => [...s.matchAll(/\{(\w+)\}/g)].map((m) => m[1]).sort();

function hasWords(text: string): boolean {
  const words = text.match(/[A-Za-zÄÖÜäöüß]{2,}/g) ?? [];
  return words.some((w) => !NEUTRAL_WORDS.has(w));
}

/** String parts of an expression that end up on screen without going through t(). */
function untranslated(e: ts.Expression): ts.Node[] {
  e = strip(e);
  if (ts.isStringLiteralLike(e)) return hasWords(e.text) ? [e] : [];
  if (ts.isTemplateExpression(e)) {
    const statics = [e.head.text, ...e.templateSpans.map((s) => s.literal.text)].join(" ");
    return [...(hasWords(statics) ? [e] : []), ...e.templateSpans.flatMap((s) => untranslated(s.expression))];
  }
  if (ts.isConditionalExpression(e)) return [...untranslated(e.whenTrue), ...untranslated(e.whenFalse)];
  if (ts.isBinaryExpression(e)) {
    const k = e.operatorToken.kind;
    if (k === ts.SyntaxKind.AmpersandAmpersandToken) return untranslated(e.right);
    if (k === ts.SyntaxKind.QuestionQuestionToken || k === ts.SyntaxKind.BarBarToken || k === ts.SyntaxKind.PlusToken)
      return [...untranslated(e.left), ...untranslated(e.right)];
  }
  // calls (t() itself, formatters, .map/.join over data) and nested JSX are checked on their own
  return [];
}

/** Visible English text that bypasses t(), as "file:line  text". */
function untranslatedText(): string[] {
  const out: string[] = [];
  for (const [name, sf] of files) {
    if (name === "i18n.ts") continue;
    const report = (n: ts.Node, text: string) =>
      out.push(`${name}:${sf.getLineAndCharacterOfPosition(n.getStart()).line + 1}  ${text.trim().replace(/\s+/g, " ")}`);
    const check = (e: ts.Expression) => {
      for (const n of untranslated(e)) report(n, n.getText());
    };
    sf.forEachChild(function walk(n): void {
      // text between tags
      if (ts.isJsxText(n) && hasWords(n.text)) report(n, n.text);
      // {expression} children
      if (ts.isJsxExpression(n) && n.expression && (ts.isJsxElement(n.parent) || ts.isJsxFragment(n.parent))) check(n.expression);
      // visible attributes
      if (ts.isJsxAttribute(n) && TEXT_ATTRS.has(n.name.getText()) && n.initializer) {
        if (ts.isStringLiteral(n.initializer)) {
          if (hasWords(n.initializer.text)) report(n, n.getText());
        } else if (ts.isJsxExpression(n.initializer) && n.initializer.expression) check(n.initializer.expression);
      }
      // messages shown as toast, confirmation, busy label or error
      if (ts.isCallExpression(n)) {
        const e = n.expression;
        const callee = ts.isPropertyAccessExpression(e) ? e.name.text : ts.isIdentifier(e) ? e.text : "";
        const arg = ({ toast: 1, confirmDialog: 0, run: 0, setBusy: 0 } as Record<string, number>)[callee];
        if (arg !== undefined && n.arguments[arg]) check(n.arguments[arg]);
      }
      if (ts.isNewExpression(n) && n.expression.getText() === "Error" && n.arguments?.[0]) check(n.arguments[0]);
      n.forEachChild(walk);
    });
  }
  return out;
}

// ---------------------------------------------------------------- tests

describe("German translation table", () => {
  const dict = dictionary();
  const de = new Map(dict.map((e) => [e.key, e.value]));
  const { uses, unresolved, specialsUsed } = scan();

  it("finds the translated strings of the UI", () => {
    // sanity: the scan sees direct calls, conditionals, tables and ribbon labels
    const keys = new Set(uses.map((u) => u.key));
    for (const k of ["Repair", "Upload & print", "Fewest supports", "Receiving file", "Lay Flat", "Cube", "Click a support to remove it", "Move it on the plate", "Lift & retract", "Plate center", "Inside-out parts"])
      expect(keys).toContain(k);
    expect(uses.length).toBeGreaterThan(300);
  });

  it("can resolve every t()/tr() argument (register new dynamic call sites in SPECIAL)", () => {
    expect(unresolved).toEqual([]);
    expect([...specialsUsed].sort()).toEqual(Object.keys(SPECIAL).sort());
  });

  it("has a German entry for every translated string", () => {
    const missing = [...new Set(uses.filter((u) => !de.has(u.key)).map((u) => `${JSON.stringify(u.key)}  (${u.where})`))];
    expect(missing).toEqual([]);
  });

  it("has no duplicate keys", () => {
    const seen = new Map<string, number>();
    const dups: string[] = [];
    for (const e of dict) {
      if (seen.has(e.key)) dups.push(`${JSON.stringify(e.key)} (lines ${seen.get(e.key)} and ${e.line})`);
      seen.set(e.key, e.line);
    }
    expect(dups).toEqual([]);
  });

  it("has no empty or dead entries", () => {
    expect(dict.filter((e) => !e.value.trim()).map((e) => e.key)).toEqual([]);
    const used = new Set(uses.map((u) => u.key));
    expect(dict.filter((e) => !used.has(e.key)).map((e) => e.key)).toEqual([]);
  });

  it("keeps placeholders and punctuation consistent", () => {
    const bad = dict.filter(
      (e) =>
        e.key.endsWith("…") !== e.value.endsWith("…") ||
        e.key.endsWith("?") !== e.value.endsWith("?") ||
        placeholders(e.key).join() !== placeholders(e.value).join(),
    );
    expect(bad.map((e) => `${e.key} → ${e.value}`)).toEqual([]);
  });

  it("passes every {placeholder} of a key to t()", () => {
    const bad = uses
      .filter((u) => placeholders(u.key).length || u.vars)
      .filter((u) => placeholders(u.key).join() !== [...(u.vars ?? [])].sort().join())
      .map((u) => `${u.where}  ${JSON.stringify(u.key)} gets {${(u.vars ?? []).join(", ")}}`);
    expect([...new Set(bad)]).toEqual([]);
  });
});

describe("visible text", () => {
  it("goes through t() (JSX text, visible attributes, toasts, confirmations, busy labels, errors)", () => {
    expect(untranslatedText()).toEqual([]);
  });

  it("the check catches English outside t()", () => {
    const src = [
      'const a = <p title="Open file">Layer {n} of {m}</p>;',
      "const b = <Field label={`Size ${x}`} unit=\"mm\" />;",
      'toast("info", ok ? "Saved" : tr("Failed"));',
      'const c = <span className="muted">{n} mm · {tr("files")} {d.name ?? "Unnamed"}</span>;',
    ].join("\n");
    files.set("probe.tsx", ts.createSourceFile("probe.tsx", src, ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX));
    try {
      const found = untranslatedText()
        .filter((l) => l.startsWith("probe.tsx"))
        .map((l) => l.replace(/^probe\.tsx:\d+ {2}/, ""));
      expect(found).toEqual(['title="Open file"', "Layer", "of", "`Size ${x}`", '"Saved"', '"Unnamed"']);
    } finally {
      files.delete("probe.tsx");
    }
  });
});

describe("t() in English", () => {
  it("returns the English text unchanged", () => {
    expect(t("Repair")).toBe("Repair");
    expect(t("Not in the table")).toBe("Not in the table");
  });

  it("fills {placeholders}", () => {
    expect(t("{n} layers", { n: 12 })).toBe("12 layers");
    expect(t("Layer {n}", { n: 1 })).toBe("Layer 1");
    expect(t("{a} and {b}", { a: "x" })).toBe("x and {b}");
  });
});

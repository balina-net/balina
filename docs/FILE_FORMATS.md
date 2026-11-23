# File Formats

All formats exchange `bn_core::io::Document { network, visual }` — visual
metadata lives in bn-core (not the app) precisely so positions can round-trip
through XDSL. Each format has its own DTO layer; **runtime types never derive
Serialize directly**, so the model can evolve without breaking files.

Dispatch: `io::load(path)` / `io::save(doc, path)` pick the format from the
extension (`Format::from_path`): `.balina`/`.json` → native, `.xml`/`.bif`/
`.xmlbif` → XMLBIF, `.xdsl` → XDSL, `.balpck` → `Format::Pack` (binary:
the text `load_str`/`save_str` reject it; see the pack section).
`load_str`/`save_str` exist for tests.
`save` returns `Vec<Warning>` — currently only `Warning::Lossy(String)`,
which the app prints to the message log.

**Shared value ordering**: all three formats list table values with parents
as outer axes (declared order) and the node itself varying fastest — exactly
bn-core's memory layout, so table data is copied verbatim. If you add a
format, verify its ordering convention first; this is the classic
interchange bug.

## Native `.balina` (io/native.rs)

Versioned JSON:

```json
{
  "format": "balina-project", "version": 1,
  "uid": "3f9a1c07e2b4d815",      // project identity (ties the .baltsv to it)
  "active_tab": 0,
  "tabs": [
    {
      "label": "Asia", "uid": "9c41e0d27a3b5f60",   // tab identity (keys its data)
      "name": "Chest Clinic (Asia)", "comment": "",
      "nodes": [
        { "name": "VisitAsia", "title": "", "kind": "Chance",
          "states": [{"name": "yes", "value": null}, {"name": "no", "value": null}],
          "parents": [],            // parent NAMES, in table-axis order
          "table": [0.01, 0.99],    // flat, layout as above
          "experience": null, "comment": "" },
        ...
      ],
      "visual": { "VisitAsia": {"x": 40.0, "y": 40.0, "display": "BeliefBars", "color": null,
                                "precision": 2}, ... },  // decimals (0..=8), per NODE
      "notes": [],
      "precision": 2              // legacy tab-level decimals: seed for nodes
                                  // without a per-node value (pre-per-node
                                  // files); still written for old readers
    }
  ]
}
```

- Nodes are written in **topological order** so loading is two passes:
  create all nodes, then add edges (in stored parent order — `add_edge`
  appends parents, so order is preserved) and set tables. Note `add_edge`
  reshapes tables, which is why tables are set **after** edges.
- Everything non-essential is `#[serde(default)]` — forward compatible.
- Visual is keyed by node **name** (slotmap keys are meaningless across
  sessions). Rename-then-save is fine because saving re-derives the map from
  live NodeIds.
- XMLBIF/XDSL drop per-node `precision` silently, like `display`/`color`.
- `uid`s are 16 hex digits. Files saved before they existed load with
  empty ones; the session generates them silently (not a modification), so
  they appear on the next save. New and duplicated tabs get fresh uids;
  Save As keeps the project's. Both are omitted when empty.
- Not yet persisted: evidence, camera, net-level window state (ROADMAP).

## XMLBIF v0.3 (io/xmlbif.rs)

- Chance-only format. Decision nodes are written as chance nodes with a
  uniform TABLE; utility nodes are **skipped**; both emit `Warning::Lossy`.
- `<VARIABLE>` holds `<NAME>`, `<OUTCOME>`s, and layout as
  `<PROPERTY>position = (x, y)</PROPERTY>` (JavaBayes convention).
- `<DEFINITION>` holds `<FOR>`, `<GIVEN>`s (= parent order), `<TABLE>`
  (whitespace-separated floats).
- Parser is quick-xml **manual events** (no serde feature — XMLBIF's mixed
  content fights serde). It tracks an element path stack and two builder
  structs; `<PROBABILITY>` is accepted as an alias for `<DEFINITION>`.
  Writing is plain string building with an `esc()` helper.

## XDSL / GeNIe (io/xdsl.rs)

- Full influence-diagram support: `<cpt>`, `<decision>`, `<utility>`
  (+ `<deterministic>` parsed as cpt) inside `<nodes>`; each has
  `<state id=…/>`s, `<parents>` (space-separated ids), and
  `<probabilities>`/`<utilities>`.
- Layout and titles live in `<extensions><genie><node id=…><name>` and
  `<position>x1 y1 x2 y2</position>` (we store x1 y1 and write x1+120, y1+60
  as the second corner).
- XDSL ids must be identifier-like: `ident()` sanitizes names on write
  (non-alphanumerics → `_`, leading digit prefixed). Loading a file we wrote
  therefore may have different names than the in-memory net it came from —
  round-trip tests compare *semantics* (beliefs/MEU), not names.
- Net name comes from the `<smile id=…>` attribute.

## Case files (learn/cases.rs — CSV, not a network format)

Header row of node names (case-insensitive match); `IDnum` column ignored;
`NumCases`/`__count` column = case weight; missing markers: empty, `*`, `?`,
`NA`. Values are state names or bare indices. `write_cases` emits state
names and adds `NumCases` only when weights are non-uniform.

## Data sidecar `.baltsv` (io/baltsv.rs)

The case data attached to tabs (by Learn CPTs, Learn Structure, Simulate or
Cases > Attach Data). Desktop saves of `<stem>.balina` write it as
`<stem>.baltsv` beside the project (only when the data changed or the file is
missing; atomically via `<stem>.baltsv.tmp` + rename), and opening a
`.balina` attaches a sidecar whose project uid matches. With no data left, a
sidecar with this project's uid is deleted on save. XMLBIF/XDSL saves never
write one (a warning says so).

UTF-8 text, LF line ends (CRLF accepted), no BOM. Lines starting with `#`
are structure, every other line is data; fields are TAB-separated (shown as
`⇥`):

```
#BALTSV⇥1
#project⇥3f9a1c07e2b4d815
#section⇥9c41e0d27a3b5f60⇥offset=0000000000000431⇥bytes=0000000000183920⇥rows=5000⇥cols=8⇥origin=learn-cpts⇥source=asia.csv⇥created=2026-11-23T10:04:00Z⇥name=Asia
#end-header
#====⇥9c41e0d27a3b5f60⇥Asia
VisitAsia⇥Smoking⇥…        ← column names (not counted in rows)
yes⇥no⇥…
```

- One `#section` line per tab with data: tab uid, then `key=value` fields.
  `offset` (from the start of the file) points at the section's `#====`
  marker line; `bytes` spans the marker through the last row. Both are
  zero-padded to 16 digits so the header's size is known before writing.
  `origin` is `learn-cpts` / `learn-structure` / `simulate` / `attach`;
  `source` (file name), `created` (RFC 3339 UTC) and `name` (the tab name,
  informational) are optional. Unknown keys are ignored.
- Valid = magic line with a version ≤ 1, and (for discovery) a matching
  project uid. The sections must tile the file exactly (each starts where
  the previous one ended, the last ends at EOF) with every offset on its
  marker; otherwise (a hand edit changed some section's length) the reader
  rebuilds the section table by scanning for `#====` lines and warns.
- Cells are the **raw** source values (continuous values, `IDnum`,
  `NumCases`, columns matching no node — all kept), parsed exactly like
  learning parses them (sniffed/forced delimiter, BOM stripped, trimmed);
  missing markers stay verbatim. Escapes: `\t \n \r \\`, and a line
  whose first cell starts with `#` writes it as `\#`. Header values use the
  same escapes.
- No row index is stored: readers build a sparse in-memory index (byte
  offset every 256 rows) to fetch row windows on demand.

## Pack `.balpck` (io/pack.rs)

A zip for sharing a project with its data (the only way the web build keeps
data). Entries in order: `mimetype` (stored, `application/x-balina-pack`,
ODF-style so the file is recognizable from its first bytes),
`project.balina` (the native JSON) and, when any tab has data,
`data.baltsv` (both deflated, `zip` crate with the pure-Rust zlib-rs
backend so it builds for wasm). Re-saving a pack whose data did not change
copies `data.baltsv` raw, without recompressing.

## Round-trip guarantees (enforced in tests/feature_tests.rs)

- native: `save(load(save(x)))` byte-stable; beliefs equal to 1e-12.
- XMLBIF: beliefs equal to 1e-12 (chance-only nets).
- XDSL: MEU and optimal policies equal on influence diagrams.
Float noise ~1e-16 after reload is expected (different summation order from a
different topo order) — compare with tolerance, never exact equality.

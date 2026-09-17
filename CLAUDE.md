# CLAUDE.md

Viti is a native GTK 4 / libadwaita MongoDB client in Rust: a port of MongoDB Compass
with vi-mongo keyboard navigation, a `:` command line, documents edited in an external
editor (nvim) inside an embedded terminal, and Compass's AI features replaced by shelling
out to `claude -p` / `codex`. App id `dev.turbinebmw.Viti`.

## Build & run

No Flatpak, no meson. Plain cargo.

```
cargo run                                   # dev build
cargo clippy --all-targets -- -D warnings   # keep clean (dead_code is allowed crate-wide until later phases land)
cargo fmt --check && cargo test
sh install.sh                               # ~/.local/bin/viti + .desktop + icons
```

Host deps: gtk4 ≥ 4.18, libadwaita ≥ 1.7, gtksourceview-5, vte-2.91-gtk4 ≥ 0.78.
Dev database: `docker run -d --name viti-mongo -p 27017:27017 mongo:7`, then
`docker exec -i viti-mongo mongosh --quiet < scripts/seed.js` (creates `viti_test`).

Dev hooks: `viti mongodb://localhost:27017` saves a profile and connects on launch;
`VITI_DEBUG_OPEN=viti_test.people[,viti_test.orders…]` opens those collections once
connected (one tab each); `VITI_DEBUG_PIPELINE='[{ $match: {} }]'` loads and runs a pipeline on the first tab's
Aggregations page, `VITI_DEBUG_ACTION=id,id` runs action ids and
`VITI_DEBUG_COMMAND=index;update {…}` runs `:` lines (`schema`, `validation`,
`export csv`, `import /path.csv` open those pages / dialogs), in that order, 1.5 s after that;
`VITI_LOG=viti=debug` turns logging up (tracing env-filter syntax).
`VITI_TEST_URI=mongodb://localhost:27017 cargo test live` runs the server round-trip
tests in `mongo/ops.rs` and `mongo/export.rs` (export → import → validation; each creates
and drops a throwaway `viti_it_*` database).

## Layout

```
src/
  main.rs        tracing, adw::Application, `viti [uri]`
  app.rs         Rc<App>: all widget handles + state, run_action (vi keys), run_command (:), editor round-trip,
                 long ops (import/export in the banner with Cancel: abort + killOp + remove partial file)
  dispatch.rs    window key controllers: chords (ShortcutController) + normal-mode keys (EventControllerKey)
  focus.rs       Scope enum; which pane owns the vi keys; `.viti-focused`; Tab cycling
  keybinds.rs    ACTIONS registry (id, title, default accel, scope, text_safe); overrides; keymap
  commands.rs    `:` grammar: parse() + complete(); pure, tested
  config.rs      ~/.config/viti/{config,connections,queries,pipelines,keybindings}.json; #[serde(default)], atomic writes
  secrets.rs     passwords: secret-service keyring, secrets.json (0600) fallback
  style.rs       BUILTIN css (APPLICATION priority) + user style.css (USER priority, hot-reloaded)
  ai.rs          AI backends (claude -p / codex exec / custom argv): Task (Query, Pipeline, ExplainPlan,
                 IndexSuggest), prompt builder with the schema summary, tokio::process run (stdin prompt,
                 120 s timeout, kill on drop), claude JSON result extraction, brace-balanced JSON scan,
                 parse_response -> Query / pipeline text / explanation / index suggestions; tested
  query_complete.rs  query bar completions: merge_fields(docs) -> dotted paths + types; complete(text, caret,
                 fields, Kind) -> field / operator / constructor (`ObjectId("…")`) replacements; apply(); tested
  accent.rs      GSettings accent fallback for non-GNOME portals (copied from Rustle)
  notify.rs      fdo D-Bus desktop notifications (copied from Bubo)
  events.rs      Event bus from tokio to the GTK thread
  export_to_language.rs  filter/pipeline -> C#/Go/Java/Node/PHP/Python/Ruby/Rust literals (+ driver code); golden tests
  rt.rs          the tokio runtime; `rt::io(fut).await` from glib-local code
  mongo/
    mod.rs       connect() (through the SSH tunnel if any); Conn { client, profile, server, tunnel }
    profile.rs   URI password split/redact; UriParts parse/rebuild (the form and the URI entry edit it)
    tunnel.rs    `ssh -N -L` child per connection, one forward per host; killed on drop
    ejson.rs     loose (mongosh-style) parser -> BSON; pretty Extended JSON; type names/summaries; tested
    ops.rs       every server op: list, find_page, count, CRUD, create/drop/rename db+coll+view,
                 coll_stats, indexes (list+$indexStats, create, drop, hide), update_many/delete_many,
                 preview_update (transaction -> aggregation -> client emulation), server_info, kill_by_comment,
                 aggregate (AggOpts), explain_find / explain_aggregate (VERBOSITIES)
    update_preview.rs  client-side $set/$unset/$inc/… emulation for previews on standalone servers; tested
    pipeline.rs  Stage { operator, body text, enabled } + Pipeline text/BSON conversions, STAGES catalogue with templates; tested
    explain.rs   explain output -> PlanNode tree + Summary (classic, SBE planNodeId, `stages` chain, sharded); tested
    schema.rs    analyze(docs) -> Schema { fields: path, presence, types %, nested children, array elements };
                 chart() -> Bars (top values, booleans, few numbers) | Histogram (numbers, dates); value/range filters; tested
    export.rs    JsonMode (Default keeps $numberLong / Relaxed / Canonical), JsonWriter (array | NDJSON), CSV
                 (flattened dotted paths incl. array indexes, formula escaping), Source (Full | Find | Aggregate),
                 run() streams a cursor to a file with progress; tested + live test
    import.rs    JSON (array / NDJSON / single doc) and CSV (FieldType per column guessed from a preview, dotted
                 headers -> nested, ignore empty, stop on error); run() inserts in batches -> Report; tested
    validation.rs  fetch/set the validator via listCollections / collMod; json_schema(Schema) -> $jsonSchema; tested
    perf.rs      Performance model: serverStatus -> Snapshot, Rates between two, $currentOp -> CurrentOp (own
                 polls / heartbeats filtered), top -> hottest collections; tested
  ui/
    mod.rs       json_view (sourceview), confirm dialogs, helpers
    ai.rs        AI entry points: `Ctrl+I` / `:ai` / the AI buttons -> request dialog -> context (Schema page's
                 analysis or a fresh sample, indexes, explain output) -> backend as a cancellable long op ->
                 query bar filled / pipeline replaced / explanation card / suggestions dialog (Create… prefills)
    performance.rs  PerformancePane: one tab per connection (`:perf`, Ctrl+Shift+P, sidebar menu); 1 Hz
                 serverStatus + $currentOp + top; cairo line charts, hottest collections, slowest ops with
                 `o` details and `Ctrl+D` killOp, `space` pause
    window.rs    chrome: ToastOverlay > Banner > OverlaySplitView(sidebar | TabView) > Paned(editor pane) > cmdline
    sidebar.rs   one Section per connection (header + own scrolled TreeListModel of Db/Coll); only the active one expands; children loaded lazily into ListStores
    connections.rs  profile editor (General/Auth/TLS/SSH/Advanced pages two-way synced with the
                 URI entry) + manager (Ctrl+O) with import/export (reads Compass exports)
    collection.rs   one tab: ViewStack of Documents + Aggregations + Schema + Explain + Indexes + Validation
    aggregation.rs  AggregationPane: stage cards (operator dropdown, editor, switch, live output preview),
                 text mode, results paged below, focus mode dialog, save/open pipelines, create view,
                 export to language, `Ctrl+E` external editor (JobKind::Pipeline)
    explain.rs   ExplainPane: query or pipeline source, verbosity, stat tiles, plan tree (TreeListModel)
                 with per-node details, raw JSON
    export_lang.rs  "Export to language" dialog (language dropdown, driver-code toggle, copy)
    export.rs    "Export" dialog (`X`): source (full / query / aggregation), JSON flavour + NDJSON or CSV fields
                 (sampled) + delimiter + escaping; file chooser; runs as a long op
    import.rs    "Import" dialog (`I`): file, format, CSV column rows (include + type dropdown), options; report dialog
    schema.rs    SchemaPane: sample ($sample, falls back to find) + analyse; one row per field with a DrawingArea
                 chart; hover caption, click / `h` `l` + Enter filter the Documents page; `o` details; `C` copy $jsonSchema
    validation.rs  ValidationPane: rules editor (sourceview), action/level dropdowns, passing/failing samples,
                 generate from the Schema page's analysis (or a fresh sample), `Ctrl+E` external editor (JobKind::Validation)
    documents/   DocumentsPane (state, paging, CRUD, cancel) + list/json/table views over one shared model
    indexes.rs   IndexesPane: ColumnView of IndexInfo, create dialog, hide/unhide, drop (typed confirm)
    manage.rs    create database/collection/view (capped, time-series, clustered, collation), drop, rename
    bulk.rs      bulk update (count + before/after preview) and bulk delete dialogs
    my_queries.rs  favourites + saved pipelines across namespaces (Ctrl+Shift+Y); App::run_saved_query /
                 run_saved_pipeline / toggle_favourite
    query_bar.rs the single toolbar row (filter, history, options toggle, Find/Stop) + completers on the entries
                 + options revealer + history popover; the pane adds its view switcher,
                 pager and ⋮ menu to `row`; emits Query
    editor_pane.rs  VTE: external editor jobs (temp EJSON file) and mongosh
    completer.rs completion popover on an Entry (fields from the pages seen so far, `$ops`, constructors):
                 Tab accepts, Up/Down move, Enter accepts only after moving, Esc dismisses, Ctrl+Space reopens
    palette.rs   the `:` entry with Tab completion
    help.rs      `?` overlay
    settings.rs  Ctrl+, preferences incl. keybinding capture
data/            .desktop, icons;  scripts/seed.js
```

## Rules

- **Threading:** all driver work runs on tokio (`rt::spawn` / `rt::io`). Widgets are
  touched only on the GTK thread, in the glib-local continuation. Clone the `Client`
  and owned args into the task; never hold a `RefCell` borrow across an `.await`.
- **RefCell re-entrancy:** appending to a `ListStore` re-binds rows synchronously and
  bind closures read pane state. Fill state first, drop the borrow, then touch the model
  (this crashed the first run in `Sidebar::set_databases`).
- **Keys:** every key is an entry in `keybinds::ACTIONS` with a scope-prefixed id
  (`docs.next-page`). Bare keys are normal-mode keys and only fire when no text widget
  has focus; chords are `text_safe` only if they may fire inside entries. Terminals
  receive everything except `global.leave-terminal`. Dispatch is one `match` in
  `App::run_action`.
- **Parsing user input:** always go through `mongo::ejson::parse_*` (accepts mongosh
  syntax) and surface `ParseError` with its line/col; never `serde_json` directly.
- **Writes** go through `App::write_guard()` so read-only mode holds everywhere. Drops and
  "delete everything" use `ui::confirm_typed` (the user types the name).
- **Secrets:** `connections.json` never holds a password. The user password and the TLS
  key password are stored via `secrets::set_all` (keyring, `secrets.json` fallback) and
  re-inserted into the URI only in `mongo::connect`. `profile::redact_uri` for display.
- **Every server op** takes an `OpCtx` (maxTimeMS cap + `viti:<uuid>` comment) so it can
  be cancelled with Escape (`kill_by_comment`).
- **Errors shown to the user are also logged** at the boundary naming the resource
  (`toast_error("query on db.coll", &e)`).
- **Accent/theme:** CSS uses `var(--accent-color)` etc.; user `style.css` beats everything.
- Commits: Conventional Commits, terse, no AI/co-author trailers.

## Roadmap

See `~/.claude/plans/viti-is-going-to-shimmying-hummingbird.md`. Done: phase 1 (MVP:
connect by URI, sidebar, documents list/JSON/table, query bar + history, CRUD, nvim edit
in VTE, vi keys, `:` palette, settings) and phase 2 (advanced connection form incl. SSH
tunnel + TLS, profile import/export, create/drop/rename databases, collections and
views, Indexes tab, My Queries, bulk update/delete) and phase 3 (Aggregations page with
stage cards / text mode / previews / focus mode / saved pipelines / create view /
external editor, export to language, Explain page) and phase 4 (import JSON/CSV, export
JSON/CSV, Schema page, Validation page) and phase 5 (Performance page, embedded mongosh, AI
query/pipeline/explain/index generation via `ai.rs`). Next: polish (6).

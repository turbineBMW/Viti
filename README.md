# Viti

MongoDB Compass, the GTK way, with vi keys.

A native GTK 4 / libadwaita MongoDB client in Rust. It aims at everything Compass
does (documents, query bar, aggregation builder, schema, explain, indexes, validation,
import/export, bulk update/delete, performance, embedded shell) with vi-mongo's
keyboard model, a `:` command line, documents edited in **your** editor (nvim by
default) inside an embedded terminal, and Compass's AI features replaced by shelling
out to `claude -p` or `codex`.

Status: phases 1–4 — connections (advanced form with auth mechanisms, TLS files, SSH
tunnel, read preference…; import/export incl. Compass exports), sidebar, documents
(list / JSON / table), query bar with history and favourites (My Queries),
insert/edit/duplicate/delete, external editing, bulk update with preview and bulk delete,
create/drop/rename databases, collections and views, an Indexes tab, an Aggregations
page (stage cards with live previews, text mode, focus mode, saved pipelines, create
view, edit the pipeline in your editor), an Explain Plan page (query or pipeline; stat
tiles, plan tree, raw JSON), export of queries and pipelines to eight languages,
import of JSON / CSV files (typed columns, nested dotted headers, error report) and
export of the collection, the query or the aggregation results to Extended JSON (three
flavours, array or one per line) or CSV (chosen fields, formula escaping), a Schema page
(random sample, type shares, value histograms, click a bar to filter) and a Validation
page (rules editor, action / level, passing and failing samples, generate a `$jsonSchema`
from the sample), vi keys, `:` palette, settings. See `CLAUDE.md` for the roadmap.

## Build

```
cargo run                       # needs gtk4 ≥ 4.18, libadwaita ≥ 1.7, gtksourceview-5, vte-2.91-gtk4
sh install.sh                   # user-local install
viti mongodb://localhost:27017  # connect straight away
```

## Keys

Keys act on the pane with the accent outline. They never fire while you are typing
in an entry, editor or terminal; Escape returns to the pane.

| Key | Action |
|---|---|
| `j` `k` `h` `l` `g` `G` | move / collapse / expand / top / bottom |
| `Tab` `Shift+Tab` `Ctrl+L` `Ctrl+H` | cycle panes |
| `/` | filter (sidebar) or query bar (documents) |
| `:` | command line (`:db`, `:coll`, `:find {…}`, `:view json`, `:set readonly on`, `:42`, `:mkcoll`, `:drop`, `:rename`, `:update {…}`, `:delete`, `:queries`, `:agg`, `:schema`, `:validation`, `:explain`, `:export [json|csv|language]`, `:import [path]`, `:conn new`, `:q`) |
| `?` | all keybindings |
| `Ctrl+O` | connections |
| `v` | list → JSON → table |
| `o` / `Enter`, `O` | open document, full page |
| `e`, `E` | edit inline, edit in external editor |
| `A`, `D`, `Ctrl+D` | add, duplicate, delete |
| `V`, `c`, `C` | multi-select, copy value, copy document |
| `]` `[` `n` `b` | next/prev document, next/prev page |
| `s` `S` `H` `r` | sort, sort by column, hide column, reset columns |
| `u`, `Ctrl+Shift+D` | bulk update / bulk delete everything the filter matches |
| `X`, `I` | export the collection / query (or the aggregation results) to JSON / CSV, import JSON / CSV |
| `Alt+O`, `Ctrl+Y`, `Ctrl+S`, `Ctrl+Shift+Y` | query options, history, save favourite, My Queries |
| sidebar: `A`, `Ctrl+D`, `R`, `i`, `Shift+Enter` | new collection (or database), drop, rename, indexes, open in new tab |
| indexes: `A`, `Ctrl+D`, `H`, `o` | create, drop, hide/unhide, details |
| aggregations: `a` `e` `Ctrl+D` `J` `K` `t` | add / edit / remove / move / enable-disable the current stage |
| aggregations: `R` `Esc` `m` `f` `T` | run, cancel, text mode, focus mode, auto-preview on/off |
| aggregations: `Ctrl+J` `o` `n` `b` | vi keys on the results, open result, next/prev results page |
| aggregations: `Ctrl+E` `Ctrl+S` `Ctrl+Y` `V` `C` | pipeline in your editor, save, open saved, create view, clear |
| `P`, `Ctrl+Shift+X` | explain the query / pipeline, export it to language (documents and aggregations) |
| explain: `R` `v` `o` `h` `l` `s` `V` `C` | run, tree/raw, stage details, collapse/expand, query/pipeline source, verbosity, copy |
| schema: `R` `h` `l` `Enter` `o` `C` | analyse a sample, pick a bar, filter the documents by it, field details, copy a generated `$jsonSchema` |
| validation: `e` `Ctrl+E` `G` `r` `Ctrl+S` | edit the rules, in your editor, generate from the schema, reload, save (`collMod`) |
| `Ctrl+`` ` | mongosh |
| `Ctrl+,` | settings (including rebinding every key) |

Bindings live in `~/.config/viti/keybindings.json` and reload on save.

## Config

`~/.config/viti/`: `config.json`, `connections.json` (no passwords — those go to the
keyring, or `secrets.json` with mode 0600), `queries.json`, `pipelines.json`, `keybindings.json`, and
`style.css` (user CSS, applied live).

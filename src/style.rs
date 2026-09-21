//! `~/.config/viti/style.css`: user CSS loaded at USER priority (above the
//! Adwaita theme and the built-in rules), seeded from a commented template and
//! hot-reloaded by `App::watch_user_css`.
use std::path::PathBuf;

pub fn path() -> PathBuf {
    crate::config::config_dir().join("style.css")
}

pub const TEMPLATE: &str = r#"/* viti-appearance-schema: 1
 *
 * Viti user stylesheet. Loaded at GTK's USER priority, so anything here beats
 * both the Adwaita theme and Viti's built-in rules. Saved changes apply live.
 *
 * Handy hooks:
 *   :root { --accent-bg-color: #3584e4; --accent-color: #3584e4; }
 *   .viti-focused        the pane that receives vi keys (accent outline)
 *   .viti-doc-card       one document in the List view
 *   .viti-doc-json       one document in the JSON view
 *   .viti-table          the Table view
 *   .viti-marked         a row selected with V
 *   .viti-terminal       the embedded editor / mongosh terminal
 *   .viti-type           the small BSON type badge next to a value
 *   .viti-cmdline        the : command line
 *   .viti-stage          one aggregation stage card (.viti-stage-current)
 *   .viti-tile           one explain-plan stat tile
 *   .viti-schema-row     one field row on the Schema page
 */
"#;

/// The current stylesheet, seeding the template when the file is missing.
pub fn load() -> String {
    match std::fs::read_to_string(path()) {
        Ok(s) => s,
        Err(_) => {
            if let Err(e) = crate::config::write_atomic(&path(), TEMPLATE.as_bytes()) {
                tracing::warn!("cannot seed {}: {e}", path().display());
            }
            TEMPLATE.to_string()
        }
    }
}

/// Built-in rules, at APPLICATION priority. Everything is expressed against
/// Adwaita tokens so it tracks light/dark and the accent for free.
pub const BUILTIN: &str = r#"
/* The collection tabs live inside the content header (ui::window). Adwaita
 * sizes a tab bar as a standalone toolbar — 6px above and below the tabs, its
 * own headerbar background and a bottom shade line — which made that header
 * ~13px taller than the sidebar's and pushed the whole content pane down.
 * Strip all of it so the strip is exactly a header's 34px content height. */
headerbar tabbar .box { background: none; box-shadow: none; padding: 0; }
headerbar tabbar tabbox { padding-top: 0; padding-bottom: 0; }
headerbar tabbar .start-action, headerbar tabbar .end-action { padding-top: 0; padding-bottom: 0; }
/* Vmux's tab look: the selected tab carries the accent instead of Adwaita's
 * gray, and keeps it when it is the only tab — stock Adwaita flattens a lone
 * tab into a plain label (tabbox.single-tab), which left the one open
 * collection looking like a window title rather than a tab. */
tabbar tab:selected, tabbar tabbox.single-tab tab:selected {
    background-color: color-mix(in srgb, var(--accent-bg-color) 25%, transparent);
    color: var(--accent-color);
}
tabbar tab:selected:hover, tabbar tabbox.single-tab tab:selected:hover {
    background-color: color-mix(in srgb, var(--accent-bg-color) 30%, transparent);
}
tabbar tab:selected:active, tabbar tabbox.single-tab tab:selected:active {
    background-color: color-mix(in srgb, var(--accent-bg-color) 38%, transparent);
}
/* Sits next to the tabs, so it is shaped and tinted like one (Adwaita has no
 * flat style for dropdowns, and its raised pill both outgrew the header and
 * shouted louder than the tabs). */
.viti-page-picker > button { background: none; box-shadow: none; min-height: 26px; padding: 4px 8px; border-radius: 9px; }
.viti-page-picker > button:hover { background-color: color-mix(in srgb, currentColor 7%, transparent); }
.viti-page-picker > button:active, .viti-page-picker > button:checked { background-color: color-mix(in srgb, currentColor 16%, transparent); }
.viti-focused { outline: 2px solid var(--accent-color); outline-offset: -2px; border-radius: 6px; }
/* The documents views: no pane outline and no row backdrop; the current
 * document's own border carries the accent instead. */
.viti-docs.viti-focused { outline: none; }
/* Sidebar: one section per connection. The pane itself never outlines; the
 * active section carries the accent while the pane is focused, and nothing
 * does while no connection is open. */
.viti-sidebar { padding: 0 8px 8px 8px; }
.viti-sidebar .viti-conn { border-radius: 8px; padding: 2px; }
.viti-sidebar .viti-conn > .header { padding: 6px 8px; border-radius: 6px; }
.viti-sidebar .viti-conn > .header:hover { background: alpha(currentColor, 0.05); }
.viti-sidebar .viti-conn > .header.selected { background: alpha(currentColor, 0.1); }
.viti-sidebar .viti-conn.active > .header label.heading { color: var(--accent-color); }
.viti-sidebar .viti-focused { outline: none; }
.viti-sidebar .viti-focused .viti-conn.active { outline: 2px solid var(--accent-color); outline-offset: -2px; }
.viti-sidebar.idle .viti-focused .viti-conn { outline: none; }
.viti-docs listview > row, .viti-docs listview > row:selected, .viti-docs listview > row:hover { background: none; box-shadow: none; outline: none; }
.viti-doc-card { background: var(--card-bg-color); color: var(--card-fg-color); border-radius: 8px; padding: 6px 10px; margin: 3px 6px; border: 1px solid alpha(currentColor, 0.08); }
.viti-doc-card.viti-marked, .viti-doc-json.viti-marked, row.viti-marked { box-shadow: inset 4px 0 0 var(--accent-bg-color); }
.viti-doc-json { margin: 3px 6px; border-radius: 8px; border: 1px solid alpha(currentColor, 0.08); }
.viti-docs listview > row:selected .viti-doc-card, .viti-docs listview > row:selected .viti-doc-json { border-color: alpha(var(--accent-color), 0.5); }
.viti-docs.viti-focused listview > row:selected .viti-doc-card, .viti-docs.viti-focused listview > row:selected .viti-doc-json { border-color: var(--accent-color); }
.viti-doc-more { font-size: 0.85em; color: alpha(currentColor, 0.55); font-style: italic; }
.viti-doc-json textview, .viti-doc-json text { font-family: monospace; font-size: 0.92em; }
.viti-key { font-family: monospace; font-weight: 600; }
.viti-value { font-family: monospace; }
.viti-value.string { color: var(--green-4); }
.viti-value.number { color: var(--blue-4); }
.viti-value.boolean { color: var(--orange-4); }
.viti-value.objectid, .viti-value.date { color: var(--purple-4); }
.viti-value.null { color: alpha(currentColor, 0.5); font-style: italic; }
.viti-type { font-size: 0.75em; padding: 1px 5px; border-radius: 4px; background: alpha(currentColor, 0.08); color: alpha(currentColor, 0.6); }
.viti-cmdline { font-family: monospace; }
.viti-cmdline-hint { font-family: monospace; color: alpha(currentColor, 0.6); }
.viti-table cell { padding: 2px 8px; }
.viti-terminal { padding: 4px; }
.viti-mono { font-family: monospace; }
.viti-complete > contents { padding: 0; }
.viti-complete-list { background: transparent; }
.viti-complete-list > row { padding: 4px 10px; min-width: 260px; }
.viti-dim { color: alpha(currentColor, 0.6); }
.viti-count { font-size: 0.9em; color: alpha(currentColor, 0.6); }
/* Aggregations: stage cards and their previews. */
.viti-stage { background: var(--card-bg-color); color: var(--card-fg-color); border-radius: 8px; border: 1px solid alpha(currentColor, 0.12); }
.viti-stage.viti-stage-current { border-color: var(--accent-color); }
.viti-aggregation.viti-focused { outline: none; }
.viti-aggregation.viti-focused .viti-stage.viti-stage-current { outline: 1px solid var(--accent-color); outline-offset: -2px; }
.viti-stage.viti-stage-disabled { opacity: 0.55; }
.viti-stage-header { padding: 4px 6px; border-bottom: 1px solid alpha(currentColor, 0.08); }
.viti-stage textview, .viti-stage text { font-family: monospace; font-size: 0.92em; }
.viti-stage-preview { border-left: 1px solid alpha(currentColor, 0.08); }
.viti-preview-doc { font-family: monospace; font-size: 0.85em; padding: 2px 6px; border-radius: 4px; }
.viti-preview-doc:hover { background: alpha(currentColor, 0.06); }
.viti-agg-results { border-top: 1px solid alpha(currentColor, 0.12); }
/* Explain: stat tiles and the plan tree. */
.viti-tile { background: var(--card-bg-color); color: var(--card-fg-color); border-radius: 8px; padding: 6px 12px; border: 1px solid alpha(currentColor, 0.08); }
.viti-tile .value { font-size: 1.25em; font-weight: 700; }
.viti-perf-card { background: var(--card-bg-color); color: var(--card-fg-color); border-radius: 8px; padding: 8px 12px; border: 1px solid alpha(currentColor, 0.08); }
.viti-perf-hot > row { padding: 2px 0; }
.viti-perf-bar trough { min-height: 4px; }
.viti-perf-bar block { min-height: 4px; }
.viti-ai-card { background: var(--card-bg-color); color: var(--card-fg-color); border-radius: 8px; padding: 8px 12px; border: 1px solid alpha(var(--accent-color), 0.4); }
.viti-collscan { color: var(--error-color); }
.viti-explain.viti-focused { outline: none; }
.viti-explain.viti-focused listview > row:selected { outline: 1px solid var(--accent-color); outline-offset: -2px; border-radius: 6px; }
.viti-schema.viti-focused { outline: none; }
.viti-schema.viti-focused listview > row:selected { outline: 1px solid var(--accent-color); outline-offset: -2px; border-radius: 6px; }
.viti-schema-list > row { border-bottom: 1px solid alpha(currentColor, 0.08); }
.viti-schema-name { font-size: 1.02em; }
.viti-validation-editor textview, .viti-validation-editor text { font-family: monospace; }
.viti-pulse { animation: viti-pulse 1s ease-in-out infinite alternate; }
@keyframes viti-pulse { from { box-shadow: 0 0 0 0 alpha(var(--accent-bg-color), 0.6); } to { box-shadow: 0 0 0 4px alpha(var(--accent-bg-color), 0.0); } }
"#;

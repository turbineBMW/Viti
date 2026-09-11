//! The action registry: one static table is the source of truth for ids,
//! titles, default accelerators and the pane each key belongs to. Overrides
//! live in `settings.keybindings` / `keybindings.json`.
use crate::config::Settings;
use crate::focus::Scope;
use gtk4 as gtk;
use gtk4::glib;
use std::collections::HashMap;

#[derive(Clone, Copy, Debug)]
pub struct Action {
    pub id: &'static str,
    pub title: &'static str,
    /// GTK accelerator syntax; "" = unbound. Bare keys are normal-mode keys and
    /// only fire when no text widget has focus.
    pub accel: &'static str,
    pub scope: Scope,
    /// Chords that may fire even while a text entry has focus.
    pub text_safe: bool,
}

const fn a(id: &'static str, title: &'static str, accel: &'static str, scope: Scope) -> Action {
    Action {
        id,
        title,
        accel,
        scope,
        text_safe: false,
    }
}
const fn g(id: &'static str, title: &'static str, accel: &'static str) -> Action {
    Action {
        id,
        title,
        accel,
        scope: Scope::Global,
        text_safe: true,
    }
}

pub const ACTIONS: &[Action] = &[
    // Global chords (fire everywhere except inside a terminal)
    g("global.palette", "Command line", "colon"),
    g("global.filter", "Filter / query bar", "slash"),
    g("global.help", "Show keybindings", "question"),
    g("global.connections", "Connections", "<Control>o"),
    g("global.focus-next", "Focus next pane", "<Control>l"),
    g("global.focus-prev", "Focus previous pane", "<Control>h"),
    g("global.toggle-sidebar", "Hide / show sidebar", "<Control>n"),
    g("global.query-options", "Toggle query options", "<Alt>o"),
    g("global.new-tab", "New collection tab", "<Control>t"),
    g("global.close-tab", "Close tab", "<Control>w"),
    g("global.next-tab", "Next tab", "<Control>Page_Down"),
    g("global.prev-tab", "Previous tab", "<Control>Page_Up"),
    g("global.shell", "Toggle mongosh", "<Control>grave"),
    g("global.settings", "Settings", "<Control>comma"),
    g("global.refresh", "Refresh", "<Control>r"),
    g("global.my-queries", "My Queries", "<Control><Shift>y"),
    g("global.quit", "Quit", "<Control>q"),
    g(
        "global.leave-terminal",
        "Leave terminal pane",
        "<Control><Shift>Escape",
    ),
    // Normal-mode keys: everywhere
    a("global.down", "Down", "j", Scope::Global),
    a("global.up", "Up", "k", Scope::Global),
    a("global.left", "Left / collapse", "h", Scope::Global),
    a("global.right", "Right / expand", "l", Scope::Global),
    a("global.top", "Go to top", "g", Scope::Global),
    a("global.bottom", "Go to bottom", "<Shift>g", Scope::Global),
    a(
        "global.escape",
        "Cancel / clear selection",
        "Escape",
        Scope::Global,
    ),
    a("global.quit-key", "Quit", "q", Scope::Global),
    // Sidebar
    a(
        "sidebar.filter",
        "Filter databases",
        "slash",
        Scope::Sidebar,
    ),
    a(
        "sidebar.open",
        "Open collection / expand",
        "Return",
        Scope::Sidebar,
    ),
    a(
        "sidebar.expand-all",
        "Expand all",
        "<Shift>e",
        Scope::Sidebar,
    ),
    a(
        "sidebar.collapse-all",
        "Collapse all",
        "<Shift>w",
        Scope::Sidebar,
    ),
    a("sidebar.refresh", "Refresh", "r", Scope::Sidebar),
    a(
        "sidebar.open-new-tab",
        "Open collection in a new tab",
        "<Shift>Return",
        Scope::Sidebar,
    ),
    a(
        "sidebar.add-collection",
        "New collection (on a connection: new database)",
        "<Shift>a",
        Scope::Sidebar,
    ),
    a(
        "sidebar.delete",
        "Drop collection / database",
        "<Control>d",
        Scope::Sidebar,
    ),
    a(
        "sidebar.rename",
        "Rename collection",
        "<Shift>r",
        Scope::Sidebar,
    ),
    a(
        "sidebar.indexes",
        "Open collection's indexes",
        "i",
        Scope::Sidebar,
    ),
    // Documents
    a(
        "docs.cycle-view",
        "Change view (list / JSON / table)",
        "v",
        Scope::Documents,
    ),
    a("docs.peek", "Open document", "o", Scope::Documents),
    a(
        "docs.peek-enter",
        "Open document",
        "Return",
        Scope::Documents,
    ),
    a(
        "docs.peek-full",
        "Open document full page",
        "<Shift>o",
        Scope::Documents,
    ),
    a("docs.add", "Add document", "<Shift>a", Scope::Documents),
    a("docs.edit", "Edit document", "e", Scope::Documents),
    a(
        "docs.edit-external",
        "Edit in external editor",
        "<Shift>e",
        Scope::Documents,
    ),
    a(
        "docs.duplicate",
        "Duplicate document",
        "<Shift>d",
        Scope::Documents,
    ),
    a(
        "docs.duplicate-now",
        "Duplicate without confirm",
        "<Alt><Shift>d",
        Scope::Documents,
    ),
    a(
        "docs.delete",
        "Delete document(s)",
        "<Control>d",
        Scope::Documents,
    ),
    a(
        "docs.delete-now",
        "Delete without confirm",
        "<Alt>d",
        Scope::Documents,
    ),
    a(
        "docs.select",
        "Toggle multi-select",
        "<Shift>v",
        Scope::Documents,
    ),
    a("docs.copy", "Copy highlighted value", "c", Scope::Documents),
    a(
        "docs.copy-doc",
        "Copy document",
        "<Shift>c",
        Scope::Documents,
    ),
    a(
        "docs.export",
        "Export to JSON / CSV",
        "<Shift>x",
        Scope::Documents,
    ),
    a(
        "docs.import",
        "Import JSON / CSV",
        "<Shift>i",
        Scope::Documents,
    ),
    a(
        "docs.export-language",
        "Export query to language",
        "<Control><Shift>x",
        Scope::Documents,
    ),
    a("docs.explain", "Explain plan", "<Shift>p", Scope::Documents),
    a(
        "docs.bulk-update",
        "Update all matching documents",
        "u",
        Scope::Documents,
    ),
    a(
        "docs.bulk-delete",
        "Delete all matching documents",
        "<Control><Shift>d",
        Scope::Documents,
    ),
    a("docs.refresh", "Refresh", "<Control>r", Scope::Documents),
    a("docs.query", "Toggle query bar", "slash", Scope::Documents),
    a("docs.sort", "Sort", "s", Scope::Documents),
    a(
        "docs.sort-column",
        "Sort by column",
        "<Shift>s",
        Scope::Documents,
    ),
    a(
        "docs.hide-column",
        "Hide column",
        "<Shift>h",
        Scope::Documents,
    ),
    a(
        "docs.reset-columns",
        "Reset hidden columns",
        "r",
        Scope::Documents,
    ),
    a(
        "docs.next-doc",
        "Next document",
        "bracketright",
        Scope::Documents,
    ),
    a(
        "docs.prev-doc",
        "Previous document",
        "bracketleft",
        Scope::Documents,
    ),
    a("docs.next-page", "Next page", "n", Scope::Documents),
    a("docs.prev-page", "Previous page", "b", Scope::Documents),
    a(
        "docs.expand",
        "Expand / collapse fields",
        "x",
        Scope::Documents,
    ),
    a(
        "docs.expand-all",
        "Expand all documents",
        "<Shift>x",
        Scope::Documents,
    ),
    // Query bar
    a(
        "query.history",
        "Query history",
        "<Control>y",
        Scope::QueryBar,
    ),
    a("query.clear", "Clear input", "<Control>u", Scope::QueryBar),
    a(
        "query.favourite",
        "Save as favourite",
        "<Control>s",
        Scope::QueryBar,
    ),
    // Aggregation
    a("agg.add-stage", "Add stage", "a", Scope::Aggregation),
    a("agg.edit-stage", "Edit stage", "e", Scope::Aggregation),
    a(
        "agg.edit-external",
        "Edit pipeline in external editor",
        "<Control>e",
        Scope::Aggregation,
    ),
    a(
        "agg.delete-stage",
        "Delete stage",
        "<Control>d",
        Scope::Aggregation,
    ),
    a("agg.run", "Run pipeline", "<Shift>r", Scope::Aggregation),
    a(
        "agg.clear",
        "Clear all stages",
        "<Shift>c",
        Scope::Aggregation,
    ),
    a(
        "agg.move-down",
        "Move stage down",
        "<Shift>j",
        Scope::Aggregation,
    ),
    a(
        "agg.move-up",
        "Move stage up",
        "<Shift>k",
        Scope::Aggregation,
    ),
    a(
        "agg.toggle-stage",
        "Enable / disable stage",
        "t",
        Scope::Aggregation,
    ),
    a(
        "agg.focus-results",
        "Focus results / stages",
        "<Control>j",
        Scope::Aggregation,
    ),
    a(
        "agg.focus-mode",
        "Focus mode (one stage)",
        "f",
        Scope::Aggregation,
    ),
    a(
        "agg.text-mode",
        "Stage cards / text mode",
        "m",
        Scope::Aggregation,
    ),
    a("agg.peek", "Open result document", "o", Scope::Aggregation),
    a(
        "agg.peek-enter",
        "Open result document",
        "Return",
        Scope::Aggregation,
    ),
    a(
        "agg.next-page",
        "Next results page",
        "n",
        Scope::Aggregation,
    ),
    a(
        "agg.prev-page",
        "Previous results page",
        "b",
        Scope::Aggregation,
    ),
    a(
        "agg.save",
        "Save pipeline",
        "<Control>s",
        Scope::Aggregation,
    ),
    a(
        "agg.open",
        "Open a saved pipeline",
        "<Control>y",
        Scope::Aggregation,
    ),
    a(
        "agg.create-view",
        "Create a view from the pipeline",
        "<Shift>v",
        Scope::Aggregation,
    ),
    a(
        "agg.export-language",
        "Export pipeline to language",
        "<Control><Shift>x",
        Scope::Aggregation,
    ),
    a(
        "agg.export",
        "Export results to JSON / CSV",
        "<Shift>x",
        Scope::Aggregation,
    ),
    a(
        "agg.explain",
        "Explain pipeline",
        "<Shift>p",
        Scope::Aggregation,
    ),
    a(
        "agg.preview-toggle",
        "Auto-preview stages on / off",
        "<Shift>t",
        Scope::Aggregation,
    ),
    // Explain
    a("explain.run", "Run explain", "<Shift>r", Scope::Explain),
    a(
        "explain.toggle-view",
        "Plan tree / raw JSON",
        "v",
        Scope::Explain,
    ),
    a("explain.peek", "Stage details", "o", Scope::Explain),
    a(
        "explain.peek-enter",
        "Stage details",
        "Return",
        Scope::Explain,
    ),
    a(
        "explain.copy",
        "Copy raw explain",
        "<Shift>c",
        Scope::Explain,
    ),
    a(
        "explain.source",
        "Explain the query / the pipeline",
        "s",
        Scope::Explain,
    ),
    a(
        "explain.verbosity",
        "Cycle verbosity",
        "<Shift>v",
        Scope::Explain,
    ),
    // Schema
    a(
        "schema.analyze",
        "Sample and analyse",
        "<Shift>r",
        Scope::Schema,
    ),
    a(
        "schema.filter",
        "Filter documents by the highlighted bar",
        "Return",
        Scope::Schema,
    ),
    a("schema.peek", "Field details", "o", Scope::Schema),
    a(
        "schema.copy-json-schema",
        "Copy a generated $jsonSchema",
        "<Shift>c",
        Scope::Schema,
    ),
    // Validation
    a("validation.edit", "Edit the rules", "e", Scope::Validation),
    a(
        "validation.edit-external",
        "Edit rules in external editor",
        "<Control>e",
        Scope::Validation,
    ),
    a(
        "validation.generate",
        "Generate rules from schema",
        "<Shift>g",
        Scope::Validation,
    ),
    a(
        "validation.refresh",
        "Reload rules and samples",
        "r",
        Scope::Validation,
    ),
    a(
        "validation.save",
        "Save rules (collMod)",
        "<Control>s",
        Scope::Validation,
    ),
    // Indexes
    a("idx.add", "Create index", "<Shift>a", Scope::Indexes),
    a("idx.drop", "Drop index", "<Control>d", Scope::Indexes),
    a(
        "idx.hide",
        "Hide / unhide index",
        "<Shift>h",
        Scope::Indexes,
    ),
    a("idx.peek", "Index details", "o", Scope::Indexes),
    a("idx.peek-enter", "Index details", "Return", Scope::Indexes),
    a("idx.refresh", "Refresh indexes", "r", Scope::Indexes),
];

pub fn find(id: &str) -> Option<&'static Action> {
    ACTIONS.iter().find(|a| a.id == id)
}

pub fn accel_for(settings: &Settings, id: &str) -> String {
    if let Some(over) = settings.keybindings.get(id) {
        return over.clone();
    }
    find(id).map(|a| a.accel.to_string()).unwrap_or_default()
}

/// Every action with its effective accelerator.
pub fn merged(settings: &Settings) -> Vec<(&'static Action, String)> {
    ACTIONS
        .iter()
        .map(|a| (a, accel_for(settings, a.id)))
        .collect()
}

/// Normalise an accelerator string the way `dispatch` builds lookup keys:
/// lower-case keyval, modifier mask only. Returns None for unparsable input.
pub fn normalise(accel: &str) -> Option<String> {
    let (key, mods) = gtk::accelerator_parse(accel)?;
    let mods = mods & gtk::accelerator_get_default_mod_mask();
    Some(gtk::accelerator_name(key.to_lower(), mods).to_string())
}

/// Whether an accelerator is a bare key (no Ctrl/Alt/Super): a normal-mode key.
pub fn is_bare(accel: &str) -> bool {
    match gtk::accelerator_parse(accel) {
        Some((_, mods)) => {
            let m = mods & gtk::accelerator_get_default_mod_mask();
            (m & (gtk::gdk::ModifierType::CONTROL_MASK
                | gtk::gdk::ModifierType::ALT_MASK
                | gtk::gdk::ModifierType::SUPER_MASK
                | gtk::gdk::ModifierType::META_MASK))
                .is_empty()
        }
        None => false,
    }
}

/// (scope, normalised accel) -> action id, for the normal-mode dispatcher.
pub type Keymap = HashMap<(Scope, String), &'static str>;

pub fn build_keymap(settings: &Settings) -> Keymap {
    let mut map = HashMap::new();
    for (action, accel) in merged(settings) {
        if accel.is_empty() {
            continue;
        }
        if let Some(n) = normalise(&accel) {
            map.insert((action.scope, n), action.id);
        }
    }
    map
}

pub fn pretty_accel(accel: &str) -> String {
    if accel.is_empty() {
        return "unbound".into();
    }
    match gtk::accelerator_parse(accel) {
        Some((key, mods)) => gtk::accelerator_get_label(key, mods).to_string(),
        None => accel.to_string(),
    }
}

pub fn add_sc(
    ctl: &gtk::ShortcutController,
    trig: &str,
    f: impl Fn() -> glib::Propagation + 'static,
) {
    let action = gtk::CallbackAction::new(move |_, _| f());
    if let Some(trigger) = gtk::ShortcutTrigger::parse_string(trig) {
        ctl.add_shortcut(gtk::Shortcut::new(Some(trigger), Some(action)));
    } else {
        tracing::warn!("bad shortcut trigger: {trig}");
    }
}

/// Apply a rebind, stealing the accel from any other action *in the same scope*.
pub fn apply_binding(settings: &mut Settings, action_id: &str, accel: &str) {
    let scope = find(action_id).map(|a| a.scope).unwrap_or(Scope::Global);
    if !accel.is_empty() {
        let norm = normalise(accel);
        let owners: Vec<&str> = ACTIONS
            .iter()
            .filter(|a| a.scope == scope && a.id != action_id)
            .map(|a| a.id)
            .collect();
        for id in owners {
            if normalise(&accel_for(settings, id)) == norm && norm.is_some() {
                settings.keybindings.insert(id.to_string(), String::new());
            }
        }
    }
    settings
        .keybindings
        .insert(action_id.to_string(), accel.to_string());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_and_prefixed_by_scope() {
        let mut seen = std::collections::HashSet::new();
        for a in ACTIONS {
            assert!(seen.insert(a.id), "duplicate id {}", a.id);
            let prefix = a.id.split('.').next().unwrap();
            assert_eq!(Scope::from_id_prefix(prefix), a.scope, "{}", a.id);
        }
    }

    #[test]
    fn overrides_win() {
        let mut s = Settings::default();
        assert_eq!(accel_for(&s, "docs.next-page"), "n");
        s.keybindings
            .insert("docs.next-page".into(), "<Shift>n".into());
        assert_eq!(accel_for(&s, "docs.next-page"), "<Shift>n");
        s.keybindings.insert("docs.next-page".into(), String::new());
        assert_eq!(accel_for(&s, "docs.next-page"), "");
    }
}

//! A modal dialog (80% of the window) with two terminals: the external editor
//! (nvim on a temp file) and mongosh. The editor round-trip: write Extended JSON to a temp
//! file, run the editor in the VTE, and on exit read it back, parse and apply
//! (`App::on_editor_exited`).
use crate::config::Settings;
use crate::mongo::ConnectionId;
use crate::mongo::ops::Namespace;
use adw::prelude::*;
use bson::{Bson, Document};
use gtk4 as gtk;
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use vte4 as vte;
use vte4::prelude::*;

#[derive(Clone, Debug)]
pub enum JobKind {
    Document {
        conn: ConnectionId,
        ns: Namespace,
        /// `None` for a new document.
        id: Option<Bson>,
        is_new: bool,
    },
    /// Several documents in one array; applied by `_id`.
    Documents { conn: ConnectionId, ns: Namespace },
    /// The aggregation page's pipeline as one array; loaded back into the cards.
    Pipeline { conn: ConnectionId, ns: Namespace },
    /// The validation page's rules document; loaded back unsaved.
    Validation { conn: ConnectionId, ns: Namespace },
    /// Free text handed back to `on_text` (query filter, pipeline…).
    Text { purpose: String },
}

#[derive(Clone, Debug)]
pub struct EditorJob {
    pub id: uuid::Uuid,
    pub path: PathBuf,
    pub kind: JobKind,
    pub original_text: String,
}

type ExitHandler = Rc<dyn Fn(EditorJob, i32)>;

pub struct EditorPane {
    pub root: gtk::Box,
    pub stack: gtk::Stack,
    pub editor: vte::Terminal,
    pub shell: vte::Terminal,
    pub job: RefCell<Option<EditorJob>>,
    pub shell_running: std::cell::Cell<bool>,
    on_exit: RefCell<Option<ExitHandler>>,
    dialog: RefCell<Option<adw::Dialog>>,
    parent: RefCell<Option<gtk::Window>>,
}

fn terminal() -> vte::Terminal {
    let t = vte::Terminal::new();
    t.set_scrollback_lines(5000);
    t.set_scroll_on_output(false);
    t.set_scroll_on_keystroke(true);
    t.set_mouse_autohide(true);
    t.set_vexpand(true);
    t.set_hexpand(true);
    t.add_css_class("viti-terminal");
    t
}

impl EditorPane {
    pub fn new() -> Rc<Self> {
        let editor = terminal();
        let shell = terminal();
        let stack = gtk::Stack::new();
        stack.add_named(&editor, Some("editor"));
        stack.add_named(&shell, Some("shell"));

        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.append(&stack);

        let pane = Rc::new(Self {
            root,
            stack,
            editor,
            shell,
            job: RefCell::new(None),
            shell_running: std::cell::Cell::new(false),
            on_exit: RefCell::new(None),
            dialog: RefCell::new(None),
            parent: RefCell::new(None),
        });
        {
            let p = pane.clone();
            pane.editor.connect_child_exited(move |_, status| {
                let job = p.job.borrow_mut().take();
                if let Some(job) = job {
                    let cb = p.on_exit.borrow().clone();
                    if let Some(cb) = cb {
                        cb(job, status);
                    }
                }
            });
        }
        {
            let p = pane.clone();
            pane.shell.connect_child_exited(move |t, _| {
                p.shell_running.set(false);
                t.feed("\r\n\x1b[2m[mongosh exited — Ctrl+` to restart]\x1b[0m\r\n".as_bytes());
            });
        }
        pane
    }

    /// The window the dialog is presented on.
    pub fn attach(&self, window: &impl IsA<gtk::Window>) {
        *self.parent.borrow_mut() = Some(window.clone().upcast());
    }

    pub fn set_on_exit(&self, f: impl Fn(EditorJob, i32) + 'static) {
        *self.on_exit.borrow_mut() = Some(Rc::new(f));
    }

    pub fn is_shown(&self) -> bool {
        self.dialog.borrow().is_some()
    }

    pub fn hide(&self) {
        let dialog = self.dialog.borrow_mut().take();
        if let Some(d) = dialog {
            d.set_can_close(true);
            d.close();
        }
    }

    /// Present (or retitle) the dialog: 80% of the window, centred. While an
    /// editor job runs the dialog cannot be dismissed — the temp file would be
    /// orphaned — so the user leaves by quitting the editor.
    fn show(&self, page: &str, title: &str) {
        self.stack.set_visible_child_name(page);
        if let Some(d) = self.dialog.borrow().as_ref() {
            d.set_title(title);
            d.set_can_close(page != "editor");
            return;
        }
        let parent = self.parent.borrow().clone();
        let (w, h) = parent
            .as_ref()
            .map(|p| (p.width(), p.height()))
            .unwrap_or((1200, 800));
        let dialog = adw::Dialog::builder()
            .title(title)
            .content_width((w as f32 * 0.8) as i32)
            .content_height((h as f32 * 0.8) as i32)
            .can_close(page != "editor")
            .build();
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&adw::HeaderBar::new());
        toolbar.set_content(Some(&self.root));
        dialog.set_child(Some(&toolbar));
        {
            let root = self.root.clone();
            let toolbar = toolbar.clone();
            dialog.connect_closed(move |_| {
                // Keep the terminals alive for the next open.
                toolbar.set_content(None::<&gtk::Widget>);
                let _ = &root;
            });
        }
        dialog.present(parent.as_ref());
        *self.dialog.borrow_mut() = Some(dialog);
    }

    /// Editor busy: refuse to start a second job (its temp file would be lost).
    pub fn busy(&self) -> bool {
        self.job.borrow().is_some()
    }

    pub fn open(
        &self,
        settings: &Settings,
        kind: JobKind,
        text: String,
        title: &str,
    ) -> anyhow::Result<()> {
        if self.busy() {
            anyhow::bail!("the editor is already open; finish that edit first");
        }
        let dir = gtk::glib::user_runtime_dir();
        let file = tempfile::Builder::new()
            .prefix("viti-")
            .suffix(".json")
            .tempfile_in(if dir.exists() {
                dir
            } else {
                std::env::temp_dir()
            })?;
        std::fs::write(file.path(), text.as_bytes())?;
        let (_, path) = file.keep()?;
        let mut argv = settings.editor_argv();
        argv.push(path.to_string_lossy().into_owned());
        *self.job.borrow_mut() = Some(EditorJob {
            id: uuid::Uuid::new_v4(),
            path: path.clone(),
            kind,
            original_text: text,
        });
        self.show("editor", title);
        self.editor.reset(true, true);
        spawn_argv(&self.editor, &argv, None);
        self.editor.grab_focus();
        Ok(())
    }

    /// Reopen the same file after a parse error, so nothing typed is lost.
    pub fn reopen(&self, settings: &Settings, job: EditorJob, title: &str) {
        let mut argv = settings.editor_argv();
        argv.push(job.path.to_string_lossy().into_owned());
        *self.job.borrow_mut() = Some(job);
        self.show("editor", title);
        self.editor.reset(true, true);
        spawn_argv(&self.editor, &argv, None);
        self.editor.grab_focus();
    }

    /// Start (or refocus) mongosh on the given URI; the password is entered in
    /// the terminal, never passed on the command line.
    pub fn toggle_shell(&self, settings: &Settings, uri: Option<(String, Option<String>)>) {
        if self.is_shown() && self.stack.visible_child_name().as_deref() == Some("shell") {
            self.hide();
            return;
        }
        let Some((uri, user)) = uri else {
            return;
        };
        self.show(
            "shell",
            &format!("mongosh — {}", crate::mongo::profile::redact_uri(&uri)),
        );
        if !self.shell_running.get() {
            let mut argv = shell_words::split(&settings.mongosh_command)
                .unwrap_or_else(|_| vec![settings.mongosh_command.clone()]);
            argv.push(uri);
            if let Some(u) = user {
                argv.push("--username".into());
                argv.push(u);
            }
            self.shell.reset(true, true);
            spawn_argv(&self.shell, &argv, None);
            self.shell_running.set(true);
        }
        self.shell.grab_focus();
    }

    pub fn shell_visible(&self) -> bool {
        self.is_shown() && self.stack.visible_child_name().as_deref() == Some("shell")
    }
}

/// Spawn `argv` in the terminal with a clean environment: TERM describes vte
/// itself, and TMUX/TERM_PROGRAM from the launching terminal are scrubbed.
pub fn spawn_argv(term: &vte::Terminal, argv: &[String], cwd: Option<&str>) {
    const SCRUB: &[&str] = &[
        "TERM",
        "TMUX",
        "TMUX_PANE",
        "TERM_PROGRAM",
        "TERM_PROGRAM_VERSION",
    ];
    let mut env: Vec<String> = std::env::vars()
        .filter(|(k, _)| !SCRUB.contains(&k.as_str()))
        .map(|(k, v)| format!("{k}={v}"))
        .collect();
    env.push("TERM=xterm-256color".into());
    env.push("TERM_PROGRAM=viti".into());
    env.push(concat!("TERM_PROGRAM_VERSION=", env!("CARGO_PKG_VERSION")).to_string());
    let argv_refs: Vec<&str> = argv.iter().map(|s| s.as_str()).collect();
    let env_refs: Vec<&str> = env.iter().map(|s| s.as_str()).collect();
    let tw = term.downgrade();
    term.spawn_async(
        vte::PtyFlags::DEFAULT,
        cwd,
        &argv_refs,
        &env_refs,
        gtk::glib::SpawnFlags::SEARCH_PATH,
        || {},
        -1,
        None::<&gtk::gio::Cancellable>,
        move |res| {
            if let Err(e) = res
                && let Some(t) = tw.upgrade()
            {
                t.feed(format!("\r\n\x1b[31m[viti] spawn failed: {e}\x1b[0m\r\n").as_bytes());
            }
        },
    );
}

/// The text handed to the editor for a document.
pub fn document_text(doc: &Document) -> String {
    crate::mongo::ejson::pretty(doc, crate::mongo::ejson::Mode::Canonical)
}

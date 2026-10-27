# Viti

Viti is a native GTK MongoDB client for Linux, built in Rust. It combines the
workflows of MongoDB Compass with a fast, keyboard-first interface.

![Viti connection editor](docs/viti.png)

> Viti is under active development. Review destructive operations carefully
> and keep backups of important data.

## Highlights

- Browse, filter, edit, import, and export documents.
- Build aggregation pipelines with live previews.
- Inspect schemas, indexes, validation rules, explain plans, and server
  performance.
- Work in list, JSON, and table views with pagination for large collections.
- Edit with the built-in editor or your preferred terminal editor.
- Use vi-style navigation, configurable shortcuts, and a `:` command palette.
- Connect with TLS, SSH tunnels, authentication mechanisms, and imported
  MongoDB Compass profiles.
- Optionally draft queries and pipelines through local `claude` or `codex`
  commands.
- Follow the active Omarchy desktop palette and theme switches live.

## Build and install

Viti requires Rust and these Linux libraries:

- GTK 4.18 or newer
- libadwaita 1.7 or newer
- GtkSourceView 5
- VTE for GTK 4

Run it from the source tree:

```sh
cargo run
```

Or install the binary, desktop entry, and icons for the current user:

```sh
./install.sh
```

The installer writes to `~/.local` and does not require root access.

## Usage

Start Viti and add a connection with `Ctrl+O`:

```sh
viti
```

For local development, a URI can be supplied directly:

```sh
viti mongodb://localhost:27017
```

Avoid putting passwords in command-line URIs because shell history and process
lists may expose them. Viti stores connection profiles without passwords and
uses the system keyring by default for secrets.

Useful shortcuts:

| Key | Action |
| --- | --- |
| `Ctrl+O` | Connections |
| `Tab` / `Shift+Tab` | Switch between the sidebar and the content (also `Ctrl+L` / `Ctrl+H`) |
| `↑` `↓` `←` `→` | Move in the sidebar and the lists, like `j` `k` `h` `l` |
| `/` | Filter the sidebar, or focus the query bar (from any page of a tab) |
| `:` | Command line |
| `Ctrl+/` | Command palette: every action and `:` command on screen |
| `e` / `E` | Edit inline / with the configured editor |
| `v` | Cycle document views |
| `?` | Show all shortcuts |

## Development

```sh
cargo fmt -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Tests that require a live MongoDB instance are skipped unless `VITI_TEST_URI`
is set.

## Omarchy themes

When an Omarchy theme is present, *Settings → Theme → Follow Omarchy theme*
is enabled by default. Viti reads the active theme's `colors.toml`, applies its
window colours, accent, and light/dark mode, and follows `omarchy theme set`
live. Turning the option off restores Viti's System/Light/Dark preference.

A theme can replace the generated CSS by shipping a `viti.css` with
libadwaita CSS variables, either directly or from a
`~/.config/omarchy/themed/viti.css.tpl` template. The theme's `colors.toml`
is still required: it decides light or dark mode and the accent of drawn
charts. Viti's own `~/.config/viti/style.css` remains the final override.

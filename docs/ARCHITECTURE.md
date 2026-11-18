# Architecture

**Atlas** is a **Rust** application on **GTK 4** and **libadwaita**, shipped as a **Flatpak** on the **GNOME 51** runtime. It is split in two crates so that everything that is plain data can be tested without a display or a database.

| Crate | Holds | Tested on the host |
| :--- | :--- | :--- |
| **`crates/atlas-core`** | schema model, relation inference, layered layout, springs, SQL building, the console guard, dump parser, DDL, diff, migrations, pending edits, CSV, query plans, completion, backup, users, designs, Mermaid, stored formats, design tokens | **yes**, `cargo test -p atlas-core` (85 tests) |
| **`atlas`** | the GTK application: sources (`db/`), canvas (`canvas/`), window and panels (`ui/`), export | inside the Flatpak SDK (no GTK headers on the host); `db/tests.rs` runs against SQLite always, MariaDB and PostgreSQL when given |

```mermaid
flowchart TD
    subgraph Sources ["Sources (atlas/src/db)"]
        Maria["MariaDB / MySQL<br/><b>sqlx, information_schema</b>"]
        Pg["PostgreSQL<br/><b>sqlx, pg_catalog</b>"]
        Lite["SQLite<br/><b>sqlx, mode=ro</b>"]
        Dump["SQL dump<br/><b>sqlparser-rs</b>"]
        Ssh["SSH tunnel, optional<br/><b>system ssh -L</b>"]
    end

    subgraph Core ["atlas-core (pure, unit-tested)"]
        Schema["Schema model<br/><b>tables, columns, keys, relations</b>"]
        Infer["Inferred relations<br/><b>*_id to *.id</b>"]
        Layout["Layered layout<br/><b>positions, edge routes</b>"]
        Tokens["Insights Mono tokens"]
    end

    subgraph App ["atlas (GTK 4 + libadwaita)"]
        Canvas["Canvas widget<br/><b>GSK render nodes, spring camera</b>"]
        Data["Data browser<br/><b>ColumnView, paged, edits reviewed</b>"]
        Window["Window, sidebar, search, minimap"]
    end

    Ssh --> Maria
    Ssh --> Pg
    Maria --> Schema
    Pg --> Schema
    Lite --> Schema
    Dump --> Schema
    Schema --> Infer --> Layout --> Canvas
    Tokens -->|build.rs: style.css, style-dark.css| Window
    Schema --> Data
```

---

## Principles

- **Read-only unless allowed:** a session carries an `Access`. `ReadOnly`, the default, opens it read-only on the server side (**MariaDB** `SET SESSION TRANSACTION READ ONLY` in `after_connect`, **PostgreSQL** `default_transaction_read_only = on` as a startup option, **SQLite** the read-only open flag), and the console's guard refuses everything but one reading statement. `ReadWrite` comes only from the connection's **Allow changes** switch (or **Allow Changes to This File**); `Session::execute` and `execute_all` refuse on a read-only session before sending. *Tested*: `db/tests.rs` sends an `UPDATE` to all three read-only sessions and expects an error, and runs the write path on writable ones.
- **Shown, then one transaction:** every change Atlas writes itself is SQL from `atlas-core` (`edit`, `migrate`, `diff`, `admin`, the CSV importer), shown in a review dialog, then sent by `execute_all` between `BEGIN` and `COMMIT`, rolled back on the first error. SQLite table rebuilds turn foreign keys off around the transaction and run `PRAGMA foreign_key_check` before committing. MariaDB commits DDL by itself; the dialogs say so.
- **Text all the way:** Atlas sends plain SQL text, no prepared statements. The MySQL text protocol and PostgreSQL's simple query protocol return every value as text, which is what a viewer shows; SQLite values are turned into text by their storage class. One code path reads catalogs, pages and console results alike.
- **Values never raw:** filter values reach MariaDB as `CONVERT(X'…' USING utf8mb4)`, PostgreSQL and SQLite as quoted strings with `standard_conforming_strings` on. Identifiers are quoted per dialect. *Tested* (unit tests, and a filter with a quote against all three databases).
- **The UI never waits:** database work runs on a two-thread **Tokio** runtime (`runtime.rs`); the UI awaits it inside `glib::spawn_future_local`. Late answers to an old query are dropped by a generation counter.
- **Passwords stay out of Atlas' files:** asked on every connect, unless the connection has **Remember password in the keyring** on; then `keyring.rs` keeps it in the desktop keyring through **oo7**. Connections, layouts, console history, tabs and saved queries are JSON in the app's config folder, written through a temporary file.
- **SSH through the system:** `ssh -N -L` with `BatchMode=yes` and `ExitOnForwardFailure=yes`; Atlas waits until the forwarded port accepts, or reports ssh's own error. The Flatpak gets the agent socket and `~/.ssh` read-only.
- **Cheap frames, nothing at rest:** see [RENDERING.md](RENDERING.md) and [MOTION.md](MOTION.md).

## Where Things Are

| Path | Holds |
| :--- | :--- |
| **`atlas/src/db/`** | `mysql.rs`, `postgres.rs`, `sqlite.rs` (connect, catalog, cell to text), `ssh.rs`, `tests.rs` |
| **`atlas/src/canvas/`** | `mod.rs` (widget, camera, gestures, animation), `card.rs` (measure and draw a card), `edges.rs` (anchors, curves, crow's feet), `design.rs` (Design Mode, undo) |
| **`atlas/src/ui/`** | `window.rs` (everything wired together, panel growth), `start.rs`, `connect.rs`, `sidebar.rs`, `search.rs`, `minimap.rs`, `data.rs` (rows, structure, edits), `console.rs` (tabs, plans, saved queries), `complete.rs` (completion provider), `grid.rs`, `structure.rs`, `editor.rs` (table editor), `review.rs` (SQL review, batches), `import.rs` (CSV, SQL file, backup), `compare.rs`, `admin.rs` (server, users), `export.rs` |
| **`atlas/src/keyring.rs`** | the opt-in keyring (oo7) |
| **`atlas/src/shots.rs`** | the screenshot walk-through (`ATLAS_SCREENSHOT`) |
| **`data/`** | stylesheet template, GSettings schema, desktop file, metainfo, icons, SQL highlighting and its two colour schemes |
| **`po/`** | `atlas.pot`, `de.po`; `tools/update-po.sh` refreshes both |
| **`tests/fixtures/`** | the demo shop (`demo.sql`, `demo-data.sql`) |

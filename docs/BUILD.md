# Build

**Atlas** is built inside the **GNOME 51 SDK** with the **Rust stable** SDK extension, so the host needs neither GTK headers nor root. Only `atlas-core` is built and tested directly on the host.

---

## Once

```bash
flatpak install --user flathub org.gnome.Sdk//51 org.gnome.Platform//51 org.freedesktop.Sdk.Extension.rust-stable//26.08
```

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal -c clippy,rustfmt
```

**flatpak-builder** comes from the distribution (`dnf install flatpak-builder`, `apt install flatpak-builder`, `pacman -S flatpak-builder`).

---

## Commands

| Does | Command |
| :--- | :--- |
| **Test the core on the host** | `cargo test -p atlas-core` |
| **Check everything in the SDK** | see below; output goes to **`target-sdk/`** so host and SDK builds never mix |
| **Build and install the Flatpak** | `flatpak-builder --user --install --force-clean --disable-rofiles-fuse build-dir build-aux/io.github.tomsliikee.Atlas.json` |
| **Run** | `flatpak run io.github.tomsliikee.Atlas` |
| **Screenshots** | see below |
| **Refresh translations** | `tools/update-po.sh` inside the SDK |

```bash
flatpak run --share=network --filesystem=$PWD --command=bash org.gnome.Sdk//51 -c 'export PATH=$PATH:/usr/lib/sdk/rust-stable/bin CARGO_TARGET_DIR=target-sdk; cargo clippy --all-targets -- -D warnings && cargo test'
```

## Database Tests

`cargo test` always runs the SQLite test (a temporary file from `tests/fixtures/`). MariaDB and PostgreSQL run when they are given; the demo data loads into throwaway containers:

```bash
podman run -d --rm --name atlas-mariadb -e MARIADB_ROOT_PASSWORD=root-test -e MARIADB_DATABASE=shop -e MARIADB_USER=atlas -e MARIADB_PASSWORD=atlas-test -p 127.0.0.1:33061:3306 docker.io/library/mariadb:10.11
```

```bash
podman run -d --rm --name atlas-postgres -e POSTGRES_PASSWORD=atlas-test -e POSTGRES_USER=atlas -e POSTGRES_DB=shop -p 127.0.0.1:54321:5432 docker.io/library/postgres:17-alpine
```

```bash
cat tests/fixtures/demo.sql tests/fixtures/demo-data.sql | podman exec -i atlas-mariadb mariadb -uatlas -patlas-test shop
```

For PostgreSQL the boolean `1` of `products.active` must be `true`; load `demo.sql` as it is and the products rows with that change. Then, in the SDK:

```bash
ATLAS_TEST_MARIADB=127.0.0.1:33061:atlas:shop ATLAS_TEST_PASSWORD=atlas-test ATLAS_TEST_POSTGRES=127.0.0.1:54321:atlas:shop ATLAS_TEST_PG_PASSWORD=atlas-test cargo test
```

Never point these at a real database: the test sends an `UPDATE` to prove that it is refused.

For a real database there is `probe`, which connects exactly as the app does and only reads the structure; on a failure it prints the classified problem and, when the database name was wrong, the databases the user may open:

```bash
ATLAS_PROBE=127.0.0.1:3306:user:database ATLAS_PROBE_PASSWORD=… cargo test probe -- --nocapture
```

## Screenshot Mode

With `ATLAS_SCREENSHOT=<dir>` Atlas renders its window through its own GSK renderer at 2x and walks through itself: start page; with `ATLAS_OPEN=<file>` the diagram; with `ATLAS_FOCUS=<table>` focus, unfold, the panel growing, rows, a filtered table, search (`ATLAS_SEARCH`), the console (`ATLAS_SQL`), exports (`ATLAS_EXPORT_DIR`), dark; last the connection dialog. Then it restores the stored theme and quits. `ATLAS_FAIL=host:port:user:database` instead shows the failure dialog of a MariaDB connection without password (a test user only). `ATLAS_CONFIG_DIR` keeps such runs out of the real configuration (Flatpak overrides `XDG_CONFIG_HOME`).

```bash
flatpak run --filesystem=/tmp --filesystem=$PWD/readmestuff --env=ATLAS_CONFIG_DIR=/tmp/atlas-shots-config --env=ATLAS_SCREENSHOT=$PWD/readmestuff/shots --env=ATLAS_OPEN=/tmp/shop.sqlite --env=ATLAS_FOCUS=products io.github.tomsliikee.Atlas
```

`/tmp/shop.sqlite` comes from `cat tests/fixtures/demo.sql tests/fixtures/demo-data.sql | sqlite3 /tmp/shop.sqlite`.

---

## Notes

- **Network during the build:** the manifest lets cargo fetch crates (`build-args: --share=network`). That is fine for local builds; **Flathub** needs vendored sources (`flatpak-cargo-generator` → `cargo-sources.json`). *Planned* for publishing.
- **Size:** the stripped release binary is about **11 MB** (sqlx with rustls and a bundled SQLite, sqlparser, Tokio); GTK, libadwaita and GtkSourceView come from the runtime.
- **Distributions:** the Flatpak runs where **Flatpak** and the **GNOME 51 runtime** run. *Tested* on **Fedora 44** (GNOME 50, Wayland, NVIDIA RTX 4070). **CachyOS**, **Linux Mint**, **Debian stable** and **Ubuntu** are *planned*; Debian needs `apt install flatpak` and the Flathub remote first.

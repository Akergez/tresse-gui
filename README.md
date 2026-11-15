# Tresse GUI

Folders versioned and synchronized with
[Tresse](https://gitlab.com/ragusseven/tresse): pick a folder — an existing
Tresse repository or a new one started in place — connect it to a Tresse
server or your own S3-compatible bucket, and sync.

Built with GPUI Kit and the `gpui-adaptive` design system, the same stack as
Money Manager. One window, drawn once for a pointer and a finger.

## What it does

- **Folders.** A folder with `.tresse/` in it is an existing repository; any
  other folder can be made one. The list reads each folder's own files —
  never a cached copy of them.
- **Connections.** The server's page (a connection string comes back from
  it), a connection string from another device, or every field by hand:
  server URL and token, or endpoint, region, bucket and keys. The encryption
  key is made here and never leaves the folder.
- **Sync.** Reading the folder, exchanging encrypted objects, applying what
  arrived — each step in the background, with the phase on the list row and
  on the folder's page. One sync per folder at a time; the repository lock
  is the CLI's, so the terminal and this window exclude each other.
- **The format is the CLI's.** Configuration, layout, locking, file index and
  operation journal are the ones `tresse` writes, so a folder synced here can
  be read by the terminal and the other clients.

## Building

Rust is pinned by `rust-toolchain.toml` (1.95). On Linux the toolkit needs
the usual development packages:

```sh
sudo apt-get install -y pkg-config libwayland-dev libxkbcommon-dev \
  libxkbcommon-x11-dev libxcb1-dev libvulkan-dev libfontconfig-dev libfreetype-dev
cargo build -p tresse-gui
```

The first build compiles the toolkit and takes a while. Tests:

```sh
cargo test --workspace
```

The interface itself is checked by playing the scenarios in
`crates/tresse-gui/tests/ui/` into the real application — keyboard and pointer
steps in, the application's own state out:

```sh
build-aux/ui-tests.sh --headless
```

Windows and macOS are built by CI (cross-compiled with `cargo-xwin` and wine
on Linux, and natively on a macOS runner); the scripts are in `build-aux/`.
Android is packaged from the same Rust library: see `android/README.md`.

## Layout

```
crates/tresse-client   the filesystem client (Tresse, over a vault seam)
crates/tresse-gui      the window
data/                  desktop entry, AppStream record, icons, macOS plist
build-aux/             flatpak manifest, packaging and publishing scripts
android/               the Gradle project that packages the library
.github/workflows/     the CI that builds and publishes every package
```

## Licence

GPL-3.0. See `LICENSE`.

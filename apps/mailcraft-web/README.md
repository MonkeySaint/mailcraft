# MailCraft for the web

The MailCraft interface compiled to WebAssembly and drawn into a browser canvas by eframe. It is the same UI as
the desktop app (`crates/ui-egui`) running on the same engine, with three differences:

- **No mail server connection.** The page opens the synthetic sample mailbox (people at example.com/.org/.net),
  dated relative to the moment the page loads.
- **Changes stay in this browser.** The mailbox is saved as one JSON document in the site's local storage
  (key `mailcraft.mailbox`) about two seconds after a change, and right away when the page loses focus. Nothing
  is sent anywhere. Add `?reset` to the address (`http://127.0.0.1:8080/?reset`) to throw the saved mailbox away
  and start again from the sample.
- **No desktop-only features.** The control channel (`--control-port`) is not built, and saving files to disk
  reports an error.

## Requirements

- The Rust toolchain from the repository root, plus the wasm target: `rustup target add wasm32-unknown-unknown`.
- [Trunk](https://trunkrs.dev) (MIT OR Apache-2.0): `cargo install --locked trunk`. Trunk also fetches a
  matching `wasm-bindgen` CLI the first time it builds.

## Develop

```sh
cd apps/mailcraft-web
trunk serve
```

Then open <http://127.0.0.1:8080>. Trunk rebuilds and reloads the page when sources change. The server listens on
127.0.0.1 only (`Trunk.toml`).

## Release build

```sh
cd apps/mailcraft-web
trunk build --release
```

The site is written to `apps/mailcraft-web/dist/` (`index.html`, the `.wasm` and its JavaScript loader, the
icon). Serve that folder from any static web server. If it lives below the site root, pass the path:
`trunk build --release --public-url /mailcraft/`. Serve `.wasm` files as `application/wasm`.

## Checking that it compiles without Trunk

```sh
cargo build -p mailcraft-web --target wasm32-unknown-unknown
```

Built for the native target, `mailcraft-web` only prints a pointer to this file; its storage and clock logic
(`src/persist.rs`, `src/shell.rs`) is unit-tested natively with `cargo test -p mailcraft-web`.

## Files

| File | Job |
|---|---|
| `index.html` | Loader page: canvas, loading notice with the app icon, a message if the app stops |
| `Trunk.toml` | Trunk settings (`dist/`, 127.0.0.1:8080) |
| `src/main.rs` | Entry point (browser on wasm32, a short notice natively) |
| `src/browser.rs` | Browser glue: canvas, `Date`, local storage, eframe's `WebRunner` |
| `src/shell.rs` | eframe app: browser clock into the session, autosave |
| `src/persist.rs` | Mailbox ⇄ stored JSON document |

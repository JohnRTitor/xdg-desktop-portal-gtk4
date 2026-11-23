# Project Rules — xdg-desktop-portal-gtk4

This is a Linux desktop portal daemon implementing the `org.freedesktop.impl.portal` D-Bus specification using GTK4, Tokio, and zbus. It runs as a systemd-managed service.

## Architecture

- Split portals into `dbus.rs` (D-Bus frontend) and `gui.rs` (GTK backend) inside `src/portals/<name>/`.
- Anchor the D-Bus lifecycle in the `Portal` struct (`src/core/mod.rs`); its destruction unregisters the D-Bus name.
- Use the `Drop` trait for deterministic cleanup of external resources (e.g., temporary files).
- Register portal requests via `SessionManager` to cancel operations if the client disconnects.

## Async & Threading

- Run the GTK4 event loop strictly on the main thread (`gtk4` objects are `!Send` and `!Sync`).
- Run the Tokio runtime on a dedicated background OS thread (`#[tokio::main(flavor = "current_thread")]`) to handle D-Bus methods without blocking the UI. This single-threaded runtime is sufficient since DBus operations are largely I/O bound.
- Offload heavy or blocking operations from Tokio using `tokio::task::spawn_blocking` to prevent freezing the single-threaded async runtime.
- Offload blocking background work from the GTK thread (e.g., thumbnail generation) using a shared `glib::ThreadPool` rather than permanent `std::thread` loops. Use `ThreadPool::shared`, not `exclusive`: exclusive pools keep their workers for the process lifetime, while shared workers return to GLib's global pool and terminate after its idle timeout.
- Dispatch UI operations from Tokio to GTK using `crate::gui::run_ui_task(proxy, ...)` and await the channel response.
- Cache long-lived GTK objects across multiple requests using `thread_local!` within the GTK thread.

### Reading thread dumps

Useful when debugging with `ps -L`, `gdb`, or similar. Thread names are chosen by libraries and may change between versions; only the ownership and lifetime are stable.

- `pool-*` — GLib thread-pool workers. This includes thumbnail jobs, `GTask`, and any other non-exclusive GLib pool. `GThreadPool` does not let callers name its threads.
- `pool-spawner` — GLib's global thread-pool spawner; lives for the process lifetime.
- `disk$*` (often shown as `process:disk$0..3`) — Mesa shader/disk-cache workers created by `util_queue_init(..., "disk$", ...)`. They persist for the process lifetime and are not portal work.
- `tokio-zbus` — the dedicated Tokio runtime thread; `gmain` — the GTK main loop; `dconf worker`, `gdbus`, and `[pango] fontcon` — standard GLib/GTK runtime threads.

## DBus

- Implement D-Bus services using `zbus::interface` blocks.
- Keep D-Bus handlers asynchronous but lightweight; delegate heavy blocking work or UI logic.
- Gracefully handle name-lost events by exiting, allowing seamless replacement (`--replace`).

## Error Handling

- Use `thiserror` for domain and portal-specific error types (e.g., `UiError`).
- Return `zbus::fdo::Result` from D-Bus methods to propagate D-Bus specific errors cleanly.
- Prefer `?` for error propagation instead of deep nested `match` blocks.

## Code Style

- Use `let-else` statements and `if let ... && let ...` chains to flatten nested control flow and reduce indentation.
- Use `parking_lot::Mutex` (not `tokio::sync::Mutex` or `std::sync::Mutex`) for shared state since lock durations are microscopic.
- **Fail-Fast:** Systemd will cleanly restart the daemon on panic. `parking_lot::Mutex` does not implement lock poisoning, so there is no need to unwrap results or handle `PoisonError`.
- **Never hold a `MutexGuard` across an `.await` point.** Scope the guard inside a separate block if necessary.
- Group related magic values and repeated string literals into constants.
- Prefer iterator chains (`.map().filter().collect()`) over manual for-loops allocating into a `Vec`.
- For formatting, always use: `cargo fmt -- --config imports_granularity=One,unstable_features=true`

## Memory & allocation discipline

Allocation work is measured, not guessed. `src/alloc_probe.rs` provides the
test-only `#[global_allocator]` shim and `AllocScope`; use it for any change
that claims to reduce allocations. Pin layout changes with `size_of` tests
following `src/core/session.rs` and `src/gui/windowing/window_identifier.rs`.
Run the suite under `dbus-run-session -- cargo test --all-features`: without a
session bus the D-Bus tests silently skip. Do not introduce a custom
allocator, arena, jemalloc or mimalloc — the allocation rate is the problem,
not the allocator.

Patterns that apply here:

- **Borrow at the D-Bus edge.** `Header::sender()` borrows for the whole
  handler; pass `&str` into `run_request` instead of copying a `String`. Move
  write-once fields (activation tokens, owned GUI strings) out of options
  structs rather than cloning them; `SessionManager` makes the one owned copy
  it needs for its key.
- **Box state that never grows.** `Box<str>` / `Box<[T]>` for request paths,
  sender keys, registry specs and MIME lists drops the capacity word and its
  slack. `Box<str>` implements zbus `Type`, but has no `Into<Variant>`, so
  arrays built via `Variant::from(Vec<T>)` still need `&str`.
- **Borrow zbus values instead of cloning them.** `<&Structure>` and borrowed
  `Array` conversions replace `Structure::try_from` / `Vec<T>::try_from` when
  only one field or the first element is read. Keep one-level
  `Variant::Variant` unwrapping and whole-value rejection on malformed
  elements if that was the old behavior.
- **Keep small buffers on the stack.** `smallvec` is a direct dependency:
  use `SmallVec<[T; N]>` for short internal lists (notification actions),
  `Cow` for borrow-or-own (file filters), and `with_capacity` when the upper
  bound is known. A capacity hint that over-counts skipped items can force a
  heap spill.
- **Cache repeated construction.** `SupportedOptions` is a `LazyLock`, not a
  HashMap rebuilt on every property read. Hoist key strings and config names
  out of signal and file-monitor callbacks.
- **Reuse borrowed state in hot paths.** `SettingsState::get_ref` avoids
  cloning `OwnedVariant` on every reload diff; only the wire reply needs an
  owned clone. Reuse thumbnail paths and URIs across tiers instead of
  recomputing MD5 and `fs::read` several times per file.
- **Avoid copying bytes.** `Bytes::from_owned` over `Bytes::from(&Vec)`,
  `String::from_utf8` with a lossy fallback, a compile-time CRC table, and one
  exact-size buffer for PNG tEXt chunks. Guard `String::replace` with a
  `contains` check when the common case has no match.
- **Watch layout.** Box large rare enum variants, narrow integers only where
  the domain allows, and reorder fields to remove padding. Pin results with
  `#[cfg(target_pointer_width = "64")] size_of` tests. Remember `Arc<str>` is
  a fat pointer (16 bytes), not 8.

Never trade correctness for an allocation: keep validation semantics, wire
signatures and existing assertions intact. Use `Weak` when an identity marker
must not own its target (activation tokens), and check `Arc::ptr_eq` only
after upgrading. Changes to host ids, icon arrays or mailto URLs need a test
that fails on revert.

## Logging

- Use `tracing` (`error!`, `warn!`, `debug!`, `info!`, `instrument`) for all logging.
- On Linux with the `systemd` feature, native journald integration uses `tracing-journald` automatically when `stderr` is connected to the journal. The journald path is gated to `target_os = "linux"`; on other targets logging always goes to stderr.

## Testing

- Use `#[tokio::test]` for async test functions.
- Gracefully skip integration tests that require a D-Bus session if `Connection::session()` fails.

## Releases

`Cargo.toml`'s `[package] version` is the single source of truth. A release is tagged as `v<version>`; the tag triggers `.github/workflows/release.yml`.

When bumping the version, update **all** of these:

- `Cargo.toml`, single source of truth — bump `[package] version` to the new `X.Y.Z`.
- `Cargo.lock` — regenerate the root package entry (plain `cargo check` /
  `cargo build` does this). CI and all packaging recipes build with `--locked`,
  so a stale lockfile breaks the release.
- `packaging/archlinux/PKGBUILD` — set `pkgver=X.Y.Z` and reset `pkgrel=1`. The
  recipe builds from tag `v$pkgver`, so the tag must exist before an external
  `makepkg` run; the `pkgver()` function derives the version from `git describe`.
  PKGBUILD has no changelog field — the user-facing notes live elsewhere.
- `packaging/debian/changelog` — prepend a new
  `xdg-desktop-portal-gtk4 (X.Y.Z-1) unstable; urgency=<low|medium>` stanza with
  the user-facing changes and the ` -- <maintainer>  <date>` trailer (`date -R`
  gives the required RFC 2822 format). `packaging/debian/rules` fails the build
  if this version does not match `Cargo.toml`.
- `packaging/fedora/xdg-desktop-portal-gtk4.spec` — bump `Version:` and prepend
  a matching `* <date> <maintainer> - X.Y.Z-1` entry to `%changelog` with the
  same user-facing notes. `%prep` fails if `Version:` does not match
  `Cargo.toml`.

Review the commits from the previous tag, and the proposed tag checkpoint (usually HEAD).
Write the changes as short, end-user-facing bullets (what changed for people
running the portal, not implementation details). Keep the distro packaging in
agreement, and put the same notes in tag message.

Order of operations: bump the files above, commit, then tag.

## Environment & Tooling

- If `nix` is present in the environment, `nix shell` or `nix run` can be used to get the packages needed during agent session.

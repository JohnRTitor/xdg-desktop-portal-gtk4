# Project Rules — xdg-desktop-portal-gtk4

This is a Linux desktop portal daemon implementing the `org.freedesktop.impl.portal` D-Bus specification using GTK4, Tokio, and zbus. It runs as a systemd-managed service.

## Architecture

- Split portals into `dbus.rs` (D-Bus frontend) and `gui.rs` (GTK backend) inside `src/portals/<name>/`.
- Anchor the D-Bus lifecycle in the `Portal` struct (`src/core/mod.rs`); its destruction unregisters the D-Bus name.
- Use the `Drop` trait for deterministic cleanup of external resources (e.g., temporary files).
- Register portal requests via `SessionManager` to cancel operations if the client disconnects.

## Async & Threading

- Run the GTK4 event loop strictly on the main thread (`gtk4` objects are `!Send` and `!Sync`).
- Run the Tokio runtime on a dedicated background OS thread (`#[tokio::main]`) to handle D-Bus methods without blocking the UI.
- Dispatch UI operations from Tokio to GTK using `crate::gui::run_ui_task(proxy, ...)` and await the channel response.
- Cache long-lived GTK objects across multiple requests using `thread_local!` within the GTK thread.

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

## Logging

- Use `tracing` (`error!`, `warn!`, `debug!`, `info!`, `instrument`) for all logging.
- Native systemd integration uses `tracing-journald` automatically when `stderr` is connected to the journal.

## Testing

- Use `#[tokio::test]` for async test functions.
- Gracefully skip integration tests that require a D-Bus session if `Connection::session()` fails.

## Environment & Tooling

- If `nix` is present in the environment, `nix shell` or `nix run` can be used to get the packages needed during agent session.

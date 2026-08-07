# D-Bus Portal Integration Tests

This directory contains integration tests that exercise the public D-Bus API of `xdg-desktop-portal-gtk4`.

## Running Tests

To run all tests (both unit and integration tests), use standard cargo commands:
```bash
cargo test
```

To run a specific test file (e.g., for the access portal):
```bash
cargo test --test access_test
```

## Architecture

These tests verify that the portal implementations properly register on the D-Bus, successfully deserialize standard requests from D-Bus clients, and respond correctly according to the `org.freedesktop.portal.*` specifications.

To achieve reliable and isolated testing without requiring a full desktop environment or active Wayland/X11 session:

1. **Isolated Session Bus:** Each test spawns its own temporary D-Bus session using `zbus::connection::Builder::session()?.serve_at(...)`. This prevents tests from interfering with each other or depending on the host's session bus.
2. **Dummy UI Proxy:** Many portals interact with the host GTK environment via a `UiProxy` to spawn file choosers, app choosers, etc. In these tests, we inject a `dummy_proxy()` (defined in `common/mod.rs`) which simply drops the internal channel. When the portal attempts to send a UI request, it immediately receives a channel closed error, which the portal safely translates into a `fdo::Error::Failed` or cancellation response. This allows us to test the D-Bus deserialization and method invocation logic without actually opening GTK windows during CI.

## Adding New Tests

When adding a test for a new portal:
1. Define a `#[proxy]` trait representing the client side of the portal.
2. Use the helpers from `common::mod.rs` to construct the isolated bus and proxy.
3. Invoke the D-Bus methods and assert the expected results (e.g. `is_err()` for dummy UI interactions, or specific `Ok` values for logic-only portals like `Settings`).

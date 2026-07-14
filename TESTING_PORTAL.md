# Testing Portal Documentation

This document provides a comprehensive guide for manually testing the `xdg-desktop-portal-gtk4` implementation. It details the implemented D-Bus interfaces, object paths, bus names, and provides exact commands to test every portal method and monitor signals.

## Overview

- **Bus Name:** `org.freedesktop.impl.portal.desktop.gtk4` (or `org.freedesktop.impl.portal.desktop.gtk` if standard fallback is used, but this implementation registers the former based on the codebase `src/core/mod.rs`).
- **Object Path:** `/org/freedesktop/portal/desktop`
- **Required Environment:** GTK4, D-Bus session bus.
- **Required Services:** `org.freedesktop.portal.Desktop` (the frontend portal daemon) must be configured to use this backend.

## Startup

### Building and Running Manually

1. **Build the project:**
   ```bash
   cargo build
   ```
2. **Run it manually (replacing any existing instance):**
   ```bash
   ./target/debug/xdg-desktop-portal-gtk4 --replace
   ```
3. **Ensure xdg-desktop-portal uses this backend:**
   Create or modify `~/.config/xdg-desktop-portal/portals.conf`:
   ```ini
   [preferred]
   default=gtk4
   ```
   Then restart the frontend portal:
   ```bash
   systemctl --user restart xdg-desktop-portal
   ```

## Debugging

Enable verbose logging to trace requests and observe GTK/Glib errors:

```bash
RUST_LOG=trace ./target/debug/xdg-desktop-portal-gtk4 --replace
```
- **Why:** Shows trace logs from the Rust backend, including incoming D-Bus requests and errors.

```bash
G_MESSAGES_DEBUG=all ./target/debug/xdg-desktop-portal-gtk4
```
- **Why:** Enables GTK/GIO debug messages, useful for diagnosing UI or AppInfo issues.

```bash
journalctl --user -u xdg-desktop-portal-gtk4 -f
```
- **Why:** Follows the logs if the service is started by systemd.

```bash
dbus-monitor "destination='org.freedesktop.impl.portal.desktop.gtk4'"
```
- **Why:** Logs all raw D-Bus traffic sent to the portal backend.

```bash
gdbus monitor --session --dest org.freedesktop.impl.portal.desktop.gtk4
```
- **Why:** Provides a higher-level view of D-Bus method calls and signals emitted by the portal.

## Request Objects

The portal backend often processes asynchronous actions via **Request Objects**. When a method is called (e.g., `FileChooser.OpenFile`), it receives a `handle` object path representing the request.
- **Observing:** The backend exports an object at the given `handle` path with the interface `org.freedesktop.impl.portal.Request`.
- **Completion:** The frontend expects the backend to emit a signal or return a `Response` struct `(u32, a{sv})` indicating success (0), cancellation (1), or error (2).
- **Inspection:** You can introspect a request object while a dialog is open:
  ```bash
  gdbus introspect --session --dest org.freedesktop.impl.portal.desktop.gtk4 --object-path /org/freedesktop/portal/desktop/request/1_1/some_handle
  ```
- **Closing:** You can force close a request by calling `Close()` on it:
  ```bash
  gdbus call --session --dest org.freedesktop.impl.portal.desktop.gtk4 --object-path /org/freedesktop/portal/desktop/request/1_1/some_handle --method org.freedesktop.impl.portal.Request.Close
  ```

---

## Implemented Portals

### 1. Access (`org.freedesktop.impl.portal.Access`)

#### Overview
- **Interface:** `org.freedesktop.impl.portal.Access`
- **Object Path:** `/org/freedesktop/portal/desktop`

#### Method Testing

**`AccessDialog`**
Prompts the user for access to a resource.
```bash
gdbus call \
  --session \
  --dest org.freedesktop.impl.portal.desktop.gtk4 \
  --object-path /org/freedesktop/portal/desktop \
  --method org.freedesktop.impl.portal.Access.AccessDialog \
  "/org/freedesktop/portal/desktop/request/1_1/access1" \
  "org.example.App" \
  "" \
  "Permission Required" \
  "App wants access" \
  "Please allow access." \
  "{'modal': <true>}"
```

**dbus-send:**
```bash
dbus-send --session --print-reply --dest=org.freedesktop.impl.portal.desktop.gtk4 \
  /org/freedesktop/portal/desktop \
  org.freedesktop.impl.portal.Access.AccessDialog \
  objpath:"/org/freedesktop/portal/desktop/request/1_1/access1" \
  string:"org.example.App" string:"" string:"Title" string:"Subtitle" string:"Body" \
  dict:string:variant:modal,boolean:true
```
- **Expected Output:** `(uint32 0, {'choices': <@a(ss) []>})` on success.
- **Error Cases:** Reject the dialog to observe a `1` (cancelled) return value. Send an invalid handle to trigger D-Bus errors.

---

### 2. Account (`org.freedesktop.impl.portal.Account`)

#### Overview
- **Interface:** `org.freedesktop.impl.portal.Account`
- **Object Path:** `/org/freedesktop/portal/desktop`
- **Required Services:** `org.freedesktop.Accounts`

#### Method Testing

**`GetUserInformation`**
```bash
gdbus call \
  --session \
  --dest org.freedesktop.impl.portal.desktop.gtk4 \
  --object-path /org/freedesktop/portal/desktop \
  --method org.freedesktop.impl.portal.Account.GetUserInformation \
  "/org/freedesktop/portal/desktop/request/1_1/account1" \
  "org.example.App" \
  "" \
  "{'reason': <'Authentication'>}"
```
- **Expected Output:** Returns user ID, name, and image URI on success.

---

### 3. AppChooser (`org.freedesktop.impl.portal.AppChooser`)

#### Overview
- **Interface:** `org.freedesktop.impl.portal.AppChooser`
- **Object Path:** `/org/freedesktop/portal/desktop`

#### Method Testing

**`ChooseApplication`**
```bash
gdbus call \
  --session \
  --dest org.freedesktop.impl.portal.desktop.gtk4 \
  --object-path /org/freedesktop/portal/desktop \
  --method org.freedesktop.impl.portal.AppChooser.ChooseApplication \
  "/org/freedesktop/portal/desktop/request/1_1/appchooser1" \
  "org.example.App" \
  "" \
  "['org.gnome.TextEditor.desktop']" \
  "{'filename': <'test.txt'>}"
```
- **Expected Output:** Returns `(uint32 0, {'choice': <'org.gnome.TextEditor.desktop'>})`.

**`UpdateChoices`**
Updates the choices in an active AppChooser dialog.
```bash
gdbus call \
  --session \
  --dest org.freedesktop.impl.portal.desktop.gtk4 \
  --object-path /org/freedesktop/portal/desktop \
  --method org.freedesktop.impl.portal.AppChooser.UpdateChoices \
  "/org/freedesktop/portal/desktop/request/1_1/appchooser1" \
  "['org.gnome.gedit.desktop']"
```

---

### 4. DynamicLauncher (`org.freedesktop.impl.portal.DynamicLauncher`)

#### Overview
- **Interface:** `org.freedesktop.impl.portal.DynamicLauncher`
- **Properties:** `SupportedLauncherTypes` (u32), `version` (u32)

#### Method Testing

**`PrepareInstall`**
```bash
gdbus call \
  --session \
  --dest org.freedesktop.impl.portal.desktop.gtk4 \
  --object-path /org/freedesktop/portal/desktop \
  --method org.freedesktop.impl.portal.DynamicLauncher.PrepareInstall \
  "/org/freedesktop/portal/desktop/request/1_1/dynlaunch1" \
  "org.example.App" \
  "" \
  "My App" \
  "<'system-run'>" \
  "{'editable_name': <true>}"
```

**`RequestInstallToken`**
```bash
gdbus call \
  --session \
  --dest org.freedesktop.impl.portal.desktop.gtk4 \
  --object-path /org/freedesktop/portal/desktop \
  --method org.freedesktop.impl.portal.DynamicLauncher.RequestInstallToken \
  "org.gnome.Software" \
  "{}"
```
- **Expected Output:** `(uint32 0,)` for allowed apps, `(uint32 2,)` for denied.

---

### 5. Email (`org.freedesktop.impl.portal.Email`)

#### Overview
- **Interface:** `org.freedesktop.impl.portal.Email`
- **Behavior:** Constructs a `mailto:` URI and opens the default email client.

#### Method Testing

**`ComposeEmail`**
```bash
gdbus call \
  --session \
  --dest org.freedesktop.impl.portal.desktop.gtk4 \
  --object-path /org/freedesktop/portal/desktop \
  --method org.freedesktop.impl.portal.Email.ComposeEmail \
  "/org/freedesktop/portal/desktop/request/1_1/email1" \
  "org.example.App" \
  "" \
  "{'address': <'user@example.com'>, 'subject': <'Hello'>}"
```

---

### 6. FileChooser (`org.freedesktop.impl.portal.FileChooser`)

#### Overview
- **Interface:** `org.freedesktop.impl.portal.FileChooser`

#### Method Testing

**`OpenFile`**
```bash
gdbus call \
  --session \
  --dest org.freedesktop.impl.portal.desktop.gtk4 \
  --object-path /org/freedesktop/portal/desktop \
  --method org.freedesktop.impl.portal.FileChooser.OpenFile \
  "/org/freedesktop/portal/desktop/request/1_1/filechooser1" \
  "org.example.App" \
  "" \
  "Open a Document" \
  "{'multiple': <false>}"
```

**`SaveFile`**
```bash
gdbus call \
  --session \
  --dest org.freedesktop.impl.portal.desktop.gtk4 \
  --object-path /org/freedesktop/portal/desktop \
  --method org.freedesktop.impl.portal.FileChooser.SaveFile \
  "/org/freedesktop/portal/desktop/request/1_1/filechooser2" \
  "org.example.App" \
  "" \
  "Save Document" \
  "{'current_name': <'untitled.txt'>}"
```

**`SaveFiles`**
```bash
gdbus call \
  --session \
  --dest org.freedesktop.impl.portal.desktop.gtk4 \
  --object-path /org/freedesktop/portal/desktop \
  --method org.freedesktop.impl.portal.FileChooser.SaveFiles \
  "/org/freedesktop/portal/desktop/request/1_1/filechooser3" \
  "org.example.App" \
  "" \
  "Save Multiple" \
  "{'files': <[b'doc1.txt', b'doc2.txt']>}"
```

---

### 7. Inhibit (`org.freedesktop.impl.portal.Inhibit`)

#### Overview
- **Interface:** `org.freedesktop.impl.portal.Inhibit`
- **Signals:** `StateChanged(o, a{sv})`

#### Method Testing

**`Inhibit`**
```bash
gdbus call \
  --session \
  --dest org.freedesktop.impl.portal.desktop.gtk4 \
  --object-path /org/freedesktop/portal/desktop \
  --method org.freedesktop.impl.portal.Inhibit.Inhibit \
  "/org/freedesktop/portal/desktop/request/1_1/inhibit1" \
  "org.example.App" \
  "" \
  8 \
  "{'reason': <'Watching a video'>}"
```

**`CreateMonitor`**
```bash
gdbus call \
  --session \
  --dest org.freedesktop.impl.portal.desktop.gtk4 \
  --object-path /org/freedesktop/portal/desktop \
  --method org.freedesktop.impl.portal.Inhibit.CreateMonitor \
  "/org/freedesktop/portal/desktop/request/1_1/inhibit2" \
  "/org/freedesktop/portal/desktop/session/1_1/session1" \
  "org.example.App" \
  ""
```

---

### 8. Lockdown (`org.freedesktop.impl.portal.Lockdown`)

#### Overview
- **Interface:** `org.freedesktop.impl.portal.Lockdown`
- **Behavior:** Exposes boolean properties indicating if certain features are locked down (all default to `false`).

#### Method Testing (Properties)

```bash
gdbus call \
  --session \
  --dest org.freedesktop.impl.portal.desktop.gtk4 \
  --object-path /org/freedesktop/portal/desktop \
  --method org.freedesktop.DBus.Properties.GetAll \
  "org.freedesktop.impl.portal.Lockdown"
```
- **Expected Output:** `{ 'disable-printing': <false>, 'disable-save-to-disk': <false>, ... }`

---

### 9. Notification (`org.freedesktop.impl.portal.Notification`)

#### Overview
- **Interface:** `org.freedesktop.impl.portal.Notification`
- **Signals:** `ActionInvoked(s, s, s, av)`

#### Method Testing

**`AddNotification`**
```bash
gdbus call \
  --session \
  --dest org.freedesktop.impl.portal.desktop.gtk4 \
  --object-path /org/freedesktop/portal/desktop \
  --method org.freedesktop.impl.portal.Notification.AddNotification \
  "org.example.App" \
  "notif1" \
  "{'title': <'Hello'>, 'body': <'World'>}"
```

**`RemoveNotification`**
```bash
gdbus call \
  --session \
  --dest org.freedesktop.impl.portal.desktop.gtk4 \
  --object-path /org/freedesktop/portal/desktop \
  --method org.freedesktop.impl.portal.Notification.RemoveNotification \
  "org.example.App" \
  "notif1"
```

---

### 10. Print (`org.freedesktop.impl.portal.Print`)

#### Overview
- **Interface:** `org.freedesktop.impl.portal.Print`

#### Method Testing

**`PreparePrint`**
```bash
gdbus call \
  --session \
  --dest org.freedesktop.impl.portal.desktop.gtk4 \
  --object-path /org/freedesktop/portal/desktop \
  --method org.freedesktop.impl.portal.Print.PreparePrint \
  "/org/freedesktop/portal/desktop/request/1_1/print1" \
  "org.example.App" \
  "" \
  "Print Document" \
  "{}" \
  "{}" \
  "{}"
```

---

### 11. Settings (`org.freedesktop.impl.portal.Settings`)

#### Overview
- **Interface:** `org.freedesktop.impl.portal.Settings`
- **Signals:** `SettingChanged(s, s, v)`
- **Behavior:** Translates `org.gnome.desktop.interface` and `org.freedesktop.appearance` settings.

#### Method Testing

**`Read`**
```bash
gdbus call \
  --session \
  --dest org.freedesktop.impl.portal.desktop.gtk4 \
  --object-path /org/freedesktop/portal/desktop \
  --method org.freedesktop.impl.portal.Settings.Read \
  "org.freedesktop.appearance" \
  "color-scheme"
```

**`ReadAll`**
```bash
gdbus call \
  --session \
  --dest org.freedesktop.impl.portal.desktop.gtk4 \
  --object-path /org/freedesktop/portal/desktop \
  --method org.freedesktop.impl.portal.Settings.ReadAll \
  "['org.freedesktop.appearance']"
```

#### Signal Monitoring
```bash
dbus-monitor "interface='org.freedesktop.impl.portal.Settings',member='SettingChanged'"
```
Change a GNOME setting (e.g., toggle dark mode) and observe the signal being emitted.

---

### 12. Usb (`org.freedesktop.impl.portal.Usb`)

#### Overview
- **Interface:** `org.freedesktop.impl.portal.Usb`

#### Method Testing

**`AcquireDevices`**
```bash
gdbus call \
  --session \
  --dest org.freedesktop.impl.portal.desktop.gtk4 \
  --object-path /org/freedesktop/portal/desktop \
  --method org.freedesktop.impl.portal.Usb.AcquireDevices \
  "/org/freedesktop/portal/desktop/request/1_1/usb1" \
  "" \
  "org.example.App" \
  "[('device1', {'ID_VENDOR_FROM_DATABASE': <'Logitech'>}, {})]" \
  "{}"
```

---

## Validation Checklist

Before submitting changes, ensure the following checklist passes:

- [ ] **Backend Starts:** `xdg-desktop-portal-gtk4 --replace` starts without panics or continuous restarts.
- [ ] **Interfaces Exported:** `gdbus introspect` on the object path shows all expected D-Bus interfaces.
- [ ] **Method Execution:** Invoking methods via `gdbus` or `dbus-send` successfully displays GTK UI dialogs.
- [ ] **Return Values:** Approving and rejecting dialogs yield correct D-Bus return values `(0` for success, `1` for cancellation, `2` for error) and expected payloads.
- [ ] **Request Objects:** Request objects correctly disappear after being resolved or explicitly closed.
- [ ] **Signals:** Signals like `SettingChanged` and `ActionInvoked` are correctly emitted when underlying state changes or actions are taken.
- [ ] **Error Handling:** Invalid D-Bus parameters or missing capabilities are gracefully handled without crashing the daemon.

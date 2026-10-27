# demirror: virtual monitors over the network, in Rust

## Goal

A host computer creates any number of **virtual monitors** on request. Any device
(PC, tablet, phone) shows one of them, either in a **web browser** (no install)
or in a **native Rust client app**. Monitors are created, used and closed on demand.

## Architecture

```
 ┌─────────────────────────── host (vmon-server) ───────────────────────────┐
 │                                                                          │
 │  REST API ──► MonitorManager ──► Monitor #1 ──► DisplaySource (backend)  │
 │  (create /    (id → monitor)     Monitor #2      ├ synthetic  (any OS)   │
 │   list /                         ...             ├ wlr        (Wayland)  │
 │   close)                           │             ├ x11        (planned)  │
 │                                    ▼             ├ gnome/kde  (planned)  │
 │                         capture thread → JPEG    ├ windows    (planned)  │
 │                         → watch channel          └ macos      (planned)  │
 │                                    │                                     │
 │  WebSocket /ws/monitors/{id} ◄─────┘   (only encodes while viewed)       │
 └──────────────────────────────────────────────────────────────────────────┘
          │ binary JPEG frames + JSON control              │
          ▼                                                ▼
   Browser viewer (served by host)                 vmon-client (native Rust,
   phone / tablet / any PC                         Linux / Windows / macOS)
```

### Crates

| crate         | role                                                                 |
|---------------|----------------------------------------------------------------------|
| `vmon-proto`  | Shared wire types: `MonitorSpec`, `MonitorInfo`, `SourceSpec`, frame header |
| `vmon-server` | HTTP/WebSocket server, monitor lifecycle, capture backends, JPEG encoding, embedded web UI |
| `vmon-client` | Native viewer: can create a monitor, stream it into a window, and close it on exit |
| `demirror`    | The app (egui): **Host** runs the server with a QR code, live monitor thumbnails and the virtual display status; **Connect** finds hosts (mDNS), detects this screen's resolution and shows a monitor full screen |
| `vmon-vdisplayd` | Linux virtual display daemon (root): plugs VKMS displays in and out, serves their pixels to the server over `/run/demirror/vdisplay.sock` |

### Backends (`DisplaySource` trait)

What a "virtual monitor" can show depends on the host OS, so each OS gets a backend:

| backend        | what it does                                                                 | status  |
|----------------|------------------------------------------------------------------------------|---------|
| `vdisplay`     | **Linux virtual display (extends the desktop).** `vmon-vdisplayd` (root daemon) creates a VKMS device per monitor through configfs and reads its framebuffer with a KMS grab (GETFB2 + dma-buf). Works with any desktop that handles hot-plugged displays | Phase 1 |
| `synthetic`    | Animated test pattern, pure Rust, any OS. Used to test the pipeline end to end | Phase 1 |
| `wlr`          | Captures a Wayland output through `wlr-screencopy` (pure Rust, no C libs). It can mirror an existing screen, or **extend** the desktop when the compositor can create headless outputs (sway `create_output`, Hyprland `output create headless`), configured with `--create-output-cmd` / `--destroy-output-cmd` | Phase 1 |
| `x11`          | `xrandr --setmonitor` virtual monitor + `x11rb` capture                       | Phase 2 |
| `gnome`        | Mutter `ScreenCast.RecordVirtual` (real virtual monitor) + PipeWire           | Phase 2 |
| `kde`          | KWin `zkde_screencast` virtual output                                         | Phase 2 |
| `windows`      | GDI capture of any display (pure Rust, `windows-sys`). Paired with a virtual display driver it **extends** the desktop: the driver adds the monitor, demirror streams it (`--create-output-cmd` runs the driver's tool) | Phase 1 |
| `windows-idd`  | Own IddCx driver with direct control + DXGI Desktop Duplication               | Phase 3 |
| `macos`        | `CGVirtualDisplay` + ScreenCaptureKit                                         | Phase 3 |

> To make a real *extra* monitor, the OS (or its compositor) has to provide a
> virtual-display API, which differs per OS. Everything else (protocol, server,
> clients, lifecycle) is the same on every platform. kompact (this machine's
> compositor) offers `wlr-screencopy` but no way to create outputs yet. Adding a
> headless-output command to it would make the `wlr` backend extend the desktop here too.

## Phases

1. **Phase 1 (this commit):** workspace, protocol, server with REST + WebSocket,
   multi-monitor manager, `synthetic` + `wlr` (Linux) + `windows` backends, browser viewer + dashboard,
   native Rust client, token auth, LAN address printing.
2. **Phase 2:** input back-channel (mouse/touch/keyboard to the host through
   `wlr-virtual-pointer` / `uinput` / `SendInput`), X11/GNOME/KDE backends, mDNS discovery.
3. **Phase 3:** own Windows IddCx driver, macOS virtual display backend, H.264 (hardware
   encode) via WebCodecs/WebRTC, TLS, a WASM browser client, Android/iOS packaging.

## API

| method | path                     | body / notes                                         |
|--------|--------------------------|------------------------------------------------------|
| GET    | `/api/sources`           | available backends and capturable outputs            |
| GET    | `/api/monitors`          | list monitors                                        |
| POST   | `/api/monitors`          | `MonitorSpec` JSON, creates a monitor and returns `MonitorInfo` |
| DELETE | `/api/monitors/{id}`     | close the monitor                                    |
| GET    | `/ws/monitors/{id}`      | WebSocket: first a JSON `MonitorInfo` text message, then binary JPEG frames |
| GET    | `/`  and `/view/{id}`    | web dashboard and full-screen viewer                 |

If the server runs with `--token`, every request needs `?token=…` (or an `Authorization: Bearer …` header for the API).

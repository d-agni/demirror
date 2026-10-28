# demirror

Turn phones, tablets and other computers into **extra monitors** for your computer,
written in Rust. The desktop extends onto each device like onto a real second
monitor, and its resolution is detected from the device's own screen.

See [docs/PLAN.md](docs/PLAN.md) for the architecture and roadmap.

## Install (Linux)

```sh
cargo build --release
sudo packaging/linux/install-linux.sh
```

This installs the **demirror** app (in your applications menu), the command-line
`vmon-server` and `vmon-client`, and `vmon-vdisplayd`, the service that plugs
virtual monitors into the kernel. Everything is pure Rust; no C libraries are needed.
On Windows, `cargo build --release` with the MSVC toolchain builds the app, server and
client (virtual displays on Windows are on the roadmap; mirroring works).

## Use it

**On the computer whose desktop you want to extend:** open **demirror**, page
**Share this computer**, and press **Start sharing**. It shows a QR code and an address such as
`http://192.168.29.97:7878`.

**On a phone or tablet:** scan the QR code (or open the address) and tap
**Start monitor**. The page detects the screen's resolution (pixel density and
orientation included), the host plugs in a display of that size, and the page goes
full screen. Rotate the device or resize the window and the monitor follows.
**Disconnect** unplugs it again. Pick **Smooth**, **Balanced**, **Sharp** or
**Data saver** first for the frame rate and quality.

**On another computer:** open **demirror**, page **Use as monitor**. Computers sharing
on the network are listed by themselves, each with its own **Start monitor** (or
connect by address); the monitor's size is shown before it starts, and it opens full
screen. Esc or **Disconnect** ends it,
double-click switches between full screen and a window.

**Settings** (remembered between runs): whether devices can find this computer and
control it, a token, the port, **Share when demirror opens**, and **Open demirror when I
log in** (Linux and Windows).

**Control the host from the device.** Touch, mouse and keyboard on the viewing
device work the computer, like a touchscreen monitor:

| on the device | on the host |
|---|---|
| tap / click | click |
| one-finger drag / mouse drag | drag |
| long press, two-finger tap / right click | right click |
| two-finger drag / mouse wheel | scroll |
| keys (a hardware keyboard, or **Keyboard** on a phone) | the same keys |

A phone or tablet's **Touch** button switches touch to **trackpad**, like a laptop's:
a finger moves the pointer (faster the faster it moves), a tap clicks where the pointer
is, and a long press then moving drags. Two fingers work as above. The choice is
remembered on the device.

**Size.** A monitor has the viewing screen's shape, held upright or sideways, so it fills
the screen. Its pixels are the screen's, but the quality preset caps the longest side
(Smooth 1600, Balanced 1920, Sharp 2560, Data saver 1280): a phone's 2400 pixels would be
too much to draw and send smoothly. On kompact the virtual display gets exactly that size
(kompact 22f); other desktops pick the closest size VKMS offers.

The host turns this off with **Let devices control this computer** on the Share page
(`vmon-server --view-only`). In the app's viewer, Esc then goes to the host too: leave
with the toolbar's **Disconnect**.

The Share page lists every monitor with a live thumbnail, who's watching, and a
**Close** button. **Add test monitor** gives an animated pattern to try a device.

Options: `demirror --share` starts sharing straight away (to start with your
session); `demirror --connect 192.168.1.5 --start` starts a monitor on that host at
once (a tablet or spare laptop kept as a second screen).

The server also runs without a window:

```sh
vmon-server                        # listens on 0.0.0.0:7878, announced on the network
vmon-server --token mysecret       # require a token from every client
```

On Windows, allow demirror through the firewall the first time it shares.

### How the resolution is matched

The device's size goes to the host, which writes it as a mode hint
(`/run/vkms-mode/<display>`) before plugging the display in, and again when the
device turns or resizes. A desktop that reads the hint picks the closest resolution
the display offers (kompact does, since its step 22d); the viewer scales what is
left. A resolution chosen in the desktop's display settings still wins.

### How input reaches the host

`vmon-vdisplayd` gives each virtual display two kernel input devices (uinput): an
absolute pointer, like a virtual machine's tablet, and a keyboard. Keys and buttons
work in any desktop. So that the pointer lands on *its* display, the daemon names it in
`/run/vkms-input/<display>`; a desktop that reads this (kompact since step 22e) maps
that device's positions onto that display.

## Monitor sources

| source                  | what you see                                                         | platforms |
|-------------------------|----------------------------------------------------------------------|-----------|
| **New virtual display** | A real secondary monitor plugged into the host: the desktop **extends** onto it | Linux (any desktop that supports hot-plugged displays); Windows next |
| Test pattern            | Animated pattern with the monitor number                             | all       |
| Screen `NAME`           | Mirror of an existing screen of the host                             | Linux (Wayland), Windows |

## Virtual displays on Linux

demirror plugs virtual monitors into the kernel through **VKMS**, the mainline
kernel's virtual display driver (Linux 6.17+ with `CONFIG_DRM_VKMS`, which Ubuntu
and most distributions ship). Each monitor becomes its own DRM device
(`/dev/dri/cardN`) with a connected display. The desktop sees a newly
plugged-in monitor, the same way it sees a USB DisplayLink dock, and you arrange it, set its
resolution, and drag windows onto it in the normal display settings. demirror
reads what the desktop draws there straight from the kernel, so no
desktop-specific API is involved.

Creating display devices needs root, so a small daemon, `vmon-vdisplayd`,
does that part. The server itself stays unprivileged:

```sh
cargo build --release
sudo packaging/linux/install-linux.sh        # installs and starts the systemd service
sudo vmon-vdisplayd selftest                 # optional: checks the kernel side end to end
```

Then pick **New virtual display** in the web UI, or run `vmon-client new --virtual`.
Closing the monitor unplugs the display. If the server stops or crashes, the daemon
unplugs its displays too.

Requirements and notes:
- The desktop must handle a display device that appears at runtime. GNOME
  (Mutter) and KDE (KWin) do. wlroots compositors and Xorg need multi-GPU support.
  Until the desktop picks the display up, the monitor shows black.
- VKMS has no EDID yet, so the desktop starts the display at a standard mode
  (often 1024×768). Choose the resolution you want in the display settings,
  as you would for a real monitor.

### Other ways to add a display

Instead of virtual displays, the server can run a command that makes the
compositor or a driver add a display, and stream it as a screen:

**Linux, sway**
```sh
vmon-server --create-output-cmd 'swaymsg create_output' \
            --destroy-output-cmd 'swaymsg output {output} unplug'
```

**Linux, Hyprland**
```sh
vmon-server --create-output-cmd 'hyprctl output create headless' \
            --destroy-output-cmd 'hyprctl output remove {output}'
```

**Windows:** install a virtual display driver (an IddCx driver, such as the
open-source *Virtual Display Driver* project). Each virtual display it adds
appears as `Screen \\.\DISPLAYn` in the source list, and you can stream it directly.
If the driver has a command-line tool for adding and removing displays, pass it
as `--create-output-cmd` / `--destroy-output-cmd` so monitors are created on demand.

With these options set, choose **New display** in the web UI, or run `vmon-client new --screen new`.
The display is removed again when the monitor is closed or the server stops.

## Native client

```sh
vmon-client -s http://HOST:7878 list                    # monitors and screens
vmon-client -s http://HOST:7878 new --virtual                # extend the host's desktop
vmon-client -s http://HOST:7878 new --width 1920 --height 1080   # test pattern
vmon-client -s http://HOST:7878 new --screen VGA-1      # stream a host screen
vmon-client -s http://HOST:7878 view 3                  # open an existing monitor
vmon-client -s http://HOST:7878 bench 3                 # fps and bandwidth
vmon-client -s http://HOST:7878 snapshot 3 shot.jpg
vmon-client -s http://HOST:7878 close 3
```

`new` closes the monitor again when the window closes (unless you pass `--keep`).
In the window, press `F` or `F11` for full screen and `Esc` to quit.
`--token` (or `DEMIRROR_TOKEN`) passes the server token.

## HTTP API

| method | path                 | |
|--------|----------------------|--|
| GET    | `/api/sources`       | screen backend and capturable screens |
| GET    | `/api/monitors`      | list monitors |
| POST   | `/api/monitors`      | create: `{"name":"Left","width":1920,"height":1080,"fps":30,"quality":75,"source":{"kind":"virtual"}}`; other sources: `{"kind":"synthetic"}`, `{"kind":"screen","output":"VGA-1"}` |
| GET    | `/api/monitors/{id}` | one monitor |
| POST   | `/api/monitors/{id}/size` | `{"width":2400,"height":1080}`: the viewer's screen changed; a virtual display follows |
| DELETE | `/api/monitors/{id}` | close |
| WS     | `/ws/monitors/{id}`  | one JSON text message with the monitor info, then binary JPEG frames |

A monitor only captures and encodes while at least one client is watching it.
Hosts announce themselves over mDNS as `_demirror._tcp` (TXT `token=required|none`).

## Releases

Each `v*` tag builds a release on GitHub (`.github/workflows/release.yml`):

- **Linux** `demirror-<version>-linux-x86_64.tar.gz`: unpack it and run
  `sudo ./install-linux.sh` (it installs the programs, the menu entry and the virtual
  display service).
- **Windows** `demirror-<version>-windows-x86_64.zip`: unpack it and run `demirror.exe`.
  Windows can share its screens (mirroring) and be a monitor of another computer; adding
  a virtual display there needs a driver that isn't written yet.

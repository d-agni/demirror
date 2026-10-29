# Changelog

## 0.3.1 - 2026-10-29

### Fixed
- Windows: a monitor went black once the viewer sent its screen size. Changing a monitor's
  modes made Windows turn it off; a resize now plugs in a monitor of the new size in its
  place, at the same position.
- Windows: the first frame of a new monitor was black; demirror now waits for the driver's.
- Windows: with **Main display** off, the host's own screen stays the main one, even where
  Windows remembered the demirror screen as main.
- A browser on the host itself only watches: its pointer over the picture pulled the host's
  pointer onto the monitor and kept it there.
- Building the driver works where `cl` is on PATH outside a developer prompt, and where the
  Windows SDK has no `signtool` (it's then fetched from NuGet).

### Added
- **Touchscreen** (Windows hosts): a phone's or tablet's fingers are Windows' own touch
  (`InjectTouchInput`), so programs scroll, flick, pinch-zoom and press-and-hold as on a
  touch laptop, with up to 10 fingers. It's the new default; the Touch button still
  switches to **mouse** (formerly "direct") or **trackpad**. Fingers left down when a viewer
  disconnects are lifted.
- **Main display** (Windows hosts): the new screen becomes the main display while it's on,
  so programs open on it; the host's own screen is the main one again when it closes.
- CI signs the Windows programs, installer and uninstaller with the repository's
  code-signing certificate (`DEMIRROR_SIGN_PFX`), when it has one.

## 0.3.0 - 2026-10-28

### Fixed
- Windows: a started monitor showed only black. The display driver now hands demirror the
  frames Windows draws for the monitor (shared memory), instead of demirror reading the
  screen back with GDI, which stays black for such monitors (it's kept as a fallback).

### Added
- **Mode**: **Extend** (a new screen) or **Mirror** (show one of the host's screens), in
  the browser and in the app.
- **Text size**: Normal, Larger, Largest, for readable text on small screens.
- **Position** of the new screen beside the host's main one (Windows hosts).
- Touch, mouse and keyboard work on a mirrored screen on Windows too.
- Windows: the app writes a log (`%LOCALAPPDATA%\demirror\demirror.log`, shown in Settings).

## 0.2.1 - 2026-10-28

### Fixed
- Windows: installing the display driver failed with error 0xE0000219 ("no service"):
  the driver's INF now installs the UMDF reflector service it runs behind.
- Driver installation errors are shown with their code instead of an unreadable number.
- The build now checks the driver's INF with InfVerif.

## 0.2.0 - 2026-10-28

### Added
- Windows display driver: on Windows 10 and 11, devices now **extend** the desktop like
  real monitors (an Indirect Display driver, made-to-measure monitors, unplugged when the
  device disconnects or demirror quits).
- Touch, mouse and keyboard from a device control Windows too.
- Windows installer (`…-setup.exe`): programs, display driver and firewall rule, all
  removed again by uninstalling. The `.zip` can install the driver from the Share page.
- Windows programs carry the C runtime: nothing to install besides demirror.
- App icon; version details on the Windows programs.
- Every push is built and tested for Linux and Windows; releases come with `SHA256SUMS.txt`.
- License files (MIT or Apache-2.0).

## 0.1.0 - 2026-10-28

First release.

- Share a Linux desktop with phones, tablets and other computers: each becomes an extra
  monitor of its own screen size (VKMS virtual displays and `vmon-vdisplayd`).
- Browser viewer (no app needed on the device) and the demirror app, with touch,
  trackpad, mouse and keyboard control of the host.
- Windows: mirroring existing screens, and using a Windows PC as a monitor of another
  computer.

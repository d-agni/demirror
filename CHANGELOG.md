# Changelog

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

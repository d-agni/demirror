# Security

demirror lets devices on your network see, and if you allow it control, your desktop.
Please report security problems privately, through GitHub's
[private vulnerability reporting](https://github.com/d-agni/demirror/security/advisories/new),
not in a public issue.

Worth knowing:
- Anyone who can reach the port (7878 by default) can open monitors unless **Ask devices
  for a token** is on. Use it on networks you don't trust, and turn off **Let devices
  control this computer** when devices should only watch.
- Traffic is plain HTTP and WebSocket on the local network, not encrypted.
- On Windows the display driver is self-signed until demirror has a code-signing
  certificate: installing it trusts that build's certificate on the computer, whose private
  key was deleted when the build ended. See the README's *Windows display driver*.

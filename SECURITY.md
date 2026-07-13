# Security policy

Fozmo is pre-alpha software. Security reports are welcome, but this project
does not yet offer a bug bounty or a guaranteed response or remediation time.

## Supported versions

Security fixes are applied to the latest public pre-alpha and the current
default branch. Older snapshots and development builds are not supported.

| Version | Security updates |
| --- | --- |
| Latest public pre-alpha | Best effort |
| Current default branch | Best effort |
| Older builds | No |

## Report a vulnerability privately

Use **Security → Report a vulnerability** in this repository to open a private
report. Repository maintainers must enable GitHub private vulnerability
reporting before the public launch.

If that form is unavailable, open a public issue titled **Private security
contact requested** with no vulnerability details. A maintainer can then
establish a private channel. Do not include exploits, credentials, private
URLs, account data, logs, screenshots, IP addresses or device identifiers in
the public issue.

Include in the private report when possible:

- affected version or commit;
- affected platform and feature;
- prerequisites and minimal reproduction steps;
- expected impact and whether exploitation has been demonstrated;
- a sanitized proof of concept or log excerpt;
- any disclosure deadline that must be coordinated.

The maintainers aim to acknowledge a complete report within seven days. They
will validate the issue, coordinate a fix and release, and credit the reporter
unless anonymity is requested. Please allow a reasonable remediation window
before public disclosure.

## Security boundaries

Fresh macOS installs bind locally and LAN access is opt-in. If a user enables
LAN access without **Require LAN Authentication**, ordinary playback control by
other devices on that trusted network is intentional and is not by itself a
vulnerability. The TLS Remote Access listener always requires a linked remote
session. Credential-sensitive and administrative routes have separate
protections.

The AirPlay network implementation is a standalone GPL process. The MIT server
communicates with it over owner-only local IPC using opaque receiver IDs,
coarse metadata/control data and documented standard PCM. A report that crosses
this boundary should identify which process handles the untrusted input.

Examples of useful reports include authentication or authorization bypasses,
secret exposure, unsafe handling of uploaded files or fonts, path traversal,
remote code execution, cryptographic flaws, cross-site scripting, and a way for
network input to violate the documented AirPlay process boundary.

Reports about service availability, unsupported receivers, audible playback
quality, expected pre-alpha crashes without a security consequence, or access
from an explicitly trusted unauthenticated LAN are normally compatibility or
bug reports rather than vulnerabilities.

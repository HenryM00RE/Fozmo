# Manual Smoke Evidence Template

Copy this file to `docs/manual-smoke-evidence.local.md` when collecting local
hardware, service, LAN, or screenshot evidence. The `.local.md` file is ignored
so private notes, device names, account details, LAN addresses, and screenshots
do not get committed by accident.

Keep committed summaries in [manual-smoke-tests.md](manual-smoke-tests.md)
sanitized and brief. Use this local file for raw notes.

## Entry Template

```text
Date:
Commit:
Tester:
Machine:
Area:
Command or action:
Device or service:
Result:
Notes:
Private artifacts:
```

## Sanitization Checklist

Before copying any result into the committed smoke-test matrix:

- Remove service tokens, pairing tokens, cookies, and account identifiers.
- Replace personal device names with generic labels such as `External DAC` or
  `LAN agent`.
- Replace local music paths with generic paths such as `/music/example.flac`.
- Replace private LAN addresses with documentation addresses such as
  `192.168.1.x`.
- Remove screenshots of listening history, private albums, account pages, or
  local folder structures unless they are explicitly intended for release notes.
- Record only the date, commit, target category, command or action, result, and
  any sanitized caveat in the committed matrix.

## Screenshot Notes

Put private screenshots under `docs/screenshots/private/` or
`docs/screenshots/local/`. Those folders are ignored. Public screenshots should
only be committed after the screenshot readiness checklist in
[manual-smoke-tests.md](manual-smoke-tests.md) passes.

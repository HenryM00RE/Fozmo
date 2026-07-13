# Code Quality

This project treats quality checks as part of the implementation contract, not a cleanup step after the fact.

## Clippy Policy

- Global Clippy allows in `tools/clippy.sh` are temporary debt markers.
- Remove global allows one lint at a time, fix only the lint exposed by that removal, then run `cargo fmt`, `./tools/clippy.sh --all-targets`, and `cargo test`.
- Prefer code changes or named types over new allows.
- When an allow is still clearer than refactoring, place it directly on the smallest function or item and add a short reason comment above it.
- Avoid reintroducing `clippy::too_many_arguments` and `clippy::type_complexity` globally. Use grouped input structs, named type aliases, or narrow local allows instead.

## Frontend API Policy

- Keep public frontend import paths stable during internal refactors.
- `ui/src/shared/lib/api.ts` is the compatibility facade for existing consumers.
- Internal API implementation files can move under `ui/src/shared/lib/api/` as long as the facade continues to export the same public names.

## Verification

Run `./tools/verify.sh` before treating a code-quality cleanup as complete. If a verification failure is environment-specific, record the exact failing command and summary in the change or pull-request notes.

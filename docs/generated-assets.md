# Generated Assets

The React frontend source lives in `ui/`. The production build writes assets to
`static/react-app`, and the Rust server serves that directory in core mode.

## Current Policy

`static/react-app` remains committed for now as the built frontend snapshot that
the Rust app can serve from a checkout. Treat it as generated output:

- Update it only with `npm --prefix ui run build`.
- Do not hand-edit files under `static/react-app`.
- Generated bundle diffs are marked as generated in GitHub via `.gitattributes`.
- Run `./tools/check-frontend-snapshot.sh` after frontend source changes that
  refresh the build.
- `./tools/verify.sh` and `./tools/public-readiness.sh` fail when the committed
  snapshot is stale.
- Revisit this policy before a public release if packaging grows a separate
  frontend build step.

## Freshness Check

Use the dedicated snapshot check before committing frontend work:

```sh
./tools/check-frontend-snapshot.sh
```

The check runs the Vite production build, then fails if `static/react-app` has
changed during that build. If the check fails, review the generated asset
changes and rerun it. If another command already ran the frontend build, use:

```sh
./tools/check-frontend-snapshot.sh --no-build
```

Release/public-readiness gates additionally require the generated snapshot to
be clean in Git:

```sh
./tools/check-frontend-snapshot.sh --no-build --require-clean
```

## Release Check

Before publishing a release branch or artifact:

- Confirm `./tools/check-frontend-snapshot.sh` passes.
- Confirm source maps are intentional for the release shape.
- Confirm no local paths, tokens, private LAN details, or development-only
  assets are embedded in the generated bundle.
- Review [packaging.md](packaging.md) before deciding whether generated source
  maps ship in a public artifact.

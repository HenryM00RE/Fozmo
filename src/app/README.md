# App

Application startup, shared process state, path resolution, and static asset
wiring live here.

- `runtime.rs`: core/agent mode selection and top-level startup orchestration.
- `config.rs`: environment and CLI parsing.
- `bootstrap.rs`: shared service and `AppState` construction.
- `server.rs`: Axum router assembly, static assets, TCP binding, and LAN
  advertisement.
- `auth.rs`: pairing middleware for protected routes.

Keep product behavior in the feature modules that own it.

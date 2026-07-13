import { Component, type ErrorInfo, type ReactNode } from 'react';

type AppErrorBoundaryProps = {
  children: ReactNode;
  onError?: (error: Error, errorInfo: ErrorInfo) => void;
  onReload?: () => void;
};

type AppErrorBoundaryState = {
  failed: boolean;
};

export class AppErrorBoundary extends Component<AppErrorBoundaryProps, AppErrorBoundaryState> {
  state: AppErrorBoundaryState = { failed: false };

  static getDerivedStateFromError(): AppErrorBoundaryState {
    return { failed: true };
  }

  componentDidCatch(error: Error, errorInfo: ErrorInfo) {
    if (this.props.onError) {
      this.props.onError(error, errorInfo);
      return;
    }
    console.error('Fozmo interface render failed', error, errorInfo.componentStack);
  }

  render() {
    if (!this.state.failed) return this.props.children;

    return (
      <main className="react-app remote-auth-page">
        <section className="remote-auth-panel" role="alert" aria-live="assertive">
          <div className="remote-auth-kicker">Interface Error</div>
          <h1>Something went wrong</h1>
          <p>Fozmo hit an unexpected interface error. Reload the app to return to a clean state.</p>
          <div className="remote-auth-actions">
            <button
              className="pill primary"
              type="button"
              onClick={this.props.onReload ?? (() => window.location.reload())}
            >
              Reload Fozmo
            </button>
          </div>
        </section>
      </main>
    );
  }
}

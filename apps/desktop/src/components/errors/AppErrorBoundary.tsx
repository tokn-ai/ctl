import { Component, type ErrorInfo, type ReactNode } from "react";

interface AppErrorBoundaryProps {
  children: ReactNode;
}

interface AppErrorBoundaryState {
  error: Error | null;
}

export class AppErrorBoundary extends Component<
  AppErrorBoundaryProps,
  AppErrorBoundaryState
> {
  state: AppErrorBoundaryState = { error: null };

  static getDerivedStateFromError(error: Error): AppErrorBoundaryState {
    return { error };
  }

  componentDidCatch(error: Error, errorInfo: ErrorInfo) {
    console.error("Unhandled rmux UI error", error, errorInfo.componentStack);
  }

  render() {
    if (this.state.error) {
      return <AppCrashFallback error={this.state.error} />;
    }
    return this.props.children;
  }
}

export function AppCrashFallback({ error }: { error: Error }) {
  return (
    <main className="app-crash" role="alert">
      <span className="app-crash-product">rmux</span>
      <h1>The interface stopped unexpectedly.</h1>
      <p>
        Your rmux sessions are still running. Reload the window to reconnect.
      </p>
      <code>{error.message || error.name}</code>
      <button type="button" onClick={() => window.location.reload()}>
        Reload rmux
      </button>
    </main>
  );
}

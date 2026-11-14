import { Component, type ErrorInfo, type ReactNode } from "react";
import { useLocale } from "../../features/i18n/LocaleContext";

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
    console.error("Unhandled ctmux UI error", error, errorInfo.componentStack);
  }

  render() {
    if (this.state.error) {
      return <AppCrashFallback error={this.state.error} />;
    }
    return this.props.children;
  }
}

export function AppCrashFallback({ error }: { error: Error }) {
  const { t } = useLocale();
  return (
    <main className="app-crash" role="alert">
      <span className="app-crash-product">ctmux</span>
      <h1>{t("crash.heading")}</h1>
      <p>
        {t("crash.detail")}</p>
      <code>{error.message || error.name}</code>
      <button type="button" onClick={() => window.location.reload()}>
        {t("crash.reload")}</button>
    </main>
  );
}

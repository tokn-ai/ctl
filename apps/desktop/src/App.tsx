import "./App.css";
import { TerminalPage } from "./pages/TerminalPage";
import { FilePreviewProvider, PreviewWindowPage } from "./features/files/FilePreview";
import { AppErrorBoundary } from "./components/errors/AppErrorBoundary";
import { LocaleProvider } from "./features/i18n/LocaleContext";

function App() {
  const preview = new URLSearchParams(window.location.search).has("preview_id");
  return <LocaleProvider><AppErrorBoundary>
    {preview ? <PreviewWindowPage /> : <FilePreviewProvider><TerminalPage /></FilePreviewProvider>}
  </AppErrorBoundary></LocaleProvider>;
}

export default App;

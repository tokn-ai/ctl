import "./App.css";
import { TerminalPage } from "./pages/TerminalPage";
import { FilePreviewProvider, PreviewWindowPage } from "./features/files/FilePreview";

function App() {
  if (new URLSearchParams(window.location.search).has("preview_id")) return <PreviewWindowPage />;
  return <FilePreviewProvider><TerminalPage /></FilePreviewProvider>;
}

export default App;

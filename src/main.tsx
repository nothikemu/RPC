import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import { createTauriBackend, isTauri, type Backend } from "./lib/backend";
import { PreviewBackend } from "./lib/previewBackend";
import { StoreProvider } from "./state/store";
import "./styles/tokens.css";
import "./styles/app.css";

async function start() {
  // Inside DeskPresence the Rust backend is used. A plain browser (npm run dev)
  // gets the clearly labelled preview backend with sample data.
  const backend: Backend = isTauri() ? await createTauriBackend() : new PreviewBackend();
  const root = document.getElementById("root");
  if (!root) return;
  createRoot(root).render(
    <StrictMode>
      <StoreProvider backend={backend}>
        <App />
      </StoreProvider>
    </StrictMode>,
  );
}

void start();

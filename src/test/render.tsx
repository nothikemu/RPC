import { render } from "@testing-library/react";
import type { ReactElement } from "react";
import { PreviewBackend } from "../lib/previewBackend";
import type { Config } from "../lib/types";
import { StoreProvider } from "../state/store";

export function renderWithStore(ui: ReactElement, initial?: Partial<Config>) {
  const backend = new PreviewBackend(initial);
  const result = render(<StoreProvider backend={backend}>{ui}</StoreProvider>);
  return { backend, ...result };
}

import { fireEvent, screen, waitFor } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { SaveBar } from "../components/SaveBar";
import { useStore } from "../state/store";
import { renderWithStore } from "../test/render";
import { SettingsPage } from "./SettingsPage";

function Page() {
  const { draft } = useStore();
  if (!draft) return null;
  return (
    <>
      <SettingsPage onShowOnboarding={() => undefined} />
      <SaveBar />
    </>
  );
}

describe("Settings", () => {
  it("validates the application ID before saving and saves valid changes", async () => {
    const { backend } = renderWithStore(<Page />, { ui: { theme: "system", onboarding_completed: true, tray_hint_dismissed: true } });
    const input = await screen.findByLabelText("Discord Application ID");

    fireEvent.change(input, { target: { value: "12ab" } });
    expect(await screen.findByText(/digits only/)).toBeTruthy();

    fireEvent.change(input, { target: { value: "123456789012345678" } });
    await waitFor(() => expect(screen.queryByText(/digits only/)).toBeNull());
    fireEvent.click(screen.getByRole("button", { name: "Save changes" }));

    await waitFor(() => expect(backend.calls.some((c) => c.method === "saveConfig")).toBe(true));
    const saved = backend.calls.find((c) => c.method === "saveConfig")?.args[0] as { discord: { application_id: string } };
    expect(saved.discord.application_id).toBe("123456789012345678");
  });

  it("surfaces backend validation errors for invalid IDs", async () => {
    const { backend } = renderWithStore(<Page />);
    const input = await screen.findByLabelText("Discord Application ID");
    fireEvent.change(input, { target: { value: "1234" } });
    fireEvent.click(screen.getByRole("button", { name: "Save changes" }));
    await waitFor(() => expect(screen.getAllByText(/17 to 20/).length).toBeGreaterThan(0));
    expect(backend.calls.filter((c) => c.method === "saveConfig")).toHaveLength(1);
    expect((await backend.getConfig()).config.discord.application_id).toBeNull();
  });

  it("toggles privacy and can discard changes", async () => {
    renderWithStore(<Page />);
    const toggle = await screen.findByRole("switch", { name: "Allow window titles in presence text" });
    expect(toggle.getAttribute("aria-checked")).toBe("false");
    fireEvent.click(toggle);
    expect(toggle.getAttribute("aria-checked")).toBe("true");
    fireEvent.click(screen.getByRole("button", { name: "Discard" }));
    await waitFor(() => expect(screen.getByRole("switch", { name: "Allow window titles in presence text" }).getAttribute("aria-checked")).toBe("false"));
  });

  it("asks for confirmation before resetting", async () => {
    const { backend } = renderWithStore(<Page />);
    fireEvent.click(await screen.findByRole("button", { name: "Reset settings…" }));
    expect(screen.getByRole("dialog", { name: "Reset all settings?" })).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Keep my settings" }));
    expect(backend.calls.some((c) => c.method === "resetConfig")).toBe(false);

    fireEvent.click(screen.getByRole("button", { name: "Reset settings…" }));
    fireEvent.click(screen.getByRole("button", { name: "Reset settings" }));
    await waitFor(() => expect(backend.calls.some((c) => c.method === "resetConfig")).toBe(true));
  });
});

import { fireEvent, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { useStore } from "../state/store";
import { renderWithStore } from "../test/render";
import { Onboarding } from "./Onboarding";

function Harness({ onDone }: { onDone: () => void }) {
  const { view } = useStore();
  return view ? <Onboarding onDone={onDone} /> : null;
}

describe("Onboarding", () => {
  it("walks through setup, validates the ID syntax and saves choices", async () => {
    const onDone = vi.fn();
    const { backend } = renderWithStore(<Harness onDone={onDone} />);

    expect(await screen.findByText("Show what you're working on, automatically")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: /Continue/ }));
    expect(screen.getByText("It lives in the system tray")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: /Continue/ }));

    const input = screen.getByLabelText("Application ID");
    fireEvent.change(input, { target: { value: "not-an-id" } });
    fireEvent.click(screen.getByRole("button", { name: /Continue/ }));
    expect(screen.getByText(/digits only/)).toBeTruthy();
    expect(screen.getByText("Connect a Discord application")).toBeTruthy();

    fireEvent.change(input, { target: { value: "123456789012345678" } });
    expect(screen.getByText(/format looks right/)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: /Continue/ }));

    fireEvent.click(screen.getByRole("switch", { name: "Launch DeskPresence at Windows startup" }));
    fireEvent.click(screen.getByRole("button", { name: /Continue/ }));
    expect(screen.getByText("Your privacy")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Finish setup" }));

    await waitFor(() => expect(screen.getByText("You're all set")).toBeTruthy());
    const saved = (await backend.getConfig()).config;
    expect(saved.ui.onboarding_completed).toBe(true);
    expect(saved.discord.application_id).toBe("123456789012345678");
    expect(saved.startup.launch_at_login).toBe(true);
    expect(saved.privacy.capture_window_titles).toBe(false);

    fireEvent.click(screen.getByRole("button", { name: "Open DeskPresence" }));
    expect(onDone).toHaveBeenCalled();
  });

  it("can be completed without Discord or an Application ID", async () => {
    const { backend } = renderWithStore(<Harness onDone={() => undefined} />);
    await screen.findByText("Show what you're working on, automatically");
    fireEvent.click(screen.getByRole("button", { name: /Continue/ }));
    fireEvent.click(screen.getByRole("button", { name: /Continue/ }));
    fireEvent.click(screen.getByRole("button", { name: "Skip for now" }));
    fireEvent.click(screen.getByRole("button", { name: /Continue/ }));
    fireEvent.click(screen.getByRole("button", { name: "Finish setup" }));
    await waitFor(() => expect(screen.getByText(/previews your presence/)).toBeTruthy());
    expect((await backend.getConfig()).config.discord.application_id).toBeNull();
  });
});

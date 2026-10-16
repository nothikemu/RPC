import { fireEvent, render, screen } from "@testing-library/react";
import { useState } from "react";
import { describe, expect, it } from "vitest";
import { TemplateInput } from "./TemplateInput";

function Harness({ initial = "" }: { initial?: string }) {
  const [value, setValue] = useState(initial);
  return <TemplateInput label="Details" value={value} onChange={setValue} windowTitlesEnabled={false} renderedLength={value.length} />;
}

describe("TemplateInput", () => {
  it("inserts variables at the cursor", () => {
    render(<Harness initial="Using " />);
    const input = screen.getByLabelText("Details") as HTMLInputElement;
    input.setSelectionRange(6, 6);
    fireEvent.click(screen.getByRole("button", { name: "{app}" }));
    expect(input.value).toBe("Using {app}");
  });

  it("shows unknown-variable errors inline", () => {
    render(<Harness initial="Hi {name}" />);
    expect(screen.getByRole("alert").textContent).toContain("Unknown variable {name}");
    expect(screen.getByLabelText("Details").getAttribute("aria-invalid")).toBe("true");
  });

  it("warns when text exceeds Discord's limit", () => {
    render(<Harness initial={"x".repeat(130)} />);
    expect(screen.getByText(/128-character limit/)).toBeTruthy();
  });
});

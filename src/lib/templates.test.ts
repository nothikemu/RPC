import { describe, expect, it } from "vitest";
import { parseTemplate, renderTemplate, templateError, usesVariable } from "./templates";

const values = { app: "Zed", exe: "zed.exe", elapsed: "5m", window_title: "main.rs" };

describe("template parsing (mirrors the Rust parser)", () => {
  it("renders all variables and collapses whitespace", () => {
    expect(renderTemplate("Using  {app} ({exe}) {elapsed}\n{window_title}", values)).toBe("Using Zed (zed.exe) 5m main.rs");
  });

  it("supports escaped braces and tolerant variable names", () => {
    expect(renderTemplate("{{app}} = { App }", values)).toBe("{app} = Zed");
  });

  it("reports unknown variables with their character position", () => {
    const error = templateError("héllo {nope}");
    expect(error?.position).toBe(6);
    expect(error?.message).toContain("{nope}");
  });

  it("reports structural problems", () => {
    expect(templateError("abc {app")?.position).toBe(4);
    expect(templateError("x } y")?.position).toBe(2);
    expect(templateError("{ }")?.message).toContain("Empty");
    expect(templateError("a".repeat(513))?.message).toContain("longer");
    expect(templateError("Using {app}")).toBeNull();
  });

  it("detects variable usage", () => {
    expect(usesVariable("for {elapsed}", "elapsed")).toBe(true);
    expect(usesVariable("{app}", "elapsed")).toBe(false);
    expect(parseTemplate("").segments).toEqual([]);
  });
});

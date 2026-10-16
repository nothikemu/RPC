import { describe, expect, it } from "vitest";
import { applicationIdError, assetValueError, clamp, isValidApplicationId, matcherError } from "./validation";

describe("application ID validation", () => {
  it("accepts 17–20 digit snowflakes", () => {
    expect(isValidApplicationId("123456789012345678")).toBe(true);
    expect(isValidApplicationId("12345678901234567")).toBe(true);
  });

  it("rejects malformed IDs with helpful messages", () => {
    expect(applicationIdError("")).toBeNull();
    expect(applicationIdError("abc")).toMatch(/digits only/);
    expect(applicationIdError("1234")).toMatch(/17 to 20/);
    expect(applicationIdError("99999999999999999999")).toMatch(/out of range/);
    expect(isValidApplicationId("00000000000000000")).toBe(false);
  });
});

describe("asset and matcher validation", () => {
  it("accepts asset keys and https URLs only", () => {
    expect(assetValueError("my_logo")).toBeNull();
    expect(assetValueError("https://example.com/a.png")).toBeNull();
    expect(assetValueError("http://example.com/a.png")).toMatch(/https/);
    expect(assetValueError("has space")).toMatch(/spaces/);
  });

  it("validates matchers by kind", () => {
    expect(matcherError("exe_name", "zed.exe")).toBeNull();
    expect(matcherError("exe_name", "C:\\zed.exe")).toMatch(/path/);
    expect(matcherError("path", "zed.exe")).toMatch(/full path/);
    expect(matcherError("path", "C:\\Apps\\zed.exe")).toBeNull();
    expect(matcherError("pattern", "**")).toMatch(/everything/);
    expect(matcherError("pattern", "  ")).toMatch(/Enter/);
  });

  it("clamps numbers", () => {
    expect(clamp(5, 0, 3)).toBe(3);
    expect(clamp(Number.NaN, 1, 3)).toBe(1);
  });
});

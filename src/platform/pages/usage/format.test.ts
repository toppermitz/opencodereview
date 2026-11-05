import { describe, expect, it } from "vitest";
import type { UsageGroupRow } from "../../../types";
import { attributedRows, compactTokens, dayAxisLabel, dayLabel, isProvider, pct, usageError } from "./format";

describe("usage formatting", () => {
  it.each([
    [0, "0"],
    [887, "887"],
    [1000, "1K"],
    [887_300, "887.3K"],
    [999_960, "1M"],
    [1_900_000, "1.9M"],
    [12_420_000, "12.42M"],
    [1_234_000_000, "1.23B"],
    [-5, "0"],
  ])("compacts %d tokens to %s", (tokens, expected) => {
    expect(compactTokens(tokens)).toBe(expected);
  });

  it("formats shares and guards an empty whole", () => {
    expect(pct(186, 1000, 1)).toBe("18.6%");
    expect(pct(150, 1000)).toBe("15%");
    expect(pct(1, 3)).toBe("33%");
    expect(pct(5, 0)).toBe("0%");
  });

  it("labels local days for the axis and the tooltip", () => {
    expect(dayAxisLabel("2026-10-25")).toBe("OUT 25");
    expect(dayAxisLabel("2026-01-05")).toBe("JAN 5");
    expect(dayLabel("2026-10-25")).toBe("Out 25, 2026");
    expect(dayAxisLabel("not a day")).toBe("not a day");
  });

  it("keeps only attributed rows and known providers", () => {
    const row = (key: string) => ({ key }) as UsageGroupRow;
    expect(attributedRows([row("claude"), row("unattributed"), row("codex")]).map((item) => item.key)).toEqual([
      "claude",
      "codex",
    ]);
    expect(isProvider("codex")).toBe(true);
    expect(isProvider("gemini")).toBe(false);
    expect(isProvider("other")).toBe(false);
    expect(isProvider(null)).toBe(false);
  });

  it("turns a missing route into Docker guidance", () => {
    expect(usageError(new Error("route not found: GET /usage"))).toContain("docker compose up --build server");
    expect(usageError(new Error("database is busy"))).toBe("database is busy");
  });
});

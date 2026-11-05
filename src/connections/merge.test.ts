import { describe, expect, it } from "vitest";
import { merge3 } from "./merge";

const base = "# Title\n\nFirst paragraph here.\n\nSecond paragraph here.\n\nThird paragraph here.";

describe("merge3", () => {
  it("takes the remote text when there are no local edits", () => {
    const remote = base.replace("Second", "2nd");
    expect(merge3(base, base, remote)).toEqual({ text: remote, conflict: false });
  });

  it("keeps the local text when there are no remote edits", () => {
    const local = base.replace("Third", "3rd");
    expect(merge3(base, local, base)).toEqual({ text: local, conflict: false });
  });

  it("merges edits in different paragraphs", () => {
    const local = base.replace("First", "1st");
    const remote = base.replace("Third", "3rd");
    expect(merge3(base, local, remote)).toEqual({
      text: base.replace("First", "1st").replace("Third", "3rd"),
      conflict: false,
    });
  });

  it("merges edits to different words in the same paragraph", () => {
    const b = "# T\n\nThe quick brown fox jumps over the lazy dog every single morning.";
    const local = b.replace("quick", "slow");
    const remote = b.replace("morning", "evening");
    expect(merge3(b, local, remote).text).toBe(
      "# T\n\nThe slow brown fox jumps over the lazy dog every single evening.",
    );
  });

  it("does not duplicate a change that both sides made", () => {
    const local = base.replace("Second paragraph here.", "Second paragraph here. Added.");
    expect(merge3(base, local, local)).toEqual({ text: local, conflict: false });
  });

  it("keeps the local words and reports a conflict when both edit the same words", () => {
    const local = base.replace("Second paragraph here.", "Mine.");
    const remote = base.replace("Second paragraph here.", "Theirs.");
    const result = merge3(base, local, remote);
    expect(result.conflict).toBe(true);
    expect(result.text).toContain("Mine.");
    expect(result.text).not.toContain("Theirs.");
  });
});

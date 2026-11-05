import { describe, expect, it } from "vitest";
import { createTestCodec } from "../testCodec";
import { createOutlineDialect } from "./dialect";
import { computePatches } from "./patch";

const codec = createTestCodec(createOutlineDialect((src) => src));

// Outline writes bullets with `*`; itch writes them with `-`.
const base = "Intro text.\n\n* one\n* two\n\n\n:::info\nNote\n\n:::\n\nOutro text.";
const itchBase = codec.serialize(codec.parse(base));

describe("computePatches", () => {
  it("returns no patches when nothing changed", () => {
    expect(computePatches(codec, base, itchBase)).toEqual([]);
  });

  it("patches only the changed block, with Outline's original text as findText", () => {
    const next = itchBase.replace("Outro text.", "Outro text, edited.");
    expect(computePatches(codec, base, next)).toEqual([
      { findText: "Outro text.", text: "Outro text, edited." },
    ]);
  });

  it("uses the original text of a changed block that itch writes differently", () => {
    const next = itchBase.replace("- two", "- two and more");
    expect(computePatches(codec, base, next)).toEqual([
      { findText: "* one\n* two", text: "- one\n- two and more" },
    ]);
  });

  it("anchors an inserted block to the block before it", () => {
    const next = itchBase.replace("Intro text.", "Intro text.\n\nNew block.");
    expect(computePatches(codec, base, next)).toEqual([
      { findText: "Intro text.", text: "Intro text.\n\nNew block." },
    ]);
  });

  it("anchors a block inserted at the start to the first block", () => {
    const next = `First.\n\n${itchBase}`;
    expect(computePatches(codec, base, next)).toEqual([
      { findText: "Intro text.", text: "First.\n\nIntro text." },
    ]);
  });

  it("removes a deleted block together with the block before it, so no empty line stays", () => {
    const next = itchBase.replace("- one\n- two\n\n", "");
    expect(computePatches(codec, base, next)).toEqual([
      { findText: "Intro text.\n\n* one\n* two", text: "Intro text." },
    ]);
  });

  it("removes a deleted first block together with the block after it", () => {
    const next = itchBase.replace("Intro text.\n\n", "");
    expect(computePatches(codec, base, next)).toEqual([
      { findText: "Intro text.\n\n* one\n* two", text: "* one\n* two" },
    ]);
  });

  it("replaces several changed blocks with one patch", () => {
    const next = itchBase.replace("Intro text.\n\n- one\n- two", "New intro.");
    expect(computePatches(codec, base, next)).toEqual([
      { findText: "Intro text.\n\n* one\n* two", text: "New intro." },
    ]);
  });

  it("gives up when a patch would span an empty paragraph", () => {
    const withGap = "A.\n\n\n\nB.\n\nC.";
    expect(computePatches(codec, withGap, "C.")).toBeNull();
  });

  it("gives up when the changed block text is not the first match", () => {
    const twice = "Same.\n\nMiddle.\n\nSame.";
    const next = "Same.\n\nMiddle.\n\nChanged.";
    expect(computePatches(codec, twice, next)).toBeNull();
  });

  it("patches the first of two equal blocks", () => {
    const twice = "Same.\n\nMiddle.\n\nSame.";
    const next = "Changed.\n\nMiddle.\n\nSame.";
    expect(computePatches(codec, twice, next)).toEqual([{ findText: "Same.", text: "Changed." }]);
  });
});

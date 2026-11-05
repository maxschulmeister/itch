import { describe, expect, it } from "vitest";
import type { JSONContent } from "@tiptap/core";
import { createTestCodec } from "../testCodec";
import { createOutlineDialect } from "./dialect";
import kitchenSink from "./__fixtures__/kitchen-sink.md?raw";


const dialect = createOutlineDialect((src) => `resolved:${src}`);
const codec = createTestCodec(dialect);
const roundTrip = (md: string) => codec.serialize(codec.parse(md)).trim();

function findNodes(json: JSONContent, type: string): JSONContent[] {
  const found = json.type === type ? [json] : [];
  return found.concat(...(json.content ?? []).map((child) => findNodes(child, type)));
}

function hasMark(json: JSONContent, mark: string): boolean {
  if (json.marks?.some((m) => m.type === mark)) return true;
  return (json.content ?? []).some((child) => hasMark(child, mark));
}

describe("Outline dialect", () => {
  it("reads Outline-only syntax into editor nodes and marks", () => {
    const doc = codec.parse(kitchenSink);
    expect(findNodes(doc, "outlineNotice").map((n) => n.attrs?.style)).toEqual([
      "info",
      "warning",
      "tip",
    ]);
    expect(findNodes(doc, "outlineToggle")).toHaveLength(1);
    expect(findNodes(doc, "hardBreak")).toHaveLength(1);
    expect(findNodes(doc, "codeBlock")).toHaveLength(1);
    expect(findNodes(doc, "outlineInlineMath")[0]?.attrs?.latex).toBe("E=mc^2");
    expect(doc.content?.[doc.content.length - 1]?.content?.[0]?.text).toBe("Last paragraph.");
    expect(hasMark(doc, "underline")).toBe(true);
    expect(hasMark(doc, "highlight")).toBe(true);
    expect(findNodes(doc, "image")[0]?.attrs?.src).toBe(
      "/api/attachments.redirect?id=00000000-0000-4000-8000-000000000001",
    );
  });

  it("writes Outline syntax back", () => {
    expect(roundTrip("a __b__ c")).toBe("a __b__ c");
    expect(roundTrip("a **b** c")).toBe("a **b** c");
    expect(roundTrip("a ==b== c")).toBe("a ==b== c");
    expect(roundTrip("line\\nbreak")).toBe("line\\nbreak");
    expect(roundTrip(":::warning\nCareful\n\n:::")).toBe(":::warning\nCareful\n:::");
    expect(roundTrip("+++++\nHidden\n\n+++++")).toBe("+++++\nHidden\n+++++");
    expect(roundTrip("@[Max](mention://a/user/b) hi")).toBe("@[Max](mention://a/user/b) hi");
  });

  it("escapes text the same way as Outline", () => {
    expect(roundTrip("type \\*\\*bold\\*\\* now")).toBe("type \\*\\*bold\\*\\* now");
    expect(roundTrip("snake_case and a\\~b")).toBe("snake_case and a\\~b");
    expect(roundTrip("\\# not a heading")).toBe("\\# not a heading");
    expect(roundTrip("use `a*b` here")).toBe("use `a*b` here");
  });

  it("is stable after one round trip", () => {
    const once = roundTrip(kitchenSink);
    expect(roundTrip(once)).toBe(once);
  });

  it("resolves image sources only for display", () => {
    expect(dialect.resolveImageSrc("/api/x")).toBe("resolved:/api/x");
  });
});

// @vitest-environment node
/**
 * Contract test against a real Outline workspace. Runs only when
 * OUTLINE_TEST_URL and OUTLINE_TEST_API_KEY are set. Use a test workspace:
 * the test creates and deletes documents.
 */
import { afterAll, beforeAll, describe, expect, it, vi } from "vitest";
import { createTestCodec } from "../testCodec";
import type { ConnectionInfo, RemoteDocument } from "../types";

const BASE_URL = process.env.OUTLINE_TEST_URL ?? "";
const API_KEY = process.env.OUTLINE_TEST_API_KEY ?? "";

vi.mock("@tauri-apps/api/core", () => ({
  convertFileSrc: (path: string, protocol: string) =>
    `${protocol}://localhost/${encodeURIComponent(path)}`,
  invoke: async (command: string, args: { path: string; body: unknown }) => {
    if (command !== "connection_request") throw new Error(`Unexpected command ${command}`);
    const response = await fetch(`${BASE_URL}${args.path}`, {
      method: "POST",
      headers: { authorization: `Bearer ${API_KEY}`, "content-type": "application/json" },
      body: JSON.stringify(args.body),
    });
    const retryAfter = response.headers.get("retry-after");
    return {
      status: response.status,
      body: await response.json().catch(() => null),
      retryAfter: retryAfter ? Number(retryAfter) : null,
    };
  },
}));

const { outlineConnector: outline } = await import("./index");

const conn: ConnectionInfo = { id: "test", kind: "outline", name: "Test", baseUrl: BASE_URL };
const codec = createTestCodec(outline.dialect(conn));

describe.skipIf(!BASE_URL || !API_KEY)("Outline connector (live)", () => {
  let spaceId = "";
  let doc: RemoteDocument;
  const created: string[] = [];

  beforeAll(async () => {
    const spaces = await outline.listSpaces(conn);
    spaceId = spaces[0].id;
    doc = await outline.create(conn, { spaceId, parentId: null }, "itch contract test");
    created.push(doc.id);
    const result = await outline.write(
      conn,
      doc,
      {
        title: doc.title,
        text: "First paragraph.\n\n* one\n* two\n\n:::info\nA notice\n:::\n\nLast paragraph.",
      },
      codec,
    );
    if (result.status !== "saved") throw new Error("Setup write failed");
    doc = result.doc;
  }, 30_000);

  afterAll(async () => {
    for (const id of created) await outline.remove(conn, id).catch(() => {});
  });

  it("verifies the token and lists the tree", async () => {
    expect(await outline.verify(conn)).toBeTruthy();
    const tree = await outline.listDocuments(conn, spaceId);
    expect(tree.some((node) => node.id === doc.id)).toBe(true);
  });

  it("changes only the edited block", async () => {
    const before = await outline.read(conn, doc.id);
    const next = codec
      .serialize(codec.parse(before.text))
      .replace("Last paragraph.", "Last paragraph, edited.");
    const result = await outline.write(conn, before, { title: before.title, text: next }, codec);
    expect(result.status).toBe("saved");
    const after = await outline.read(conn, doc.id);
    expect(after.text).toBe(before.text.replace("Last paragraph.", "Last paragraph, edited."));
    doc = after;
  });

  it("inserts and deletes blocks", async () => {
    const before = await outline.read(conn, doc.id);
    const itch = codec.serialize(codec.parse(before.text));
    let result = await outline.write(
      conn,
      before,
      { title: before.title, text: itch.replace("First paragraph.", "First paragraph.\n\nInserted.") },
      codec,
    );
    expect(result.status).toBe("saved");
    let after = await outline.read(conn, doc.id);
    expect(after.text).toContain("First paragraph.\n\nInserted.");

    result = await outline.write(
      conn,
      after,
      { title: after.title, text: codec.serialize(codec.parse(after.text)).replace("Inserted.\n\n", "") },
      codec,
    );
    expect(result.status).toBe("saved");
    after = await outline.read(conn, doc.id);
    expect(after.text).not.toContain("Inserted.");
    expect(after.text).toBe(before.text);
    doc = after;
  });

  it("changes the title", async () => {
    const before = await outline.read(conn, doc.id);
    const result = await outline.write(
      conn,
      before,
      { title: "itch contract test (renamed)", text: before.text },
      codec,
    );
    expect(result.status).toBe("saved");
    expect((await outline.read(conn, doc.id)).title).toBe("itch contract test (renamed)");
  });

  it("reports a conflict when the document changed since the base revision", async () => {
    const stale = await outline.read(conn, doc.id);
    await outline.write(
      conn,
      stale,
      { title: stale.title, text: codec.serialize(codec.parse(stale.text)).replace("First", "1st") },
      codec,
    );
    const result = await outline.write(
      conn,
      stale,
      { title: stale.title, text: codec.serialize(codec.parse(stale.text)).replace("Last", "Final") },
      codec,
    );
    expect(result.status).toBe("conflict");
    if (result.status === "conflict") expect(result.remote.text).toContain("1st");
  });

  it("creates, moves, and searches documents", async () => {
    const child = await outline.create(conn, { spaceId, parentId: doc.id }, "itch child");
    created.unshift(child.id);
    let tree = await outline.listDocuments(conn, spaceId);
    expect(tree.find((n) => n.id === doc.id)?.children.map((c) => c.id)).toContain(child.id);

    await outline.move(conn, child.id, { spaceId, parentId: null });
    tree = await outline.listDocuments(conn, spaceId);
    expect(tree.some((n) => n.id === child.id)).toBe(true);

    const hits = await outline.search(conn, "Getting Started");
    expect(Array.isArray(hits)).toBe(true);
  });
});

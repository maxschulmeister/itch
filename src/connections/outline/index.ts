import { ConnectionError, assetUrl, request, upload } from "../transport";
import type {
  ConnectionInfo,
  Connector,
  EditorDialect,
  RemoteDocument,
  TreeNode,
  WriteResult,
} from "../types";
import { createOutlineDialect } from "./dialect";
import { computePatches } from "./patch";

interface OutlineDocument {
  id: string;
  title: string;
  text?: string;
  revision: number;
  updatedAt: string;
  collectionId: string;
  parentDocumentId: string | null;
  url: string;
}

interface NavigationNode {
  id: string;
  title: string;
  icon?: string | null;
  children: NavigationNode[];
}

interface Collection {
  id: string;
  name: string;
  icon?: string | null;
  index: string;
}

async function call<T>(conn: ConnectionInfo, method: string, body: object): Promise<T> {
  const response = await request<{ data: T; message?: string; error?: string }>(
    conn,
    `/api/${method}`,
    body,
  );
  if (response.status >= 200 && response.status < 300) return response.body.data;
  const message =
    response.status === 401
      ? "The API key is not valid or was revoked."
      : (response.body?.message ?? response.body?.error ?? `Outline returned HTTP ${response.status}`);
  throw new ConnectionError(message, response.status);
}

const toRemote = (doc: OutlineDocument): RemoteDocument => ({
  id: doc.id,
  title: doc.title,
  text: doc.text ?? "",
  revision: doc.revision,
  updatedAt: Math.floor(Date.parse(doc.updatedAt) / 1000),
  spaceId: doc.collectionId,
  parentId: doc.parentDocumentId ?? null,
  url: doc.url,
});

const toTree = (node: NavigationNode): TreeNode => ({
  id: node.id,
  title: node.title,
  kind: "document",
  icon: node.icon ?? null,
  children: node.children.map(toTree),
});

const isConflict = (error: unknown) => error instanceof ConnectionError && error.status === 409;

const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

const dialects = new Map<string, EditorDialect>();

export const outlineConnector: Connector = {
  kind: "outline",
  label: "Outline",
  defaultBaseUrl: "https://app.getoutline.com",
  tokenHelp: "Create an API key in Outline under Settings → API & Apps.",

  async verify(conn) {
    const info = await call<{ team: { name: string } }>(conn, "auth.info", {});
    return info.team.name;
  },

  async listSpaces(conn) {
    const collections: Collection[] = [];
    for (let offset = 0; ; offset += 100) {
      const page = await call<Collection[]>(conn, "collections.list", { limit: 100, offset });
      collections.push(...page);
      if (page.length < 100) break;
    }
    return collections
      .sort((a, b) => (a.index < b.index ? -1 : a.index > b.index ? 1 : 0))
      .map((c) => ({ id: c.id, title: c.name, kind: "space", icon: c.icon ?? null, children: [] }));
  },

  async listDocuments(conn, spaceId) {
    const nodes = await call<NavigationNode[]>(conn, "collections.documents", { id: spaceId });
    return nodes.map(toTree);
  },

  async read(conn, docId) {
    return toRemote(await call<OutlineDocument>(conn, "documents.info", { id: docId }));
  },

  async write(conn, base, next, codec): Promise<WriteResult> {
    let doc = base;
    const update = async (body: object) => {
      doc = toRemote(
        await call<OutlineDocument>(conn, "documents.update", {
          id: doc.id,
          lastRevision: doc.revision,
          ...body,
        }),
      );
    };

    try {
      if (next.title !== doc.title) await update({ title: next.title });
      const patches = computePatches(codec, doc.text, next.text);
      if (patches === null) {
        await update({ text: next.text });
      } else {
        for (const patch of patches) {
          try {
            await update({ editMode: "patch", findText: patch.findText, text: patch.text });
          } catch (error) {
            // Outline did not find the block. Save the full text, so no edit is lost.
            if (!(error instanceof ConnectionError) || error.status !== 400) throw error;
            console.warn("Outline patch failed, replacing the full text:", error.message);
            await update({ text: next.text });
            break;
          }
        }
      }
    } catch (error) {
      if (!isConflict(error)) throw error;
      return { status: "conflict", base: doc, remote: await this.read(conn, doc.id) };
    }
    return { status: "saved", doc };
  },

  async create(conn, at, title) {
    const doc = await call<OutlineDocument>(conn, "documents.create", {
      title,
      text: "",
      collectionId: at.spaceId,
      ...(at.parentId ? { parentDocumentId: at.parentId } : {}),
      publish: true,
    });
    return toRemote(doc);
  },

  async rename(conn, docId, title) {
    await call(conn, "documents.update", { id: docId, title });
  },

  async move(conn, docId, to) {
    await call(conn, "documents.move", {
      id: docId,
      collectionId: to.spaceId,
      ...(to.parentId ? { parentDocumentId: to.parentId } : {}),
      ...(to.index !== undefined ? { index: to.index } : {}),
    });
  },

  async remove(conn, docId) {
    await call(conn, "documents.delete", { id: docId });
  },

  async search(conn, query) {
    const results = await call<{ context: string; document: OutlineDocument }[]>(
      conn,
      "documents.search",
      { query, limit: 25 },
    );
    return results.map((result) => ({
      docId: result.document.id,
      title: result.document.title,
      preview: result.context.replace(/<[^>]+>/g, ""),
    }));
  },

  async uploadImage(conn, docId, file, name) {
    const result = await call<{
      uploadUrl: string;
      form: Record<string, string>;
      attachment: { url: string };
    }>(conn, "attachments.create", {
      name,
      documentId: docId,
      contentType: file.type || "application/octet-stream",
      size: file.size,
      preset: "documentAttachment",
    });
    const status = await upload(conn, result.uploadUrl, result.form, file, name);
    if (status < 200 || status >= 300) {
      throw new ConnectionError(`Image upload failed (HTTP ${status})`, status);
    }
    return result.attachment.url;
  },

  documentFromLink(conn, href) {
    const path = href.startsWith(conn.baseUrl) ? href.slice(conn.baseUrl.length) : href;
    const match = /^\/doc\/([^/?#]+)/.exec(path);
    if (!match) return null;
    const slug = match[1];
    if (UUID.test(slug)) return slug;
    return /(?:^|-)([A-Za-z0-9]{10})$/.exec(slug)?.[1] ?? null;
  },

  webUrl(conn, doc) {
    return `${conn.baseUrl}${doc.url}`;
  },

  dialect(conn) {
    let dialect = dialects.get(conn.id);
    if (!dialect) {
      dialect = createOutlineDialect((src) =>
        src.startsWith("data:") ? src : assetUrl(conn, src),
      );
      dialects.set(conn.id, dialect);
    }
    return dialect;
  },
};

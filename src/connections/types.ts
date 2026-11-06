import type { Extensions, JSONContent } from "@tiptap/core";
import type { marked } from "marked";

/** A saved connection (from `{APP_DATA}/connections.json`). */
export interface ConnectionInfo {
  id: string;
  kind: string;
  name: string;
  baseUrl: string;
}

/** A node in the connection tree. A space has no content of its own. */
export interface TreeNode {
  id: string;
  title: string;
  kind: "space" | "document";
  icon?: string | null;
  children: TreeNode[];
}

export interface RemoteDocument {
  id: string;
  title: string;
  /** Markdown body, without the title. */
  text: string;
  revision: number;
  /** Last change, in seconds since 1970. */
  updatedAt: number;
  spaceId: string;
  parentId: string | null;
  url: string;
}

export type WriteResult =
  | { status: "saved"; doc: RemoteDocument }
  /**
   * Someone else changed the document. `base` is the last version this write
   * produced (or the original base), `remote` is the latest version.
   */
  | { status: "conflict"; base: RemoteDocument; remote: RemoteDocument };

export interface SearchHit {
  docId: string;
  title: string;
  preview: string;
}

/** Location for a new or moved document. */
export interface DocumentLocation {
  spaceId: string;
  parentId: string | null;
  index?: number;
}

/** Markdown parse and serialize functions of the editor that shows the document. */
export interface MarkdownCodec {
  parse(markdown: string): JSONContent;
  serialize(json: JSONContent): string;
  /** Top-level block tokens with their exact source text. */
  lex(markdown: string): { type: string; raw: string }[];
}

/** Editor changes needed to read and write the markdown of a connector. */
export interface EditorDialect {
  /** Marked instance for this dialect, so its syntax does not leak into other editors. */
  marked: typeof marked;
  /** Outline puts images inside paragraphs. */
  inlineImages: boolean;
  /** Options merged into StarterKit.configure(), for example to replace a mark. */
  starterKit: Record<string, unknown>;
  extensions: Extensions;
  /** Called on the document JSON before it becomes markdown. */
  prepareForSerialize(json: JSONContent): JSONContent;
  /** Display URL for an image source in the document. */
  resolveImageSrc(src: string): string;
}

export interface Connector {
  kind: string;
  label: string;
  defaultBaseUrl: string;
  tokenHelp: string;
  /** Returns the workspace name. */
  verify(conn: ConnectionInfo): Promise<string>;
  listSpaces(conn: ConnectionInfo): Promise<TreeNode[]>;
  listDocuments(conn: ConnectionInfo, spaceId: string): Promise<TreeNode[]>;
  read(conn: ConnectionInfo, docId: string): Promise<RemoteDocument>;
  write(
    conn: ConnectionInfo,
    base: RemoteDocument,
    next: { title: string; text: string },
    codec: MarkdownCodec,
  ): Promise<WriteResult>;
  create(conn: ConnectionInfo, at: DocumentLocation, title: string): Promise<RemoteDocument>;
  rename(conn: ConnectionInfo, docId: string, title: string): Promise<void>;
  move(conn: ConnectionInfo, docId: string, to: DocumentLocation): Promise<void>;
  remove(conn: ConnectionInfo, docId: string): Promise<void>;
  search(conn: ConnectionInfo, query: string): Promise<SearchHit[]>;
  /** Uploads an image and returns the source to put in the document. */
  uploadImage(conn: ConnectionInfo, docId: string, file: Blob, name: string): Promise<string>;
  /** Returns the document id or URL id for a link that points into the connection. */
  documentFromLink(conn: ConnectionInfo, href: string): string | null;
  /** URL of the document in the web app. */
  webUrl(conn: ConnectionInfo, doc: RemoteDocument): string;
  dialect(conn: ConnectionInfo): EditorDialect;
}

import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { toast } from "sonner";
import { useNotesActions, useNotesData } from "../context/NotesContext";
import type { PreviewModeData } from "../components/editor/Editor";
import { merge3 } from "./merge";
import { getConnector } from "./registry";
import * as transport from "./transport";
import type {
  ConnectionInfo,
  Connector,
  DocumentLocation,
  MarkdownCodec,
  RemoteDocument,
  SearchHit,
  TreeNode,
} from "./types";

const EXPANDED_KEY = "itch:connectionsExpanded";
const DOCUMENT_POLL_MS = 4000;
const TREE_POLL_MS = 45000;
const RETRY_MS = 5000;
const SEARCH_DEBOUNCE_MS = 300;

/** Tree keys: `c:<conn>`, `s:<conn>:<space>`, `d:<conn>:<doc>`. */
export const connectionKey = (connId: string) => `c:${connId}`;
export const spaceKey = (connId: string, spaceId: string) => `s:${connId}:${spaceId}`;
export const documentKey = (connId: string, docId: string) => `d:${connId}:${docId}`;

interface ActiveDocument {
  connId: string;
  docId: string;
  key: string;
  title: string;
  /** Markdown shown when the document opens. Null while it loads. */
  content: string | null;
  revision: number;
  updatedAt: number;
  reloadVersion: number;
  remoteUpdate?: { version: number; base: string; remote: string };
}

export interface ConnectionSearchResult {
  conn: ConnectionInfo;
  hit: SearchHit;
}

interface ConnectionsContextValue {
  connections: ConnectionInfo[];
  /** Spaces for each connection id. */
  spaces: Record<string, TreeNode[]>;
  /** Document trees for each space key. */
  documents: Record<string, TreeNode[]>;
  expanded: Set<string>;
  errors: Record<string, string>;
  active: ActiveDocument | null;
  /** Results for the sidebar search query from all connections. */
  searchResults: ConnectionSearchResult[];
  /** Document source for the editor, or null when a local note is shown. */
  externalDocument: PreviewModeData | null;
  toggle: (key: string) => void;
  openDocument: (connId: string, docRef: string) => Promise<void>;
  addConnection: (kind: string, baseUrl: string, token: string) => Promise<ConnectionInfo>;
  removeConnection: (id: string) => Promise<void>;
  refresh: (connId: string) => Promise<void>;
  createDocument: (connId: string, at: DocumentLocation) => Promise<void>;
  renameDocument: (connId: string, docId: string, title: string) => Promise<void>;
  moveDocument: (connId: string, docId: string, to: DocumentLocation) => Promise<void>;
  deleteDocument: (connId: string, docId: string) => Promise<void>;
  openInBrowser: (connId: string, docId: string) => Promise<void>;
}

const ConnectionsContext = createContext<ConnectionsContextValue | null>(null);

const errorMessage = (error: unknown) => (error instanceof Error ? error.message : String(error));

/** The editor shows a document as `# Title` and the body. */
function fullMarkdown(doc: RemoteDocument): string {
  const title = doc.title.replace(/[\\`*_[\]~<>#]/g, "\\$&");
  return `# ${title}\n\n${doc.text}`;
}

/** Split editor markdown into title (first H1) and body. */
function splitTitle(codec: MarkdownCodec, markdown: string, fallbackTitle: string) {
  const json = codec.parse(markdown);
  const [first, ...rest] = json.content ?? [];
  if (first?.type !== "heading" || first.attrs?.level !== 1) {
    return { title: fallbackTitle, text: markdown };
  }
  const title = (first.content ?? []).map((node) => node.text ?? "").join("").trim();
  return { title, text: codec.serialize({ type: "doc", content: rest }) };
}

function findPath(nodes: TreeNode[], id: string): TreeNode[] | null {
  for (const node of nodes) {
    if (node.id === id) return [node];
    const path = findPath(node.children, id);
    if (path) return [node, ...path];
  }
  return null;
}

function loadExpanded(): Set<string> {
  try {
    return new Set(JSON.parse(localStorage.getItem(EXPANDED_KEY) ?? "[]"));
  } catch {
    return new Set();
  }
}

export function ConnectionsProvider({ children }: { children: ReactNode }) {
  const { selectedNoteId, searchQuery } = useNotesData();
  const { clearSelection } = useNotesActions();

  const [connections, setConnections] = useState<ConnectionInfo[]>([]);
  const [spaces, setSpaces] = useState<Record<string, TreeNode[]>>({});
  const [documents, setDocuments] = useState<Record<string, TreeNode[]>>({});
  const [expanded, setExpanded] = useState<Set<string>>(loadExpanded);
  const [errors, setErrors] = useState<Record<string, string>>({});
  const [active, setActive] = useState<ActiveDocument | null>(null);

  const connectionsRef = useRef(connections);
  connectionsRef.current = connections;
  const documentsRef = useRef(documents);
  documentsRef.current = documents;
  const activeRef = useRef(active);
  activeRef.current = active;

  /** Last known server version of each opened document, by document key. */
  const bases = useRef(new Map<string, { conn: ConnectionInfo; doc: RemoteDocument }>());
  /** Markdown functions of the editor, by connection id. */
  const codecs = useRef(new Map<string, MarkdownCodec>());
  /** Edits that wait to be written, by document key. */
  const pendingWrites = useRef(new Map<string, string>());
  const writing = useRef(false);
  const remoteVersion = useRef(0);

  const lookup = useCallback((connId: string): [ConnectionInfo, Connector] => {
    const conn = connectionsRef.current.find((c) => c.id === connId);
    const connector = conn && getConnector(conn.kind);
    if (!conn || !connector) throw new Error("Connection not found");
    return [conn, connector];
  }, []);

  useEffect(() => {
    localStorage.setItem(EXPANDED_KEY, JSON.stringify([...expanded]));
  }, [expanded]);

  // A local note was selected: close the connection document
  useEffect(() => {
    if (selectedNoteId) setActive(null);
  }, [selectedNoteId]);

  const setError = useCallback((connId: string, error: string | null) => {
    setErrors((prev) => {
      if ((prev[connId] ?? null) === error) return prev;
      const next = { ...prev };
      if (error === null) delete next[connId];
      else next[connId] = error;
      return next;
    });
  }, []);

  const loadSpaces = useCallback(
    async (connId: string) => {
      try {
        const [conn, connector] = lookup(connId);
        const list = await connector.listSpaces(conn);
        setSpaces((prev) => ({ ...prev, [connId]: list }));
        setError(connId, null);
      } catch (error) {
        setError(connId, errorMessage(error));
      }
    },
    [lookup, setError],
  );

  const loadDocuments = useCallback(
    async (connId: string, spaceId: string) => {
      try {
        const [conn, connector] = lookup(connId);
        const tree = await connector.listDocuments(conn, spaceId);
        setDocuments((prev) => ({ ...prev, [spaceKey(connId, spaceId)]: tree }));
        setError(connId, null);
      } catch (error) {
        setError(connId, errorMessage(error));
      }
    },
    [lookup, setError],
  );

  /** Reload the trees that are open in the sidebar. */
  const refreshTrees = useCallback(
    async (onlyConnId?: string) => {
      const open = [...expanded];
      await Promise.all(
        connectionsRef.current
          .filter((conn) => !onlyConnId || conn.id === onlyConnId)
          .filter((conn) => open.includes(connectionKey(conn.id)))
          .map(async (conn) => {
            await loadSpaces(conn.id);
            const prefix = `s:${conn.id}:`;
            await Promise.all(
              open
                .filter((key) => key.startsWith(prefix))
                .map((key) => loadDocuments(conn.id, key.slice(prefix.length))),
            );
          }),
      );
    },
    [expanded, loadSpaces, loadDocuments],
  );

  const reloadConnections = useCallback(async () => {
    setConnections(await transport.listConnections());
  }, []);

  useEffect(() => {
    reloadConnections().catch((error) => console.error("Failed to load connections:", error));
  }, [reloadConnections]);

  // Load open trees when connections load, and then from time to time
  const refreshTreesRef = useRef(refreshTrees);
  refreshTreesRef.current = refreshTrees;
  useEffect(() => {
    if (connections.length === 0) return;
    void refreshTreesRef.current();
    const refreshIfVisible = () => {
      if (document.visibilityState === "visible") void refreshTreesRef.current();
    };
    const timer = window.setInterval(refreshIfVisible, TREE_POLL_MS);
    document.addEventListener("visibilitychange", refreshIfVisible);
    return () => {
      window.clearInterval(timer);
      document.removeEventListener("visibilitychange", refreshIfVisible);
    };
  }, [connections]);

  const toggle = useCallback(
    (key: string) => {
      const opening = !expanded.has(key);
      setExpanded((prev) => {
        const next = new Set(prev);
        if (opening) next.add(key);
        else next.delete(key);
        return next;
      });
      if (!opening) return;
      const [type, connId, id] = key.split(":");
      if (type === "c") void loadSpaces(connId);
      if (type === "s") void loadDocuments(connId, id);
    },
    [expanded, loadSpaces, loadDocuments],
  );

  /** Send a remote change to the editor, or merge it here if the document is closed. */
  const deliverRemote = useCallback(
    (key: string, base: RemoteDocument, remote: RemoteDocument, localContent: string | null) => {
      if (activeRef.current?.key === key) {
        setActive((prev) =>
          prev?.key === key
            ? {
                ...prev,
                title: remote.title,
                revision: remote.revision,
                updatedAt: remote.updatedAt,
                remoteUpdate: {
                  version: ++remoteVersion.current,
                  base: fullMarkdown(base),
                  remote: fullMarkdown(remote),
                },
              }
            : prev,
        );
        return;
      }
      const entry = bases.current.get(key);
      const codec = entry && codecs.current.get(entry.conn.id);
      if (!codec || localContent === null) return;
      const normalize = (markdown: string) => codec.serialize(codec.parse(markdown));
      const merged = merge3(normalize(fullMarkdown(base)), localContent, normalize(fullMarkdown(remote)));
      pendingWrites.current.set(key, merged.text);
    },
    [],
  );

  const updateTreeTitle = useCallback((connId: string, docId: string, title: string) => {
    const rename = (nodes: TreeNode[]): TreeNode[] =>
      nodes.map((node) =>
        node.id === docId ? { ...node, title } : { ...node, children: rename(node.children) },
      );
    setDocuments((prev) => {
      const next: Record<string, TreeNode[]> = {};
      for (const [key, nodes] of Object.entries(prev)) {
        next[key] = key.startsWith(`s:${connId}:`) ? rename(nodes) : nodes;
      }
      return next;
    });
  }, []);

  const writeOne = useCallback(
    async (key: string, content: string) => {
      const entry = bases.current.get(key);
      const codec = entry && codecs.current.get(entry.conn.id);
      const connector = entry && getConnector(entry.conn.kind);
      if (!entry || !codec || !connector) return;

      const next = splitTitle(codec, content, entry.doc.title);
      const result = await connector.write(entry.conn, entry.doc, next, codec);
      if (result.status === "saved") {
        const titleChanged = result.doc.title !== entry.doc.title;
        entry.doc = result.doc;
        if (titleChanged) updateTreeTitle(entry.conn.id, result.doc.id, result.doc.title);
        setActive((prev) =>
          prev?.key === key
            ? {
                ...prev,
                title: result.doc.title,
                revision: result.doc.revision,
                updatedAt: result.doc.updatedAt,
              }
            : prev,
        );
      } else {
        entry.doc = result.remote;
        deliverRemote(key, result.base, result.remote, content);
      }
      setError(entry.conn.id, null);
    },
    [deliverRemote, setError, updateTreeTitle],
  );

  /** Write pending edits one at a time. On failure, keep the edit and try again later. */
  const runWrites = useCallback(async () => {
    if (writing.current) return;
    writing.current = true;
    try {
      for (;;) {
        const next = pendingWrites.current.entries().next();
        if (next.done) break;
        const [key, content] = next.value;
        pendingWrites.current.delete(key);
        try {
          await writeOne(key, content);
        } catch (error) {
          if (!pendingWrites.current.has(key)) pendingWrites.current.set(key, content);
          const connId = bases.current.get(key)?.conn.id;
          if (connId) setError(connId, `Changes not saved yet: ${errorMessage(error)}`);
          window.setTimeout(() => void runWritesRef.current(), RETRY_MS);
          break;
        }
      }
    } finally {
      writing.current = false;
    }
  }, [writeOne, setError]);
  const runWritesRef = useRef(runWrites);
  runWritesRef.current = runWrites;

  const save = useCallback(
    async (key: string, content: string) => {
      pendingWrites.current.set(key, content);
      await runWrites();
    },
    [runWrites],
  );

  const openDocument = useCallback(
    async (connId: string, docRef: string) => {
      const [conn, connector] = lookup(connId);
      clearSelection();
      const loadingKey = documentKey(connId, docRef);
      setActive({
        connId,
        docId: docRef,
        key: loadingKey,
        title: "",
        content: null,
        revision: 0,
        updatedAt: 0,
        reloadVersion: 0,
      });
      try {
        const known = bases.current.get(loadingKey);
        const busy = pendingWrites.current.has(loadingKey) || writing.current;
        const doc = known && busy ? known.doc : await connector.read(conn, docRef);
        const key = documentKey(connId, doc.id);
        bases.current.set(key, { conn, doc });
        if (activeRef.current?.key !== loadingKey) return;
        setActive({
          connId,
          docId: doc.id,
          key,
          title: doc.title,
          content: fullMarkdown(doc),
          revision: doc.revision,
          updatedAt: doc.updatedAt,
          reloadVersion: 0,
        });

        // Show the document in the tree
        const space = spaceKey(connId, doc.spaceId);
        const tree = documentsRef.current[space];
        const path = tree ? findPath(tree, doc.id) : null;
        setExpanded((prev) => {
          const next = new Set(prev);
          next.add(connectionKey(connId));
          next.add(space);
          for (const node of path?.slice(0, -1) ?? []) next.add(documentKey(connId, node.id));
          return next;
        });
        if (!tree) void loadDocuments(connId, doc.spaceId);
      } catch (error) {
        toast.error(`Could not open the document: ${errorMessage(error)}`);
        setActive((prev) => (prev?.key === loadingKey ? null : prev));
      }
    },
    [lookup, clearSelection, loadDocuments],
  );

  const reloadActive = useCallback(async () => {
    const current = activeRef.current;
    const entry = current && bases.current.get(current.key);
    if (!current || !entry) return;
    const connector = getConnector(entry.conn.kind)!;
    const doc = await connector.read(entry.conn, entry.doc.id);
    entry.doc = doc;
    setActive((prev) =>
      prev?.key === current.key
        ? {
            ...prev,
            title: doc.title,
            content: fullMarkdown(doc),
            revision: doc.revision,
            updatedAt: doc.updatedAt,
            reloadVersion: prev.reloadVersion + 1,
          }
        : prev,
    );
  }, []);

  // Check the open document for changes by other people
  const activeKey = active?.content != null ? active.key : null;
  useEffect(() => {
    if (!activeKey) return;
    const check = async () => {
      if (document.visibilityState !== "visible") return;
      if (writing.current || pendingWrites.current.size > 0) return;
      const entry = bases.current.get(activeKey);
      const connector = entry && getConnector(entry.conn.kind);
      if (!entry || !connector) return;
      try {
        const remote = await connector.read(entry.conn, entry.doc.id);
        if (remote.revision <= entry.doc.revision || writing.current) return;
        const base = entry.doc;
        entry.doc = remote;
        if (remote.title !== base.title) updateTreeTitle(entry.conn.id, remote.id, remote.title);
        deliverRemote(activeKey, base, remote, null);
        setError(entry.conn.id, null);
      } catch (error) {
        setError(entry.conn.id, errorMessage(error));
      }
    };
    const timer = window.setInterval(check, DOCUMENT_POLL_MS);
    // Check at once when the window shows again
    document.addEventListener("visibilitychange", check);
    return () => {
      window.clearInterval(timer);
      document.removeEventListener("visibilitychange", check);
    };
  }, [activeKey, deliverRemote, setError, updateTreeTitle]);

  const addConnection = useCallback(
    async (kind: string, baseUrl: string, token: string) => {
      const connector = getConnector(kind);
      if (!connector) throw new Error("Unknown connection type");
      const conn = await transport.addConnection(kind, connector.label, baseUrl, token);
      try {
        const name = await connector.verify(conn);
        await transport.renameConnection(conn.id, name);
        conn.name = name;
      } catch (error) {
        await transport.removeConnection(conn.id);
        throw error;
      }
      setExpanded((prev) => new Set(prev).add(connectionKey(conn.id)));
      await reloadConnections();
      return conn;
    },
    [reloadConnections],
  );

  const removeConnection = useCallback(
    async (id: string) => {
      await transport.removeConnection(id);
      if (activeRef.current?.connId === id) setActive(null);
      await reloadConnections();
    },
    [reloadConnections],
  );

  const refresh = useCallback((connId: string) => refreshTrees(connId), [refreshTrees]);

  const createDocument = useCallback(
    async (connId: string, at: DocumentLocation) => {
      const [conn, connector] = lookup(connId);
      try {
        const doc = await connector.create(conn, at, "");
        setExpanded((prev) => {
          const next = new Set(prev).add(spaceKey(connId, at.spaceId));
          if (at.parentId) next.add(documentKey(connId, at.parentId));
          return next;
        });
        await loadDocuments(connId, at.spaceId);
        await openDocument(connId, doc.id);
      } catch (error) {
        toast.error(`Could not create the document: ${errorMessage(error)}`);
      }
    },
    [lookup, loadDocuments, openDocument],
  );

  const renameDocument = useCallback(
    async (connId: string, docId: string, title: string) => {
      const [conn, connector] = lookup(connId);
      try {
        await connector.rename(conn, docId, title);
        updateTreeTitle(connId, docId, title);
        const key = documentKey(connId, docId);
        const entry = bases.current.get(key);
        if (entry && activeRef.current?.key === key) {
          const remote = await connector.read(conn, docId);
          const base = entry.doc;
          entry.doc = remote;
          deliverRemote(key, base, remote, null);
        }
      } catch (error) {
        toast.error(`Could not rename the document: ${errorMessage(error)}`);
      }
    },
    [lookup, updateTreeTitle, deliverRemote],
  );

  const moveDocument = useCallback(
    async (connId: string, docId: string, to: DocumentLocation) => {
      const [conn, connector] = lookup(connId);
      const from = Object.entries(documentsRef.current).find(
        ([key, nodes]) => key.startsWith(`s:${connId}:`) && findPath(nodes, docId),
      );
      const path = from ? findPath(from[1], docId) : null;
      const moved = path?.[path.length - 1];
      if (to.parentId && (to.parentId === docId || (moved && findPath(moved.children, to.parentId)))) {
        return;
      }
      const currentParent = path && path.length > 1 ? path[path.length - 2].id : null;
      if (from?.[0] === spaceKey(connId, to.spaceId) && currentParent === to.parentId) {
        return;
      }
      try {
        await connector.move(conn, docId, to);
        setExpanded((prev) => {
          const next = new Set(prev).add(spaceKey(connId, to.spaceId));
          if (to.parentId) next.add(documentKey(connId, to.parentId));
          return next;
        });
        await loadDocuments(connId, to.spaceId);
        const fromSpace = from?.[0].split(":")[2];
        if (fromSpace && fromSpace !== to.spaceId) await loadDocuments(connId, fromSpace);
      } catch (error) {
        toast.error(`Could not move the document: ${errorMessage(error)}`);
      }
    },
    [lookup, loadDocuments],
  );

  const deleteDocument = useCallback(
    async (connId: string, docId: string) => {
      const [conn, connector] = lookup(connId);
      await connector.remove(conn, docId);
      const key = documentKey(connId, docId);
      pendingWrites.current.delete(key);
      bases.current.delete(key);
      if (activeRef.current?.key === key) setActive(null);
      await refreshTrees(connId);
    },
    [lookup, refreshTrees],
  );

  const openInBrowser = useCallback(
    async (connId: string, docId: string) => {
      const [conn, connector] = lookup(connId);
      const doc =
        bases.current.get(documentKey(connId, docId))?.doc ?? (await connector.read(conn, docId));
      await openUrl(connector.webUrl(conn, doc));
    },
    [lookup],
  );

  const [searchResults, setSearchResults] = useState<ConnectionSearchResult[]>([]);
  const query = searchQuery.trim();
  useEffect(() => {
    if (!query || connections.length === 0) {
      setSearchResults([]);
      return;
    }
    let cancelled = false;
    const timer = window.setTimeout(async () => {
      const results = await Promise.all(
        connections.map(async (conn) => {
          const connector = getConnector(conn.kind);
          if (!connector) return [];
          try {
            const hits = await connector.search(conn, query);
            return hits.map((hit) => ({ conn, hit }));
          } catch {
            return [];
          }
        }),
      );
      if (!cancelled) setSearchResults(results.flat());
    }, SEARCH_DEBOUNCE_MS);
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
  }, [query, connections]);


  const externalDocument = useMemo<PreviewModeData | null>(() => {
    if (!active) return null;
    const conn = connections.find((c) => c.id === active.connId);
    const connector = conn && getConnector(conn.kind);
    if (!conn || !connector) return null;
    return {
      content: active.content,
      title: active.title,
      filePath: active.key,
      modified: active.updatedAt,
      hasExternalChanges: false,
      reloadVersion: active.reloadVersion,
      save: (content, id) => save(id ?? active.key, content),
      reload: reloadActive,
      dialect: connector.dialect(conn),
      onCodec: (codec) => codecs.current.set(conn.id, codec),
      remoteUpdate: active.remoteUpdate,
      uploadImage: (file, name) => connector.uploadImage(conn, active.docId, file, name),
      openLink: (href) => {
        const ref = connector.documentFromLink(conn, href);
        if (ref) {
          void openDocument(conn.id, ref);
          return true;
        }
        if (href.startsWith("/")) {
          void openUrl(`${conn.baseUrl}${href}`);
          return true;
        }
        return href.startsWith("mention://");
      },
    };
  }, [active, connections, save, reloadActive, openDocument]);

  const value = useMemo<ConnectionsContextValue>(
    () => ({
      connections,
      spaces,
      documents,
      expanded,
      errors,
      active,
      searchResults,
      externalDocument,
      toggle,
      openDocument,
      addConnection,
      removeConnection,
      refresh,
      createDocument,
      renameDocument,
      moveDocument,
      deleteDocument,
      openInBrowser,
    }),
    [
      connections,
      spaces,
      documents,
      expanded,
      errors,
      active,
      searchResults,
      externalDocument,
      toggle,
      openDocument,
      addConnection,
      removeConnection,
      refresh,
      createDocument,
      renameDocument,
      moveDocument,
      deleteDocument,
      openInBrowser,
    ],
  );

  return <ConnectionsContext.Provider value={value}>{children}</ConnectionsContext.Provider>;
}

export function useConnections() {
  const context = useContext(ConnectionsContext);
  if (!context) throw new Error("useConnections must be used within ConnectionsProvider");
  return context;
}

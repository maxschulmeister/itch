import { memo, useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import * as ContextMenu from "@radix-ui/react-context-menu";
import { useDraggable, useDroppable } from "@dnd-kit/core";
import { toast } from "sonner";
import {
  connectionKey,
  documentKey,
  spaceKey,
  useConnections,
} from "../../connections/ConnectionsContext";
import type { ConnectionInfo, TreeNode } from "../../connections/types";
import {
  AddNoteIcon,
  ChevronDownIcon,
  ChevronRightIcon,
  CloudCheckIcon,
  ExternalLinkIcon,
  NoteIcon,
  PencilIcon,
  RefreshCwIcon,
  TrashIcon,
} from "../icons";
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "../ui";
import { FolderNameDialog } from "../notes/FolderNameDialog";

const menuItemClass =
  "px-3 py-1.5 text-sm text-text cursor-pointer outline-none hover:bg-bg-muted focus:bg-bg-muted flex items-center gap-2 rounded-sm";
const iconClass = "w-4 h-4 stroke-[1.6]";

type Row =
  | { type: "connection"; key: string; depth: number; conn: ConnectionInfo }
  | { type: "space"; key: string; depth: number; conn: ConnectionInfo; space: TreeNode }
  | {
      type: "document";
      key: string;
      depth: number;
      conn: ConnectionInfo;
      spaceId: string;
      node: TreeNode;
    };

/** Drop data for a place that can hold documents. */
export interface ConnectionDropTarget {
  type: "connection-target";
  connId: string;
  spaceId: string;
  parentId: string | null;
}

/** Drag data for a connection document. */
export interface ConnectionDragItem {
  type: "connection-doc";
  connId: string;
  docId: string;
  title: string;
}

/** Outline icons can be emoji or icon names. Only emoji can be shown here. */
const emojiIcon = (icon?: string | null) => (icon && !/^[a-z-]+$/i.test(icon) ? icon : null);

const displayTitle = (title: string) => title.trim() || "Untitled";

interface RowViewProps {
  rowKey: string;
  depth: number;
  icon: ReactNode;
  label: string;
  muted?: boolean;
  selected?: boolean;
  focused: boolean;
  onClick: () => void;
  menu: ReactNode;
  drag?: ConnectionDragItem;
  drop?: ConnectionDropTarget;
}

const RowView = memo(function RowView({
  rowKey,
  depth,
  icon,
  label,
  muted,
  selected,
  focused,
  onClick,
  menu,
  drag,
  drop,
}: RowViewProps) {
  const ref = useRef<HTMLDivElement | null>(null);
  const draggable = useDraggable({ id: `conn-drag:${rowKey}`, data: drag, disabled: !drag });
  const droppable = useDroppable({ id: `conn-drop:${rowKey}`, data: drop, disabled: !drop });

  useEffect(() => {
    if (selected) ref.current?.scrollIntoView({ block: "nearest", behavior: "smooth" });
  }, [selected]);

  const state = draggable.isDragging
    ? "opacity-40"
    : droppable.isOver
      ? "bg-accent/10 ring-1 ring-accent"
      : selected
        ? "bg-bg-muted group-focus/connections:ring-1 group-focus/connections:ring-text-muted"
        : focused
          ? "bg-bg-muted/50 ring-1 ring-text-muted/30"
          : "hover:bg-bg-muted";

  return (
    <ContextMenu.Root>
      <ContextMenu.Trigger asChild>
        <div
          ref={(el) => {
            ref.current = el;
            draggable.setNodeRef(el);
            droppable.setNodeRef(el);
          }}
          {...(drag ? draggable.attributes : {})}
          {...(drag ? draggable.listeners : {})}
          role="button"
          tabIndex={-1}
          onClick={onClick}
          className={`flex items-center gap-1.5 py-1.5 cursor-pointer rounded-md select-none transition-colors ${state}`}
          style={{ paddingLeft: `${depth * 12 + 8}px`, paddingRight: "8px" }}
        >
          {icon}
          <span className={`text-sm truncate ${muted ? "text-text-muted" : "text-text"}`}>
            {label}
          </span>
        </div>
      </ContextMenu.Trigger>
      <ContextMenu.Portal>
        <ContextMenu.Content className="min-w-44 bg-bg border border-border rounded-md shadow-lg py-1 z-50">
          {menu}
        </ContextMenu.Content>
      </ContextMenu.Portal>
    </ContextMenu.Root>
  );
});

function Chevron({ open, onToggle }: { open: boolean; onToggle?: () => void }) {
  const Icon = open ? ChevronDownIcon : ChevronRightIcon;
  return (
    <span
      className="shrink-0 flex"
      onClick={
        onToggle &&
        ((e) => {
          e.stopPropagation();
          onToggle();
        })
      }
    >
      <Icon className="w-4 h-4 stroke-[1.6] text-text-muted/60" />
    </span>
  );
}

/** Connection folders in the sidebar, for example an Outline workspace. */
export function ConnectionsTree() {
  const {
    connections,
    spaces,
    documents,
    expanded,
    errors,
    active,
    toggle,
    openDocument,
    refresh,
    createDocument,
    renameDocument,
    deleteDocument,
    openInBrowser,
  } = useConnections();

  const [focusedKey, setFocusedKey] = useState<string | null>(null);
  const [renaming, setRenaming] = useState<{ connId: string; node: TreeNode } | null>(null);
  const [deleting, setDeleting] = useState<{ connId: string; node: TreeNode } | null>(null);
  const [isDeleting, setIsDeleting] = useState(false);

  const rows = useMemo(() => {
    const result: Row[] = [];
    const walk = (conn: ConnectionInfo, spaceId: string, nodes: TreeNode[], depth: number) => {
      for (const node of nodes) {
        const key = documentKey(conn.id, node.id);
        result.push({ type: "document", key, depth, conn, spaceId, node });
        if (node.children.length > 0 && expanded.has(key)) {
          walk(conn, spaceId, node.children, depth + 1);
        }
      }
    };
    for (const conn of connections) {
      const key = connectionKey(conn.id);
      result.push({ type: "connection", key, depth: 0, conn });
      if (!expanded.has(key)) continue;
      for (const space of spaces[conn.id] ?? []) {
        const sKey = spaceKey(conn.id, space.id);
        result.push({ type: "space", key: sKey, depth: 1, conn, space });
        if (expanded.has(sKey)) walk(conn, space.id, documents[sKey] ?? [], 2);
      }
    }
    return result;
  }, [connections, spaces, documents, expanded]);

  useEffect(() => {
    if (active) setFocusedKey(active.key);
  }, [active]);

  const activate = useCallback(
    (row: Row) => {
      setFocusedKey(row.key);
      if (row.type === "document") void openDocument(row.conn.id, row.node.id);
      else toggle(row.key);
    },
    [openDocument, toggle],
  );

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      if (!["ArrowDown", "ArrowUp", "ArrowRight", "ArrowLeft", "Enter"].includes(e.key)) return;
      // The app moves through local notes on arrow keys; keep these keys here
      e.stopPropagation();
      const index = rows.findIndex((row) => row.key === focusedKey);
      const row = rows[index];
      const expandable = row && (row.type !== "document" || row.node.children.length > 0);
      if (e.key === "ArrowDown" || e.key === "ArrowUp") {
        e.preventDefault();
        const next = rows[e.key === "ArrowDown" ? index + 1 : Math.max(index - 1, 0)];
        if (!next) return;
        setFocusedKey(next.key);
        if (next.type === "document") void openDocument(next.conn.id, next.node.id);
      } else if (e.key === "ArrowRight" && row && expandable && !expanded.has(row.key)) {
        e.preventDefault();
        toggle(row.key);
      } else if (e.key === "ArrowLeft" && row && expandable && expanded.has(row.key)) {
        e.preventDefault();
        toggle(row.key);
      } else if (e.key === "Enter" && row) {
        e.preventDefault();
        if (row.type === "document") {
          (document.querySelector(".ProseMirror") as HTMLElement | null)?.focus();
        } else {
          toggle(row.key);
        }
      }
    },
    [rows, focusedKey, expanded, toggle, openDocument],
  );

  if (connections.length === 0) return null;

  const renderRow = (row: Row) => {
    const isOpen = expanded.has(row.key);
    const focused = focusedKey === row.key;

    if (row.type === "connection") {
      const error = errors[row.conn.id];
      return (
        <div key={row.key}>
          <RowView
            rowKey={row.key}
            depth={row.depth}
            focused={focused}
            muted
            label={row.conn.name}
            icon={
              <>
                <Chevron open={isOpen} />
                <CloudCheckIcon className={`${iconClass} text-text-muted shrink-0`} />
              </>
            }
            onClick={() => activate(row)}
            menu={
              <>
                <ContextMenu.Item className={menuItemClass} onSelect={() => void refresh(row.conn.id)}>
                  <RefreshCwIcon className={iconClass} />
                  Refresh
                </ContextMenu.Item>
              </>
            }
          />
          {error && (
            <p className="px-2 pb-1 text-xs text-red-500 truncate" title={error} role="status">
              {error}
            </p>
          )}
        </div>
      );
    }

    if (row.type === "space") {
      const emoji = emojiIcon(row.space.icon);
      return (
        <RowView
          key={row.key}
          rowKey={row.key}
          depth={row.depth}
          focused={focused}
          muted
          label={row.space.title}
          icon={
            <>
              <Chevron open={isOpen} />
              {emoji && <span className="text-sm leading-none shrink-0">{emoji}</span>}
            </>
          }
          onClick={() => activate(row)}
          drop={{ type: "connection-target", connId: row.conn.id, spaceId: row.space.id, parentId: null }}
          menu={
            <ContextMenu.Item
              className={menuItemClass}
              onSelect={() => void createDocument(row.conn.id, { spaceId: row.space.id, parentId: null })}
            >
              <AddNoteIcon className={iconClass} />
              New document
            </ContextMenu.Item>
          }
        />
      );
    }

    const { node, conn, spaceId } = row;
    const hasChildren = node.children.length > 0;
    const emoji = emojiIcon(node.icon);
    return (
      <RowView
        key={row.key}
        rowKey={row.key}
        depth={row.depth}
        focused={focused}
        selected={active?.key === row.key}
        label={displayTitle(node.title)}
        icon={
          <>
            {hasChildren ? (
              <Chevron open={isOpen} onToggle={() => toggle(row.key)} />
            ) : emoji ? null : (
              <NoteIcon className={`${iconClass} opacity-50 shrink-0`} />
            )}
            {emoji && <span className="text-sm leading-none shrink-0">{emoji}</span>}
          </>
        }
        onClick={() => activate(row)}
        drag={{ type: "connection-doc", connId: conn.id, docId: node.id, title: displayTitle(node.title) }}
        drop={{ type: "connection-target", connId: conn.id, spaceId, parentId: node.id }}
        menu={
          <>
            <ContextMenu.Item
              className={menuItemClass}
              onSelect={() => void createDocument(conn.id, { spaceId, parentId: node.id })}
            >
              <AddNoteIcon className={iconClass} />
              New nested document
            </ContextMenu.Item>
            <ContextMenu.Item className={menuItemClass} onSelect={() => setRenaming({ connId: conn.id, node })}>
              <PencilIcon className={iconClass} />
              Rename
            </ContextMenu.Item>
            <ContextMenu.Item
              className={menuItemClass}
              onSelect={() =>
                void openInBrowser(conn.id, node.id).catch((error) =>
                  toast.error(`Could not open the document: ${error?.message ?? error}`),
                )
              }
            >
              <ExternalLinkIcon className={iconClass} />
              Open in browser
            </ContextMenu.Item>
            <div className="h-px bg-border my-1" />
            <ContextMenu.Item className={menuItemClass} onSelect={() => setDeleting({ connId: conn.id, node })}>
              <TrashIcon className={iconClass} />
              Delete
            </ContextMenu.Item>
          </>
        }
      />
    );
  };

  return (
    <>
      <div
        tabIndex={0}
        data-connections-tree
        className="group/connections flex flex-col gap-0.5 px-1.5 pt-1.5 outline-none"
        onKeyDown={handleKeyDown}
      >
        {rows.map(renderRow)}
      </div>

      <FolderNameDialog
        open={renaming !== null}
        onOpenChange={(open) => !open && setRenaming(null)}
        onConfirm={(title) => {
          if (renaming) void renameDocument(renaming.connId, renaming.node.id, title);
          setRenaming(null);
        }}
        title="Rename document"
        description="Enter a new title"
        placeholder="Title"
        confirmLabel="Rename"
        defaultValue={renaming?.node.title ?? ""}
      />

      <AlertDialog open={deleting !== null} onOpenChange={(open) => !open && setDeleting(null)}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Delete document?</AlertDialogTitle>
            <AlertDialogDescription>
              “{displayTitle(deleting?.node.title ?? "")}” and its nested documents move to the
              trash. You can restore them in the web app.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>Cancel</AlertDialogCancel>
            <AlertDialogAction
              disabled={isDeleting}
              onClick={async (e) => {
                e.preventDefault();
                if (!deleting) return;
                setIsDeleting(true);
                try {
                  await deleteDocument(deleting.connId, deleting.node.id);
                  setDeleting(null);
                } catch (error) {
                  toast.error(`Could not delete the document: ${error instanceof Error ? error.message : error}`);
                } finally {
                  setIsDeleting(false);
                }
              }}
            >
              {isDeleting ? "Deleting…" : "Delete"}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </>
  );
}

import { useCallback, useEffect, useRef, useState } from "react";
import { Extension, posToDOMRect, type Editor as TiptapEditor } from "@tiptap/core";
import { ReactRenderer } from "@tiptap/react";
import type { Fragment, Node as PMNode } from "@tiptap/pm/model";
import { DOMSerializer } from "@tiptap/pm/model";
import { Plugin, PluginKey, type Transaction } from "@tiptap/pm/state";
import { Decoration, DecorationSet } from "@tiptap/pm/view";
import tippy, { type Instance as TippyInstance } from "tippy.js";
import {
  AiPopover,
  type AiApplyAction,
  type AiContext,
  type AiPopoverProps,
} from "./AiPopover";

type Range = { from: number; to: number };

const aiHighlightKey = new PluginKey<DecorationSet>("aiHighlight");

/** Shows `range` as the AI target while the focus is outside the editor. */
export function setAiHighlight(editor: TiptapEditor, range: Range | null) {
  editor.view.dispatch(editor.state.tr.setMeta(aiHighlightKey, range));
}

/** Keeps the AI target range visible while focus is in the popover. */
export const AiHighlight = Extension.create({
  name: "aiHighlight",
  addProseMirrorPlugins() {
    return [
      new Plugin<DecorationSet>({
        key: aiHighlightKey,
        state: {
          init: () => DecorationSet.empty,
          apply(tr, set) {
            const range = tr.getMeta(aiHighlightKey) as Range | null | undefined;
            if (range === undefined) return set.map(tr.mapping, tr.doc);
            if (range === null) return DecorationSet.empty;
            return DecorationSet.create(tr.doc, [
              Decoration.inline(range.from, range.to, { class: "ai-target" }),
            ]);
          },
        },
        props: {
          decorations: (state) => aiHighlightKey.getState(state),
        },
      }),
    ];
  },
});

// Private-use character: the markdown serializer does not escape it.
const SENTINEL = "\uE000";

function serialize(editor: TiptapEditor, doc: PMNode): string {
  const manager = editor.storage.markdown?.manager;
  if (!manager) return doc.textContent;
  return manager.serialize(doc.toJSON()).replace(/&nbsp;|&#160;/g, " ");
}

/** Document markdown split at the selection (or cursor), plus the selection. */
function buildContext(editor: TiptapEditor, { from, to }: Range): AiContext {
  const { state, schema } = editor;
  let selection: string | null = null;
  if (from !== to) {
    const content = state.doc.slice(from, to).content;
    const block = content.firstChild?.isInline
      ? schema.nodes.paragraph.create(null, content)
      : null;
    selection = serialize(
      editor,
      schema.topNodeType.create(null, block ?? content),
    ).trim();
  }

  try {
    // Mark the range in a copy of the document. The transaction is never
    // dispatched, so the editor document does not change.
    const tr = state.tr.insertText(SENTINEL, to);
    if (selection !== null) tr.insertText(SENTINEL, from);
    const parts = serialize(editor, tr.doc).split(SENTINEL);
    if (selection !== null && parts.length === 3) {
      return { before: parts[0], after: parts[2], selection };
    }
    if (selection === null && parts.length === 2) {
      return { before: parts[0], after: parts[1], selection };
    }
  } catch (error) {
    console.warn("Failed to mark the AI range:", error);
  }
  return { before: serialize(editor, state.doc), after: "", selection };
}

function parseMarkdown(editor: TiptapEditor, markdown: string): Fragment {
  const manager = editor.storage.markdown?.manager;
  if (!manager) throw new Error("Markdown extension is not loaded");
  return editor.schema.nodeFromJSON(manager.parse(markdown)).content;
}

function renderMarkdown(
  editor: TiptapEditor,
  markdown: string,
  target: HTMLElement,
) {
  try {
    const fragment = parseMarkdown(editor, markdown);
    target.replaceChildren(
      DOMSerializer.fromSchema(editor.schema).serializeFragment(fragment),
    );
  } catch {
    target.textContent = markdown;
  }
}

/** Inserts the markdown as editor nodes in one transaction (one undo step). */
function applyMarkdown(
  editor: TiptapEditor,
  range: Range,
  markdown: string,
  action: AiApplyAction,
) {
  let content = parseMarkdown(editor, markdown);
  const { doc } = editor.state;

  if (action === "below") {
    const $to = doc.resolve(range.to);
    const pos = $to.depth > 0 ? $to.after(1) : range.to;
    editor.chain().focus().insertContentAt(pos, content).run();
    return;
  }

  // A one-paragraph answer inside one text block replaces inline text,
  // so the block is not split.
  const $from = doc.resolve(range.from);
  const $to = doc.resolve(range.to);
  const single = content.childCount === 1 ? content.firstChild : null;
  if (
    single?.type.name === "paragraph" &&
    $from.parent.isTextblock &&
    !$from.parent.type.spec.code &&
    $from.sameParent($to)
  ) {
    content = single.content;
  }
  editor.chain().focus().insertContentAt(range, content).run();
}

/** The editor DOM, or null while the view is not mounted (no note open). */
function editorDom(editor: TiptapEditor): HTMLElement | null {
  try {
    return editor.view.dom;
  } catch {
    return null;
  }
}

const isAiShortcut = (e: KeyboardEvent) =>
  (e.metaKey || e.ctrlKey) &&
  !e.shiftKey &&
  !e.altKey &&
  e.key.toLowerCase() === "j";

/** A fixed instruction that runs at once, for example from the selection menu. */
export interface AiRun {
  instruction: string;
  /** The answer is for reading (Explain): no "Replace selection". */
  readOnly?: boolean;
}

/**
 * Inline AI for the editor: Mod+J, the "/Ask AI" slash command, and the
 * popover that streams the pi response. Returns `openPopover` (empty
 * prompt), `runAi` (sends an instruction at once) and whether the popover
 * is open.
 */
export function useAiAssist(editor: TiptapEditor | null) {
  const popupRef = useRef<TippyInstance | null>(null);
  const stopWatchingDocRef = useRef<(() => void) | null>(null);
  const [popoverOpen, setPopoverOpen] = useState(false);

  const closePopover = useCallback(
    (focusEditor: boolean) => {
      stopWatchingDocRef.current?.();
      stopWatchingDocRef.current = null;
      const popup = popupRef.current;
      if (!popup) return;
      popupRef.current = null;
      popup.destroy();
      setPopoverOpen(false);
      if (!editor || editor.isDestroyed) return;
      setAiHighlight(editor, null);
      if (focusEditor) editor.commands.focus();
    },
    [editor],
  );

  const open = useCallback(
    (initial?: AiRun) => {
      if (!editor || !editor.isEditable) return;
      closePopover(false);

      const { from, to, empty } = editor.state.selection;
      const range = { from, to };
      if (!empty) setAiHighlight(editor, range);

      const props: AiPopoverProps = {
        mode: empty ? "cursor" : "selection",
        context: buildContext(editor, range),
        initialInstruction: initial?.instruction,
        readOnly: !empty && initial?.readOnly,
        renderMarkdown: (markdown, target) =>
          renderMarkdown(editor, markdown, target),
        onApply: (markdown, action) => {
          closePopover(false);
          applyMarkdown(editor, range, markdown, action);
        },
        onClose: () => closePopover(true),
        onLayoutChange: () => popupRef.current?.popperInstance?.update(),
      };
      const component = new ReactRenderer(AiPopover, { props, editor });

      popupRef.current = tippy(document.body, {
        getReferenceClientRect: () => posToDOMRect(editor.view, from, to),
        appendTo: () => document.body,
        content: component.element,
        showOnCreate: true,
        interactive: true,
        trigger: "manual",
        hideOnClick: false,
        placement: "bottom-start",
        offset: [0, 8],
        maxWidth: "none",
        popperOptions: {
          modifiers: [
            { name: "flip", options: { fallbackPlacements: ["top-start"] } },
            {
              name: "preventOverflow",
              options: { altAxis: true, tether: false, padding: 8 },
            },
          ],
        },
        onClickOutside: () => closePopover(false),
        onDestroy: () => component.destroy(),
      });
      setPopoverOpen(true);

      // The stored range is only valid for this document: close on any change
      // (external reload, note switch, undo from a menu).
      const onTransaction = ({ transaction }: { transaction: Transaction }) => {
        if (transaction.docChanged) closePopover(false);
      };
      editor.on("transaction", onTransaction);
      stopWatchingDocRef.current = () => editor.off("transaction", onTransaction);
    },
    [editor, closePopover],
  );

  const openPopover = useCallback(() => open(), [open]);

  // Close the popover (and stop pi) when the editor goes away.
  useEffect(() => () => closePopover(false), [closePopover]);

  // Mod+J in the editor
  useEffect(() => {
    if (!editor) return;
    const handleKeyDown = (e: KeyboardEvent) => {
      if (!isAiShortcut(e)) return;
      if (!editorDom(editor)?.contains(e.target as Node)) return;
      e.preventDefault();
      openPopover();
    };
    document.addEventListener("keydown", handleKeyDown);
    return () => document.removeEventListener("keydown", handleKeyDown);
  }, [editor, openPopover]);

  // "Ask AI" slash command
  useEffect(() => {
    const handler = () => openPopover();
    window.addEventListener("slash-command-ai", handler);
    return () => window.removeEventListener("slash-command-ai", handler);
  }, [openPopover]);

  // Keep the popover at the selection while the note scrolls.
  useEffect(() => {
    const onScroll = () => popupRef.current?.popperInstance?.update();
    document.addEventListener("scroll", onScroll, true);
    return () => document.removeEventListener("scroll", onScroll, true);
  }, []);

  return { openPopover, runAi: open, popoverOpen };
}

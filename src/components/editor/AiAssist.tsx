import { useCallback, useEffect, useRef } from "react";
import { Extension, posToDOMRect, type Editor as TiptapEditor } from "@tiptap/core";
import { ReactRenderer } from "@tiptap/react";
import type { Fragment, Node as PMNode } from "@tiptap/pm/model";
import { DOMSerializer } from "@tiptap/pm/model";
import { Plugin, PluginKey, TextSelection, type Transaction } from "@tiptap/pm/state";
import { Decoration, DecorationSet } from "@tiptap/pm/view";
import tippy, { type Instance as TippyInstance } from "tippy.js";
import {
  AiPopover,
  type AiApplyAction,
  type AiContext,
  type AiPopoverProps,
} from "./AiPopover";
import { SparklesIcon } from "../icons";
import { mod } from "../../lib/platform";

type Range = { from: number; to: number };

const aiHighlightKey = new PluginKey<DecorationSet>("aiHighlight");

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

function AiBubbleButton({ onClick }: { onClick: () => void }) {
  return (
    <button
      type="button"
      // Keep the editor focus and selection.
      onMouseDown={(e) => e.preventDefault()}
      onClick={onClick}
      title={`Ask AI (${mod}J)`}
      className="flex items-center gap-1.5 h-7 px-2.5 text-sm font-medium text-text bg-bg border border-border rounded-lg shadow-md hover:bg-bg-muted transition-colors cursor-pointer focus-visible:outline-2 focus-visible:outline-accent"
    >
      <SparklesIcon className="w-3.75 h-3.75 stroke-[1.5]" />
      Ask AI
    </button>
  );
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

/**
 * Inline AI for the editor: the "Ask AI" bubble on a selection, Mod+J, the
 * "/Ask AI" slash command, and the popover that streams the pi response.
 * Returns the function that opens the popover (for the format bar button).
 */
export function useAiAssist(editor: TiptapEditor | null) {
  const popupRef = useRef<TippyInstance | null>(null);
  const bubbleRef = useRef<TippyInstance | null>(null);
  const stopWatchingDocRef = useRef<(() => void) | null>(null);

  const closePopover = useCallback(
    (focusEditor: boolean) => {
      stopWatchingDocRef.current?.();
      stopWatchingDocRef.current = null;
      const popup = popupRef.current;
      if (!popup) return;
      popupRef.current = null;
      popup.destroy();
      if (!editor || editor.isDestroyed) return;
      editor.view.dispatch(editor.state.tr.setMeta(aiHighlightKey, null));
      if (focusEditor) editor.commands.focus();
    },
    [editor],
  );

  const openPopover = useCallback(() => {
    if (!editor || !editor.isEditable) return;
    closePopover(false);
    bubbleRef.current?.hide();

    const { from, to, empty } = editor.state.selection;
    const range = { from, to };
    if (!empty) {
      editor.view.dispatch(editor.state.tr.setMeta(aiHighlightKey, range));
    }

    const props: AiPopoverProps = {
      mode: empty ? "cursor" : "selection",
      context: buildContext(editor, range),
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

    // The stored range is only valid for this document: close on any change
    // (external reload, note switch, undo from a menu).
    const onTransaction = ({ transaction }: { transaction: Transaction }) => {
      if (transaction.docChanged) closePopover(false);
    };
    editor.on("transaction", onTransaction);
    stopWatchingDocRef.current = () => editor.off("transaction", onTransaction);
  }, [editor, closePopover]);

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

  // "Ask AI" bubble on a text selection
  useEffect(() => {
    if (!editor) return;
    // Created on the first show: a ReactRenderer made before the editor view
    // is mounted never renders its component.
    let component: ReactRenderer | null = null;
    const bubble = tippy(document.body, {
      appendTo: () => document.body,
      interactive: true,
      trigger: "manual",
      hideOnClick: false,
      placement: "top-start",
      offset: [0, 8],
    });
    bubbleRef.current = bubble;
    let pointerDown = false;

    const update = () => {
      const { selection } = editor.state;
      const visible =
        editorDom(editor) !== null &&
        editor.isEditable &&
        editor.isFocused &&
        !pointerDown &&
        !popupRef.current &&
        selection instanceof TextSelection &&
        !selection.empty &&
        !editor.isActive("codeBlock") &&
        !editor.isActive("frontmatter");
      if (!visible) {
        bubble.hide();
        return;
      }
      if (!component) {
        component = new ReactRenderer(AiBubbleButton, {
          props: { onClick: openPopover },
          editor,
        });
        bubble.setContent(component.element);
      }
      const { from, to } = selection;
      bubble.setProps({
        getReferenceClientRect: () => posToDOMRect(editor.view, from, to),
      });
      bubble.show();
    };
    // Show the bubble when the mouse selection is complete.
    const onPointerDown = (e: PointerEvent) => {
      if (!editorDom(editor)?.contains(e.target as Node)) return;
      pointerDown = true;
      bubble.hide();
    };
    const onPointerUp = () => {
      if (!pointerDown) return;
      pointerDown = false;
      requestAnimationFrame(update);
    };
    const onBlur = () => bubble.hide();
    const onScroll = () => {
      bubble.popperInstance?.update();
      popupRef.current?.popperInstance?.update();
    };

    editor.on("selectionUpdate", update);
    editor.on("focus", update);
    editor.on("blur", onBlur);
    document.addEventListener("pointerdown", onPointerDown);
    document.addEventListener("pointerup", onPointerUp);
    document.addEventListener("scroll", onScroll, true);
    return () => {
      editor.off("selectionUpdate", update);
      editor.off("focus", update);
      editor.off("blur", onBlur);
      document.removeEventListener("pointerdown", onPointerDown);
      document.removeEventListener("pointerup", onPointerUp);
      document.removeEventListener("scroll", onScroll, true);
      bubbleRef.current = null;
      bubble.destroy();
      component?.destroy();
    };
  }, [editor, openPopover]);

  return openPopover;
}

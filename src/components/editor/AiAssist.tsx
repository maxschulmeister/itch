import { useCallback, useEffect, useRef, useState } from "react";
import { Extension, type Editor as TiptapEditor } from "@tiptap/core";
import { ReactRenderer } from "@tiptap/react";
import type { Fragment, Node as PMNode } from "@tiptap/pm/model";
import { DOMSerializer } from "@tiptap/pm/model";
import {
  Plugin,
  PluginKey,
  type EditorState,
  type Transaction,
} from "@tiptap/pm/state";
import { Decoration, DecorationSet } from "@tiptap/pm/view";
import tippy, { type Instance as TippyInstance } from "tippy.js";
import {
  AiMenu,
  type AiApplyAction,
  type AiContext,
  type AiMenuProps,
} from "./AiMenu";
import { isInMenuLayer } from "./menuParts";

type Range = { from: number; to: number };

interface AiHighlightState {
  /** The text that the AI menu edits, or the cursor. */
  range: Range | null;
  /** The streaming answer, shown in place of `range`. */
  preview: HTMLElement | null;
  /** Changes with each new preview element (the widget key). */
  version: number;
}

type AiHighlightMeta = { range: Range | null } | { preview: HTMLElement | null };

const aiHighlightKey = new PluginKey<AiHighlightState>("aiHighlight");

/** Shows `range` as the AI target while the focus is outside the editor. */
export function setAiHighlight(editor: TiptapEditor, range: Range | null) {
  editor.view.dispatch(
    editor.state.tr.setMeta(aiHighlightKey, { range } satisfies AiHighlightMeta),
  );
}

/** Shows `preview` in place of the AI target (null: the target again). */
function setAiPreview(editor: TiptapEditor, preview: HTMLElement | null) {
  editor.view.dispatch(
    editor.state.tr.setMeta(aiHighlightKey, { preview } satisfies AiHighlightMeta),
  );
}

/** The last inline position of a block (its end before the closing tokens). */
function contentEnd(node: PMNode, pos: number): number {
  let end = pos + node.nodeSize - 1;
  let current = node;
  while (!current.isTextblock && current.lastChild && !current.lastChild.isInline) {
    current = current.lastChild;
    end -= 1;
  }
  return end;
}

function aiDecorations(state: EditorState): DecorationSet {
  const plugin = aiHighlightKey.getState(state);
  const range = plugin?.range;
  if (!range) return DecorationSet.empty;
  const { from, to } = range;
  const decorations: Decoration[] = [];

  if (!plugin.preview) {
    if (from < to) {
      decorations.push(Decoration.inline(from, to, { class: "ai-target" }));
    }
    return DecorationSet.create(state.doc, decorations);
  }

  decorations.push(
    Decoration.widget(from, plugin.preview, {
      side: -1,
      key: `ai-preview-${plugin.version}`,
      ignoreSelection: true,
    }),
  );
  if (from < to) {
    decorations.push(Decoration.inline(from, to, { class: "ai-replaced" }));
    // Blocks after the first one that the range fully covers: hide them too,
    // so that their margins and list markers go away.
    const firstBlockEnd = state.doc.resolve(from).end();
    state.doc.nodesBetween(from, to, (node, pos) => {
      if (!node.isBlock || pos < firstBlockEnd) return true;
      if (contentEnd(node, pos) > to) return true;
      decorations.push(
        Decoration.node(pos, pos + node.nodeSize, { class: "ai-replaced" }),
      );
      return false;
    });
  }
  return DecorationSet.create(state.doc, decorations);
}

/** Keeps the AI target visible, and shows the streaming answer in its place. */
export const AiHighlight = Extension.create({
  name: "aiHighlight",
  addProseMirrorPlugins() {
    return [
      new Plugin<AiHighlightState>({
        key: aiHighlightKey,
        state: {
          init: () => ({ range: null, preview: null, version: 0 }),
          apply(tr, value) {
            const meta = tr.getMeta(aiHighlightKey) as AiHighlightMeta | undefined;
            if (!meta) {
              if (!value.range || !tr.docChanged) return value;
              return {
                ...value,
                range: {
                  from: tr.mapping.map(value.range.from),
                  to: tr.mapping.map(value.range.to),
                },
              };
            }
            if ("range" in meta) {
              return { range: meta.range, preview: null, version: value.version };
            }
            return {
              ...value,
              preview: meta.preview,
              version: value.version + 1,
            };
          },
        },
        props: {
          decorations: aiDecorations,
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

/**
 * The paragraph of a one-paragraph answer when it can replace inline text
 * (the range is in one text block), so the block is not split.
 */
function inlineParagraph(
  editor: TiptapEditor,
  range: Range,
  content: Fragment,
): PMNode | null {
  const { doc } = editor.state;
  const $from = doc.resolve(range.from);
  const $to = doc.resolve(range.to);
  const single = content.childCount === 1 ? content.firstChild : null;
  return single?.type.name === "paragraph" &&
    $from.parent.isTextblock &&
    !$from.parent.type.spec.code &&
    $from.sameParent($to)
    ? single
    : null;
}

/** Inserts the markdown as editor nodes in one transaction (one undo step). */
function applyMarkdown(
  editor: TiptapEditor,
  range: Range,
  markdown: string,
  action: AiApplyAction,
) {
  const content = parseMarkdown(editor, markdown);

  if (action === "below") {
    const $to = editor.state.doc.resolve(range.to);
    const pos = $to.depth > 0 ? $to.after(1) : range.to;
    editor.chain().focus().insertContentAt(pos, content).run();
    return;
  }

  const paragraph = inlineParagraph(editor, range, content);
  editor
    .chain()
    .focus()
    .insertContentAt(range, paragraph ? paragraph.content : content)
    .run();
}

/**
 * Renders the streaming answer into one element that the editor shows in
 * place of the range. A new element only when the shape changes (inline
 * text or blocks); else its children change, without a transaction.
 */
function createPreview(editor: TiptapEditor, range: Range) {
  let element: HTMLElement | null = null;
  let inline = false;

  return (markdown: string | null) => {
    if (!markdown?.trim()) {
      if (element) {
        element = null;
        setAiPreview(editor, null);
      }
      return;
    }
    let content: Fragment | null = null;
    try {
      content = parseMarkdown(editor, markdown);
    } catch {
      content = null;
    }
    const paragraph = content ? inlineParagraph(editor, range, content) : null;
    const nextInline = !content || !!paragraph;
    if (!element || inline !== nextInline) {
      inline = nextInline;
      element = document.createElement(inline ? "span" : "div");
      element.className = "ai-insertion";
      setAiPreview(editor, element);
    }
    if (!content) {
      element.textContent = markdown;
      return;
    }
    element.replaceChildren(
      DOMSerializer.fromSchema(editor.schema).serializeFragment(
        paragraph ? paragraph.content : content,
      ),
    );
  };
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
  /** The answer is for reading (Explain): no "Apply". */
  readOnly?: boolean;
}

const MIN_MENU_WIDTH = 320;
/** Space between the target and the AI menu. */
const MENU_OFFSET = 4;
/** Space between the AI menu and the bottom of the scroll container. */
const SCROLL_MARGIN = 12;

/**
 * The top-level block element that contains `pos`. Between two top-level
 * blocks, the block before `pos`.
 */
function topBlockDom(editor: TiptapEditor, pos: number): HTMLElement | null {
  const { doc } = editor.state;
  const $pos = doc.resolve(Math.max(0, Math.min(pos, doc.content.size)));
  const blockPos =
    $pos.depth > 0
      ? $pos.before(1)
      : $pos.nodeBefore
        ? pos - $pos.nodeBefore.nodeSize
        : null;
  const dom = blockPos === null ? null : editor.view.nodeDOM(blockPos);
  return dom instanceof HTMLElement ? dom : null;
}

/** False for an element that is not rendered (`display: none`). */
const isRendered = (element: Element | null): element is Element =>
  !!element && element.getClientRects().length > 0;

/** The nearest ancestor that scrolls vertically. */
function scrollParent(element: HTMLElement): HTMLElement | null {
  for (let node = element.parentElement; node; node = node.parentElement) {
    if (/(auto|scroll)/.test(getComputedStyle(node).overflowY)) return node;
  }
  return null;
}

/** The height of the AI menu, with the suggestion list below the card. */
function menuExtent(element: HTMLElement): number {
  const top = element.getBoundingClientRect().top;
  const lists = element.querySelectorAll("[role='listbox']");
  return Math.max(
    element.offsetHeight,
    ...Array.from(lists, (list) => list.getBoundingClientRect().bottom - top),
  );
}

const prefersReducedMotion = () =>
  window.matchMedia("(prefers-reduced-motion: reduce)").matches;

/**
 * Inline AI for the editor: Mod+J, the "/Ask AI" slash command, and the AI
 * menu below the target block. The answer streams into the document as a
 * preview. Returns `openPopover` (prompt input), `runAi` (sends an
 * instruction at once) and whether the AI menu is open.
 */
export function useAiAssist(editor: TiptapEditor | null) {
  const popupRef = useRef<TippyInstance | null>(null);
  // Removes the listeners and the scroll space of the open AI menu.
  const cleanupRef = useRef<(() => void) | null>(null);
  const [popoverOpen, setPopoverOpen] = useState(false);

  const closePopover = useCallback(
    (focusEditor: boolean) => {
      cleanupRef.current?.();
      cleanupRef.current = null;
      const popup = popupRef.current;
      if (!popup) return;
      popupRef.current = null;
      popup.destroy();
      setPopoverOpen(false);
      if (!editor || editor.isDestroyed) return;
      // Focus before the highlight goes away: the DOM selection is still in
      // the highlighted text, so WebKit keeps it.
      if (focusEditor) editor.view.focus();
      setAiHighlight(editor, null);
    },
    [editor],
  );

  const open = useCallback(
    (initial?: AiRun) => {
      if (!editor || !editor.isEditable) return;
      closePopover(false);

      const { from, to, empty } = editor.state.selection;
      const range = { from, to };
      setAiHighlight(editor, range);
      const preview = createPreview(editor, range);
      const scroller = scrollParent(editor.view.dom);
      // Scroll with the answer until the user scrolls during the stream.
      let follow = true;
      let streaming = !!initial;
      // Space below the document, so that the menu can scroll into view.
      // WebKit does not scroll into a bottom margin: use the padding.
      let extraSpace = 0;
      const basePadding = parseFloat(
        getComputedStyle(editor.view.dom).paddingBottom,
      );

      const props: AiMenuProps = {
        mode: empty ? "cursor" : "selection",
        context: buildContext(editor, range),
        initialRun: initial,
        renderMarkdown: (markdown, target) =>
          renderMarkdown(editor, markdown, target),
        onPreview: (markdown) => {
          preview(markdown);
          layoutChanged();
        },
        onApply: (markdown, action) => {
          closePopover(false);
          applyMarkdown(editor, range, markdown, action);
        },
        onClose: () => closePopover(true),
        onLayoutChange: () => layoutChanged(),
        onStreamingChange: (value) => {
          streaming = value;
          // A new answer: follow it again.
          if (value) follow = true;
        },
      };
      const component = new ReactRenderer(AiMenu, { props, editor });

      // The end of the target: the bottom of the streamed answer, or of the
      // top-level block at the end of the range when it shows (a block that
      // the answer replaces is hidden). As wide as the first block.
      const anchorRect = () => {
        const answer = editor.view.dom.querySelector(".ai-insertion");
        // The last position in the range: a range that ends at the start of
        // a block ends in the block before.
        const end = to > from ? to - 1 : to;
        const ends = [answer, topBlockDom(editor, end)]
          .filter(isRendered)
          .map((element) => element.getBoundingClientRect().bottom);
        const first = topBlockDom(editor, from) ?? editor.view.dom;
        const { left, width: blockWidth } = first.getBoundingClientRect();
        const bottom = ends.length
          ? Math.max(...ends)
          : editor.view.coordsAtPos(to).bottom;
        const width = Math.min(
          Math.max(blockWidth, MIN_MENU_WIDTH),
          window.innerWidth - 16,
        );
        component.element.style.width = `${width}px`;
        return new DOMRect(left, bottom, blockWidth, 0);
      };

      // Scrolls the editor so that the AI menu fits below the target. It
      // uses the target position, not the menu position: the menu moves
      // only after the scroll.
      const reveal = () => {
        if (!follow || !scroller) return;
        const viewBottom = Math.min(
          scroller.getBoundingClientRect().bottom,
          window.innerHeight,
        );
        const menuBottom =
          anchorRect().bottom + MENU_OFFSET + menuExtent(component.element);
        const overflow = menuBottom + SCROLL_MARGIN - viewBottom;
        if (overflow <= 0) return;
        const room =
          scroller.scrollHeight - scroller.clientHeight - scroller.scrollTop;
        if (room < overflow) {
          extraSpace += Math.ceil(overflow - room);
          editor.view.dom.style.paddingBottom = `${basePadding + extraSpace}px`;
        }
        // While the AI writes, each change scrolls a small step: no animation.
        const smooth = !streaming && !prefersReducedMotion();
        scroller.scrollTo({
          top: scroller.scrollTop + overflow,
          behavior: smooth ? "smooth" : "instant",
        });
      };

      const layoutChanged = () => {
        popupRef.current?.popperInstance?.update();
        reveal();
      };

      // A scroll by the user (wheel, touch, scroll bar) stops the follow.
      const stopFollowing = () => {
        follow = false;
      };
      const onScrollerPointerDown = (e: PointerEvent) => {
        if (e.target === scroller) stopFollowing();
      };
      scroller?.addEventListener("wheel", stopFollowing, { passive: true });
      scroller?.addEventListener("touchmove", stopFollowing, { passive: true });
      scroller?.addEventListener("pointerdown", onScrollerPointerDown);

      popupRef.current = tippy(document.body, {
        getReferenceClientRect: anchorRect,
        appendTo: () => document.body,
        content: component.element,
        showOnCreate: true,
        interactive: true,
        trigger: "manual",
        hideOnClick: false,
        placement: "bottom-start",
        offset: [0, MENU_OFFSET],
        maxWidth: "none",
        popperOptions: {
          // Always below the target: never above it, and never moved up
          // over it. `reveal` scrolls it into view instead.
          modifiers: [
            { name: "flip", enabled: false },
            {
              name: "preventOverflow",
              options: { altAxis: false, padding: 8 },
            },
          ],
        },
        // The tone dropdown is in a portal: it is part of the AI menu.
        onClickOutside: (_instance, event) => {
          if (!isInMenuLayer(event.target as Node)) closePopover(false);
        },
        onDestroy: () => component.destroy(),
      });
      setPopoverOpen(true);

      // The stored range is only valid for this document: close on any change
      // (external reload, note switch, undo from a menu).
      const onTransaction = ({ transaction }: { transaction: Transaction }) => {
        if (transaction.docChanged) closePopover(false);
      };
      editor.on("transaction", onTransaction);
      cleanupRef.current = () => {
        editor.off("transaction", onTransaction);
        scroller?.removeEventListener("wheel", stopFollowing);
        scroller?.removeEventListener("touchmove", stopFollowing);
        scroller?.removeEventListener("pointerdown", onScrollerPointerDown);
        if (extraSpace) editor.view.dom.style.paddingBottom = "";
      };
    },
    [editor, closePopover],
  );

  const openPopover = useCallback(() => open(), [open]);

  // Close the AI menu (and stop the AI tool) when the editor goes away.
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

  // Keep the AI menu at its block while the note scrolls.
  useEffect(() => {
    const onScroll = () => popupRef.current?.popperInstance?.update();
    document.addEventListener("scroll", onScroll, true);
    return () => document.removeEventListener("scroll", onScroll, true);
  }, []);

  return { openPopover, runAi: open, popoverOpen };
}

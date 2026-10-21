import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type ComponentType,
  type KeyboardEvent as ReactKeyboardEvent,
  type ReactNode,
} from "react";
import type { ChainedCommands, Editor as TiptapEditor } from "@tiptap/core";
import { TextSelection, type EditorState } from "@tiptap/pm/state";
import { useEditorState } from "@tiptap/react";
import { BubbleMenu } from "@tiptap/react/menus";
import { Tooltip } from "../ui";
import {
  ArrowLeftIcon,
  BoldIcon,
  CheckIcon,
  CheckSquareIcon,
  ChevronRightIcon,
  CodeIcon,
  Heading1Icon,
  Heading2Icon,
  Heading3Icon,
  Heading4Icon,
  InfoIcon,
  InlineCodeIcon,
  ItalicIcon,
  LanguagesIcon,
  LinkIcon,
  ListIcon,
  ListOrderedIcon,
  MinusIcon,
  PencilIcon,
  PilcrowIcon,
  PlusIcon,
  QuoteIcon,
  RemoveFormattingIcon,
  SparklesIcon,
  StrikethroughIcon,
} from "../icons";
import { isMac, mod, shift } from "../../lib/platform";
import { cn } from "../../lib/utils";
import { setAiHighlight, type AiRun } from "./AiAssist";

type Icon = ComponentType<{ className?: string }>;

interface BlockType {
  label: string;
  icon: Icon;
  isActive: (editor: TiptapEditor) => boolean;
  /** A list or quote: it contains other blocks, so it is the current type. */
  wraps?: boolean;
  /** Turns the selected blocks into this type, after `clearNodes`. */
  apply: (chain: ChainedCommands) => ChainedCommands;
}

const heading = (level: 1 | 2 | 3 | 4, icon: Icon): BlockType => ({
  label: `Heading ${level}`,
  icon,
  isActive: (editor) => editor.isActive("heading", { level }),
  apply: (chain) => chain.setHeading({ level }),
});

/** The block types of the slash command, without the inserted blocks. */
const BLOCK_TYPES: BlockType[] = [
  {
    label: "Text",
    icon: PilcrowIcon,
    isActive: (editor) => editor.isActive("paragraph"),
    apply: (chain) => chain,
  },
  heading(1, Heading1Icon),
  heading(2, Heading2Icon),
  heading(3, Heading3Icon),
  heading(4, Heading4Icon),
  {
    label: "Bullet list",
    icon: ListIcon,
    isActive: (editor) => editor.isActive("bulletList"),
    wraps: true,
    apply: (chain) => chain.toggleBulletList(),
  },
  {
    label: "Numbered list",
    icon: ListOrderedIcon,
    isActive: (editor) => editor.isActive("orderedList"),
    wraps: true,
    apply: (chain) => chain.toggleOrderedList(),
  },
  {
    label: "Task list",
    icon: CheckSquareIcon,
    isActive: (editor) => editor.isActive("taskList"),
    wraps: true,
    apply: (chain) => chain.toggleTaskList(),
  },
  {
    label: "Quote",
    icon: QuoteIcon,
    isActive: (editor) => editor.isActive("blockquote"),
    wraps: true,
    apply: (chain) => chain.toggleBlockquote(),
  },
  {
    label: "Code block",
    icon: CodeIcon,
    isActive: (editor) => editor.isActive("codeBlock"),
    apply: (chain) => chain.toggleCodeBlock(),
  },
];

function currentBlockType(editor: TiptapEditor): BlockType {
  return (
    BLOCK_TYPES.find((type) => type.wraps && type.isActive(editor)) ??
    BLOCK_TYPES.find((type) => type.isActive(editor)) ??
    BLOCK_TYPES[0]
  );
}

const sep = isMac ? "" : "+";

interface Mark {
  name: string;
  label: string;
  shortcut: string;
  icon: Icon;
  toggle: (chain: ChainedCommands) => ChainedCommands;
}

const MARKS: Mark[] = [
  {
    name: "bold",
    label: "Bold",
    shortcut: `${mod}${sep}B`,
    icon: BoldIcon,
    toggle: (chain) => chain.toggleBold(),
  },
  {
    name: "italic",
    label: "Italic",
    shortcut: `${mod}${sep}I`,
    icon: ItalicIcon,
    toggle: (chain) => chain.toggleItalic(),
  },
  {
    name: "strike",
    label: "Strikethrough",
    shortcut: `${mod}${sep}${shift}${sep}S`,
    icon: StrikethroughIcon,
    toggle: (chain) => chain.toggleStrike(),
  },
  {
    name: "code",
    label: "Inline code",
    shortcut: `${mod}${sep}E`,
    icon: InlineCodeIcon,
    toggle: (chain) => chain.toggleCode(),
  },
];

interface Skill {
  label: string;
  icon: Icon;
  run: AiRun | null;
}

const SKILLS: Skill[] = [
  {
    label: "Improve writing",
    icon: PencilIcon,
    run: {
      instruction:
        "Improve the writing: make it clearer and easier to read. Keep the meaning, the language and the markdown formatting.",
    },
  },
  {
    label: "Proofread",
    icon: CheckIcon,
    run: {
      instruction:
        "Correct the spelling, grammar and punctuation. Do not change anything else.",
    },
  },
  {
    label: "Make shorter",
    icon: MinusIcon,
    run: {
      instruction:
        "Make the text shorter. Keep the important points and the language.",
    },
  },
  {
    label: "Make longer",
    icon: PlusIcon,
    run: {
      instruction:
        "Make the text longer with more detail. Keep the tone and the language.",
    },
  },
  {
    label: "Summarize",
    icon: ListIcon,
    run: {
      instruction: "Summarize the text. Use the language of the text.",
    },
  },
  {
    label: "Explain",
    icon: InfoIcon,
    run: {
      instruction:
        "Explain the text in simple words. Use the language of the text.",
      readOnly: true,
    },
  },
  // Fills the input; the user types the language.
  { label: "Translate…", icon: LanguagesIcon, run: null },
];

const TRANSLATE_PREFIX = "Translate to ";

/** The selection menu shows only for text that the user can edit. */
function hasEditableTextSelection(
  editor: TiptapEditor,
  state: EditorState,
): boolean {
  const { selection } = state;
  if (!editor.isEditable || !(selection instanceof TextSelection)) return false;
  if (selection.empty) return false;
  if (!state.doc.textBetween(selection.from, selection.to).trim()) return false;
  // Code blocks and the frontmatter are code nodes.
  return (
    !selection.$from.parent.type.spec.code &&
    !selection.$to.parent.type.spec.code
  );
}

/** Arrow keys, Home and End move the focus between the buttons of a list. */
function moveFocus(e: ReactKeyboardEvent<HTMLElement>) {
  const items = Array.from(
    e.currentTarget.querySelectorAll<HTMLButtonElement>("button"),
  );
  const index = items.indexOf(document.activeElement as HTMLButtonElement);
  const next =
    e.key === "ArrowDown"
      ? (index + 1) % items.length
      : e.key === "ArrowUp"
        ? (index - 1 + items.length) % items.length
        : e.key === "Home"
          ? 0
          : e.key === "End"
            ? items.length - 1
            : -1;
  if (next < 0) return;
  e.preventDefault();
  items[next]?.focus();
}

const ROW =
  "flex w-full items-center gap-2 h-8 px-2 rounded text-sm text-text text-left cursor-pointer transition-colors hover:bg-bg-muted focus-visible:outline-2 focus-visible:outline-accent";
const ROW_ICON = "w-4 h-4 stroke-[1.5] text-text-muted shrink-0";

function IconButton({
  label,
  active = false,
  onClick,
  children,
}: {
  label: string;
  active?: boolean;
  onClick: () => void;
  children: ReactNode;
}) {
  return (
    <Tooltip content={label}>
      <button
        type="button"
        aria-label={label}
        aria-pressed={active}
        onClick={onClick}
        className={cn(
          "h-7 w-7 flex items-center justify-center rounded transition-colors cursor-pointer focus-visible:outline-2 focus-visible:outline-accent",
          active
            ? "bg-bg-muted text-text"
            : "text-text-muted hover:bg-bg-muted hover:text-text",
        )}
      >
        {children}
      </button>
    </Tooltip>
  );
}

interface SelectionMenuProps {
  editor: TiptapEditor;
  /** The AI popover is open: the menu hides. */
  aiOpen: boolean;
  onAddLink: () => void;
  onRunAi: (run: AiRun) => void;
}

/**
 * Menu on a text selection: block type, marks, AI skills and an "Edit with
 * AI" input. The buttons keep the editor focus and selection.
 */
export function SelectionMenu({
  editor,
  aiOpen,
  onAddLink,
  onRunAi,
}: SelectionMenuProps) {
  const [dragging, setDragging] = useState(false);
  const [dismissed, setDismissed] = useState(false);
  const [view, setView] = useState<"main" | "blocks">("main");
  const [prompt, setPrompt] = useState("");
  const menuRef = useRef<HTMLDivElement>(null);
  const panelRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  // Focus the first item of the next view (keyboard use).
  const focusViewRef = useRef(false);
  // The AI popover takes the highlight: the input blur must not clear it.
  const runningRef = useRef(false);

  const visible = !dragging && !dismissed && !aiOpen;

  const active = useEditorState({
    editor,
    selector: ({ editor }) => ({
      blockType: currentBlockType(editor),
      marks: MARKS.map((mark) => editor.isActive(mark.name)),
      link: editor.isActive("link"),
    }),
  });

  const shouldShow = useCallback(
    ({
      editor,
      view,
      state,
      element,
    }: {
      editor: TiptapEditor;
      view: TiptapEditor["view"];
      state: EditorState;
      element: HTMLElement;
    }) =>
      (view.hasFocus() || element.contains(document.activeElement)) &&
      hasEditableTextSelection(editor, state),
    [],
  );

  const options = useMemo(
    () => ({
      placement: "bottom-start" as const,
      offset: 8,
      flip: { fallbackPlacements: ["top-start" as const] },
      shift: { padding: 8 },
      size: {
        padding: 8,
        apply: ({
          availableHeight,
          elements,
        }: {
          availableHeight: number;
          elements: { floating: HTMLElement };
        }) => {
          // The panel is not a direct child: pass the height as a variable.
          elements.floating.style.setProperty(
            "--selection-menu-max-height",
            `${Math.max(availableHeight, 160)}px`,
          );
        },
      },
      // The plugin element only positions the panel: keep it out of the tab
      // order (the plugin sets tabIndex 0) and above the editor content.
      onShow: () => {
        const element = menuRef.current;
        if (!element) return;
        element.tabIndex = -1;
        element.style.zIndex = "20";
      },
    }),
    [],
  );

  const updatePosition = useCallback(() => {
    if (editor.isDestroyed) return;
    editor.view.dispatch(
      editor.state.tr.setMeta("bubbleMenu", "updatePosition"),
    );
  }, [editor]);

  // A new selection shows the menu again, with the main view.
  useEffect(() => {
    const reset = () => {
      setDismissed(false);
      setView("main");
      setPrompt("");
    };
    editor.on("selectionUpdate", reset);
    return () => {
      editor.off("selectionUpdate", reset);
    };
  }, [editor]);

  // Hide while the mouse selects; show on mouse-up.
  useEffect(() => {
    const onPointerDown = (e: PointerEvent) => {
      if (editor.view.dom.contains(e.target as Node)) setDragging(true);
    };
    const onPointerUp = () => setDragging(false);
    document.addEventListener("pointerdown", onPointerDown, true);
    document.addEventListener("pointerup", onPointerUp, true);
    return () => {
      document.removeEventListener("pointerdown", onPointerDown, true);
      document.removeEventListener("pointerup", onPointerUp, true);
    };
  }, [editor]);

  // Hide when the focus goes to another part of the app.
  useEffect(() => {
    const onBlur = ({ event }: { event: FocusEvent }) => {
      // Switching to another app keeps the menu.
      if (!document.hasFocus()) return;
      const target = event.relatedTarget as Node | null;
      if (!menuRef.current?.contains(target)) setDismissed(true);
    };
    editor.on("blur", onBlur);
    return () => {
      editor.off("blur", onBlur);
    };
  }, [editor]);

  // The size changes (block list, new content): place the menu again.
  useLayoutEffect(() => {
    if (visible) updatePosition();
  }, [visible, view, updatePosition]);

  useLayoutEffect(() => {
    if (!focusViewRef.current) return;
    focusViewRef.current = false;
    panelRef.current?.querySelector<HTMLButtonElement>("button")?.focus();
  }, [view]);

  const changeView = useCallback((next: "main" | "blocks") => {
    focusViewRef.current = !!panelRef.current?.contains(document.activeElement);
    setView(next);
  }, []);

  const close = useCallback(() => {
    setDismissed(true);
    if (!editor.view.hasFocus()) editor.commands.focus();
  }, [editor]);

  // Esc closes the block list, then the menu.
  useEffect(() => {
    if (!visible) return;
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      const inMenu = !!panelRef.current?.contains(document.activeElement);
      if (!inMenu && !editor.view.hasFocus()) return;
      e.preventDefault();
      e.stopPropagation();
      if (view === "blocks") changeView("main");
      else close();
    };
    document.addEventListener("keydown", onKeyDown, true);
    return () => document.removeEventListener("keydown", onKeyDown, true);
  }, [visible, view, editor, changeView, close]);

  const run = useCallback(
    (aiRun: AiRun) => {
      runningRef.current = true;
      setDismissed(true);
      onRunAi(aiRun);
    },
    [onRunAi],
  );

  const chooseSkill = useCallback(
    (skill: Skill) => {
      if (skill.run) {
        run(skill.run);
        return;
      }
      setPrompt(TRANSLATE_PREFIX);
      requestAnimationFrame(() => {
        const input = inputRef.current;
        input?.focus();
        input?.setSelectionRange(TRANSLATE_PREFIX.length, TRANSLATE_PREFIX.length);
      });
    },
    [run],
  );

  const setBlockType = useCallback(
    (type: BlockType) => {
      if (type !== currentBlockType(editor)) {
        type.apply(editor.chain().focus().clearNodes()).run();
      }
      changeView("main");
    },
    [editor, changeView],
  );

  const handleInputKeyDown = (e: ReactKeyboardEvent<HTMLInputElement>) => {
    if (e.key === "Enter" && !e.nativeEvent.isComposing) {
      e.preventDefault();
      const instruction = prompt.trim();
      if (instruction && instruction !== TRANSLATE_PREFIX.trim()) {
        run({ instruction });
      }
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      const skills = panelRef.current?.querySelectorAll<HTMLButtonElement>(
        "[data-skill]",
      );
      skills?.[skills.length - 1]?.focus();
    }
  };

  const BlockIcon = active.blockType.icon;

  return (
    <BubbleMenu
      ref={menuRef}
      editor={editor}
      shouldShow={shouldShow}
      options={options}
    >
      {visible && (
        <div
          ref={panelRef}
          role="dialog"
          aria-label="Selection menu"
          // Keep the editor focus and selection; only the input takes focus.
          onMouseDown={(e) => {
            if (e.target !== inputRef.current) e.preventDefault();
          }}
          className="w-60 max-h-(--selection-menu-max-height) overflow-y-auto overscroll-contain bg-bg border border-border rounded-lg shadow-md p-1"
        >
          {view === "blocks" ? (
            <>
              <button
                type="button"
                onClick={() => changeView("main")}
                className={cn(ROW, "text-text-muted")}
              >
                <ArrowLeftIcon className={ROW_ICON} />
                Turn into
              </button>
              <div
                role="menu"
                aria-label="Turn into"
                onKeyDown={moveFocus}
                className="flex flex-col"
              >
                {BLOCK_TYPES.map((type) => {
                  const checked = type === active.blockType;
                  return (
                    <button
                      key={type.label}
                      type="button"
                      role="menuitemradio"
                      aria-checked={checked}
                      onClick={() => setBlockType(type)}
                      className={ROW}
                    >
                      <type.icon className={ROW_ICON} />
                      <span className="flex-1">{type.label}</span>
                      {checked && (
                        <CheckIcon className="w-4 h-4 stroke-[1.5] text-text" />
                      )}
                    </button>
                  );
                })}
              </div>
            </>
          ) : (
            <>
              <button
                type="button"
                aria-haspopup="menu"
                onClick={() => changeView("blocks")}
                onKeyDown={(e) => {
                  if (e.key === "ArrowRight") {
                    e.preventDefault();
                    changeView("blocks");
                  }
                }}
                className={ROW}
              >
                <BlockIcon className={ROW_ICON} />
                <span className="flex-1">
                  <span className="sr-only">Turn into, current type: </span>
                  {active.blockType.label}
                </span>
                <ChevronRightIcon className="w-4 h-4 stroke-[1.5] text-text-muted" />
              </button>

              <div
                role="group"
                aria-label="Format"
                className="flex items-center gap-0.5 px-1 py-0.5"
              >
                {MARKS.map((mark, index) => (
                  <IconButton
                    key={mark.name}
                    label={`${mark.label} (${mark.shortcut})`}
                    active={active.marks[index]}
                    onClick={() => mark.toggle(editor.chain().focus()).run()}
                  >
                    <mark.icon className="w-4 h-4 stroke-[1.5]" />
                  </IconButton>
                ))}
                <IconButton
                  label={`Link (${mod}${sep}K)`}
                  active={active.link}
                  onClick={onAddLink}
                >
                  <LinkIcon className="w-4 h-4 stroke-[1.5]" />
                </IconButton>
                <IconButton
                  label="Clear formatting"
                  onClick={() => editor.chain().focus().unsetAllMarks().run()}
                >
                  <RemoveFormattingIcon className="w-4 h-4 stroke-[1.5]" />
                </IconButton>
              </div>

              <div className="border-t border-border my-1" />

              <p
                id="selection-menu-skills"
                className="px-2 pt-1 pb-0.5 text-xs font-medium text-text-muted"
              >
                Skills
              </p>
              <div
                role="group"
                aria-labelledby="selection-menu-skills"
                onKeyDown={moveFocus}
                className="flex flex-col"
              >
                {SKILLS.map((skill) => (
                  <button
                    key={skill.label}
                    type="button"
                    data-skill
                    onClick={() => chooseSkill(skill)}
                    className={ROW}
                  >
                    <skill.icon className={ROW_ICON} />
                    {skill.label}
                  </button>
                ))}
              </div>

              <div className="mt-1 flex items-center gap-2 h-8 px-2 rounded border border-border focus-within:border-accent">
                <label htmlFor="selection-menu-input" className="text-text-muted">
                  <SparklesIcon className="w-4 h-4 stroke-[1.5]" />
                  <span className="sr-only">Edit with AI</span>
                </label>
                <input
                  id="selection-menu-input"
                  ref={inputRef}
                  value={prompt}
                  onChange={(e) => setPrompt(e.target.value)}
                  onKeyDown={handleInputKeyDown}
                  onFocus={() => {
                    runningRef.current = false;
                    const { from, to } = editor.state.selection;
                    setAiHighlight(editor, { from, to });
                  }}
                  onBlur={() => {
                    if (!runningRef.current) setAiHighlight(editor, null);
                  }}
                  placeholder="Edit with AI…"
                  autoComplete="off"
                  spellCheck={false}
                  className="flex-1 min-w-0 bg-transparent text-sm text-text placeholder:text-text-muted outline-none"
                />
              </div>
            </>
          )}
        </div>
      )}
    </BubbleMenu>
  );
}

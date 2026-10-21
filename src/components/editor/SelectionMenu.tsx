import {
  Fragment,
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
import * as DropdownMenu from "@radix-ui/react-dropdown-menu";
import { TooltipContent, TooltipRoot, TooltipTrigger } from "../ui";
import {
  BoldIcon,
  CheckSquareIcon,
  ChevronDownIcon,
  CodeIcon,
  Heading1Icon,
  Heading2Icon,
  Heading3Icon,
  Heading4Icon,
  IndentDecreaseIcon,
  IndentIncreaseIcon,
  InlineCodeIcon,
  ItalicIcon,
  LinkIcon,
  ListIcon,
  ListOrderedIcon,
  MoreVerticalIcon,
  QuoteIcon,
  RemoveFormattingIcon,
  SparklesIcon,
  StrikethroughIcon,
  TypeIcon,
} from "../icons";
import { isMac, mod, shift } from "../../lib/platform";
import { cn } from "../../lib/utils";
import { setAiHighlight, type AiRun } from "./AiAssist";
import { IMPROVE_GROUPS, type AiSkill } from "./aiSkills";
import {
  isInMenuLayer,
  MENU_BUTTON,
  MENU_BUTTON_TEXT,
  MENU_ICON,
  MenuContent,
  MenuItem,
  MenuLabel,
  MenuRadioItem,
  MenuSeparator,
  MenuSubContent,
  MenuSubTrigger,
  TOOLBAR_BOX,
} from "./menuParts";

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
    icon: TypeIcon,
    isActive: (editor) => editor.isActive("paragraph"),
    apply: (chain) => chain,
  },
  heading(1, Heading1Icon),
  heading(2, Heading2Icon),
  heading(3, Heading3Icon),
  heading(4, Heading4Icon),
  {
    label: "Bulleted list",
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
    label: "To-do list",
    icon: CheckSquareIcon,
    isActive: (editor) => editor.isActive("taskList"),
    wraps: true,
    apply: (chain) => chain.toggleTaskList(),
  },
  {
    label: "Blockquote",
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
    label: "Strike",
    shortcut: `${mod}${sep}${shift}${sep}S`,
    icon: StrikethroughIcon,
    toggle: (chain) => chain.toggleStrike(),
  },
  {
    name: "code",
    label: "Code",
    shortcut: `${mod}${sep}E`,
    icon: InlineCodeIcon,
    toggle: (chain) => chain.toggleCode(),
  },
];

/** The list item type at the selection, for the indent buttons. */
const listItemType = (editor: TiptapEditor) =>
  editor.isActive("taskItem") ? "taskItem" : "listItem";

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

const SEPARATOR = <div role="none" className="w-px h-6 shrink-0 bg-bg-emphasis" />;

function Tip({ label, children }: { label: string; children: ReactNode }) {
  return (
    <TooltipRoot delayDuration={300}>
      <TooltipTrigger asChild>{children}</TooltipTrigger>
      <TooltipContent
        side="top"
        sideOffset={6}
        className="mx-0 rounded-lg px-2 py-1.5 text-2xs leading-[18px] font-medium bg-text text-bg backdrop-blur-none shadow-[0_4px_6px_-1px_rgba(0,0,0,0.1)]"
      >
        {label}
      </TooltipContent>
    </TooltipRoot>
  );
}

type OpenMenu = "improve" | "blocks" | "more" | null;

interface SelectionMenuProps {
  editor: TiptapEditor;
  /** The AI menu is open: the toolbar hides. */
  aiOpen: boolean;
  onAddLink: () => void;
  /** Opens the AI menu with its prompt input. */
  onAskAi: () => void;
  onRunAi: (run: AiRun) => void;
}

/**
 * Floating toolbar on a text selection: AI ("Improve"), block type, marks,
 * link and more. The buttons keep the editor selection; the dropdowns show
 * it as a highlight while they have the focus. The dropdowns render in the
 * toolbar element, so that the bubble menu plugin sees the focus in them.
 */
export function SelectionMenu({
  editor,
  aiOpen,
  onAddLink,
  onAskAi,
  onRunAi,
}: SelectionMenuProps) {
  const [dragging, setDragging] = useState(false);
  const [dismissed, setDismissed] = useState(false);
  const [openMenu, setOpenMenu] = useState<OpenMenu>(null);
  const [focusIndex, setFocusIndex] = useState(0);
  const menuRef = useRef<HTMLDivElement>(null);
  const toolbarRef = useRef<HTMLDivElement>(null);
  // The AI menu takes the highlight: closing a dropdown must not clear it.
  const runningRef = useRef(false);
  // The dropdown was opened from the keyboard: give the focus back to its button.
  const keyboardOpenRef = useRef(false);
  // The toolbar shows the selection as a highlight while a dropdown is open.
  const highlightRef = useRef(false);

  const visible = !dragging && !dismissed && !aiOpen;

  const active = useEditorState({
    editor,
    selector: ({ editor }) => {
      const item = listItemType(editor);
      return {
        blockType: currentBlockType(editor),
        marks: MARKS.map((mark) => editor.isActive(mark.name)),
        link: editor.isActive("link"),
        canLift: editor.can().liftListItem(item),
        canSink: editor.can().sinkListItem(item),
      };
    },
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
      (view.hasFocus() ||
        element.contains(document.activeElement) ||
        isInMenuLayer(document.activeElement)) &&
      hasEditableTextSelection(editor, state),
    [],
  );

  const options = useMemo(
    () => ({
      placement: "top" as const,
      offset: 4,
      flip: { fallbackPlacements: ["bottom" as const] },
      shift: { padding: 8 },
      // The plugin element only positions the toolbar: keep it out of the tab
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

  // The plugin places the element before React renders the toolbar into it:
  // place it again with its size.
  useLayoutEffect(() => {
    if (!visible || editor.isDestroyed) return;
    editor.view.dispatch(editor.state.tr.setMeta("bubbleMenu", "updatePosition"));
  }, [visible, editor]);

  // A new selection shows the toolbar again.
  useEffect(() => {
    const reset = () => setDismissed(false);
    editor.on("selectionUpdate", reset);
    return () => {
      editor.off("selectionUpdate", reset);
    };
  }, [editor]);

  // The AI menu closed without a change (Discard, Stop): show the toolbar again.
  useEffect(() => {
    if (aiOpen) return;
    runningRef.current = false;
    setDismissed(false);
  }, [aiOpen]);

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
      // Switching to another app keeps the toolbar.
      if (!document.hasFocus()) return;
      const target = event.relatedTarget as Node | null;
      if (!menuRef.current?.contains(target) && !isInMenuLayer(target)) {
        setDismissed(true);
      }
    };
    editor.on("blur", onBlur);
    return () => {
      editor.off("blur", onBlur);
    };
  }, [editor]);

  const close = useCallback(() => {
    setDismissed(true);
    if (!editor.view.hasFocus()) editor.commands.focus();
  }, [editor]);

  // Esc closes the toolbar (a dropdown closes itself first). Alt+F10 moves
  // the focus from the editor into the toolbar.
  useEffect(() => {
    if (!visible || openMenu) return;
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === "F10" && e.altKey && editor.view.hasFocus()) {
        e.preventDefault();
        toolbarRef.current
          ?.querySelector<HTMLButtonElement>("[data-toolbar-item][tabindex='0']")
          ?.focus();
        return;
      }
      if (e.key !== "Escape") return;
      const inMenu = !!toolbarRef.current?.contains(document.activeElement);
      if (!inMenu && !editor.view.hasFocus()) return;
      e.preventDefault();
      e.stopPropagation();
      close();
    };
    document.addEventListener("keydown", onKeyDown, true);
    return () => document.removeEventListener("keydown", onKeyDown, true);
  }, [visible, openMenu, editor, close]);

  // The focus leaves the editor for a dropdown: keep the selection visible
  // until the editor has the focus again. WebKit keeps the DOM selection only
  // when the editor takes the focus before the highlight goes away.
  useEffect(() => {
    const onFocus = () => {
      if (!highlightRef.current) return;
      highlightRef.current = false;
      setAiHighlight(editor, null);
    };
    editor.on("focus", onFocus);
    return () => {
      editor.off("focus", onFocus);
    };
  }, [editor]);

  const changeMenu = useCallback(
    (menu: Exclude<OpenMenu, null>, open: boolean) => {
      setOpenMenu(open ? menu : null);
      if (open) {
        const { from, to } = editor.state.selection;
        highlightRef.current = true;
        setAiHighlight(editor, { from, to });
      } else if (runningRef.current) {
        // The AI menu owns the highlight now.
        highlightRef.current = false;
      } else if (!keyboardOpenRef.current) {
        editor.view.focus();
      }
    },
    [editor],
  );

  const onCloseAutoFocus = useCallback(
    (e: Event) => {
      // From the keyboard, the focus goes back to the dropdown button.
      if (keyboardOpenRef.current) {
        keyboardOpenRef.current = false;
        return;
      }
      e.preventDefault();
      if (!runningRef.current) editor.view.focus();
    },
    [editor],
  );

  const run = useCallback(
    (aiRun: AiRun) => {
      runningRef.current = true;
      keyboardOpenRef.current = false;
      setDismissed(true);
      onRunAi(aiRun);
    },
    [onRunAi],
  );

  const askAi = useCallback(() => {
    runningRef.current = true;
    keyboardOpenRef.current = false;
    setDismissed(true);
    onAskAi();
  }, [onAskAi]);

  const setBlockType = useCallback(
    (label: string) => {
      const type = BLOCK_TYPES.find((t) => t.label === label);
      if (type && type !== currentBlockType(editor)) {
        type.apply(editor.chain().focus().clearNodes()).run();
      }
    },
    [editor],
  );

  // Roving focus: the toolbar is one tab stop; arrow keys move in it.
  const handleToolbarKeyDown = (e: ReactKeyboardEvent<HTMLDivElement>) => {
    const items = Array.from(
      toolbarRef.current?.querySelectorAll<HTMLButtonElement>(
        "[data-toolbar-item]",
      ) ?? [],
    );
    const index = items.indexOf(document.activeElement as HTMLButtonElement);
    if (index < 0) return;
    const next =
      e.key === "ArrowRight"
        ? (index + 1) % items.length
        : e.key === "ArrowLeft"
          ? (index - 1 + items.length) % items.length
          : e.key === "Home"
            ? 0
            : e.key === "End"
              ? items.length - 1
              : -1;
    if (next < 0) {
      if (["Enter", " ", "ArrowDown"].includes(e.key)) {
        keyboardOpenRef.current = true;
      }
      return;
    }
    e.preventDefault();
    items[next]?.focus();
  };

  let itemIndex = 0;
  /** Props of one toolbar item for the roving focus. */
  const item = () => {
    const index = itemIndex++;
    return {
      "data-toolbar-item": "",
      tabIndex: index === focusIndex ? 0 : -1,
      onFocus: () => setFocusIndex(index),
    };
  };

  const renderSkill = (skill: AiSkill) =>
    skill.items ? (
      <DropdownMenu.Sub key={skill.label}>
        <MenuSubTrigger icon={skill.icon} label={skill.label} />
        <MenuSubContent container={menuRef.current}>
          {skill.items.map((sub) => (
            <MenuItem
              key={sub.label}
              label={sub.label}
              onSelect={() => sub.run && run(sub.run)}
            />
          ))}
        </MenuSubContent>
      </DropdownMenu.Sub>
    ) : (
      <MenuItem
        key={skill.label}
        icon={skill.icon}
        label={skill.label}
        onSelect={() => skill.run && run(skill.run)}
      />
    );

  return (
    <BubbleMenu
      ref={menuRef}
      editor={editor}
      shouldShow={shouldShow}
      options={options}
    >
      {visible && (
        <div
          ref={toolbarRef}
          role="toolbar"
          aria-label="Selection menu"
          onKeyDown={handleToolbarKeyDown}
          // Keep the editor focus and selection.
          onMouseDown={(e) => e.preventDefault()}
          className={cn(
            TOOLBAR_BOX,
            "animate-toolbar-in motion-reduce:animate-none",
          )}
        >
          <DropdownMenu.Root
            modal={false}
            open={openMenu === "improve"}
            onOpenChange={(open) => changeMenu("improve", open)}
          >
            <DropdownMenu.Trigger asChild>
              <button
                type="button"
                aria-label="Improve"
                className={cn(MENU_BUTTON, "gap-1")}
                {...item()}
              >
                <SparklesIcon
                  className={cn(
                    MENU_ICON,
                    openMenu === "improve" && "text-ai",
                  )}
                />
                <span className={MENU_BUTTON_TEXT}>Improve</span>
              </button>
            </DropdownMenu.Trigger>
            <MenuContent
              container={menuRef.current}
              align="start"
              aria-label="Improve"
              onCloseAutoFocus={onCloseAutoFocus}
              className="w-max"
            >
              {IMPROVE_GROUPS.map((group, index) => (
                <Fragment key={index}>
                  {index > 0 && <MenuSeparator />}
                  <DropdownMenu.Group className="flex flex-col">
                    {group.map((skill) =>
                      skill ? (
                        renderSkill(skill)
                      ) : (
                        <MenuItem
                          key="ask"
                          icon={SparklesIcon}
                          label="Ask AI"
                          onSelect={askAi}
                        />
                      ),
                    )}
                  </DropdownMenu.Group>
                </Fragment>
              ))}
            </MenuContent>
          </DropdownMenu.Root>

          {SEPARATOR}

          <DropdownMenu.Root
            modal={false}
            open={openMenu === "blocks"}
            onOpenChange={(open) => changeMenu("blocks", open)}
          >
            <DropdownMenu.Trigger asChild>
              <button
                type="button"
                aria-label={`Turn into (current: ${active.blockType.label})`}
                className={cn(MENU_BUTTON, "gap-1")}
                {...item()}
              >
                <span className={MENU_BUTTON_TEXT}>
                  {active.blockType.label}
                </span>
                <ChevronDownIcon className="w-2.5 h-2.5 shrink-0 stroke-[3]" />
              </button>
            </DropdownMenu.Trigger>
            <MenuContent
              container={menuRef.current}
              align="start"
              aria-label="Turn into"
              onCloseAutoFocus={onCloseAutoFocus}
              className="w-max min-w-36"
            >
              <MenuLabel>Turn into</MenuLabel>
              <DropdownMenu.RadioGroup
                value={active.blockType.label}
                onValueChange={setBlockType}
                className="flex flex-col"
              >
                {BLOCK_TYPES.map((type) => (
                  <MenuRadioItem
                    key={type.label}
                    value={type.label}
                    icon={type.icon}
                    label={type.label}
                  />
                ))}
              </DropdownMenu.RadioGroup>
            </MenuContent>
          </DropdownMenu.Root>

          {SEPARATOR}

          <div role="group" aria-label="Format" className="flex gap-0.5">
            {MARKS.map((mark, index) => (
              <Tip key={mark.name} label={`${mark.label} ${mark.shortcut}`}>
                <button
                  type="button"
                  aria-label={mark.label}
                  aria-keyshortcuts={mark.shortcut}
                  aria-pressed={active.marks[index]}
                  onClick={() => mark.toggle(editor.chain().focus()).run()}
                  className={MENU_BUTTON}
                  {...item()}
                >
                  <mark.icon className={MENU_ICON} />
                </button>
              </Tip>
            ))}
          </div>

          {SEPARATOR}

          <Tip label={`Link ${mod}${sep}K`}>
            <button
              type="button"
              aria-label="Link"
              aria-pressed={active.link}
              onClick={onAddLink}
              className={MENU_BUTTON}
              {...item()}
            >
              <LinkIcon className={MENU_ICON} />
            </button>
          </Tip>

          {SEPARATOR}

          <DropdownMenu.Root
            modal={false}
            open={openMenu === "more"}
            onOpenChange={(open) => changeMenu("more", open)}
          >
            <Tip label="More options">
              <DropdownMenu.Trigger asChild>
                <button
                  type="button"
                  aria-label="More options"
                  className={MENU_BUTTON}
                  {...item()}
                >
                  <MoreVerticalIcon
                    className={cn(MENU_ICON, openMenu === "more" && "text-ai")}
                  />
                </button>
              </DropdownMenu.Trigger>
            </Tip>
            <MenuContent
              container={menuRef.current}
              side="top"
              align="end"
              aria-label="More options"
              onCloseAutoFocus={onCloseAutoFocus}
              className={cn(TOOLBAR_BOX, "flex-row p-[3px] rounded-[15px]")}
            >
              {(
                [
                  {
                    label: "Decrease indent",
                    icon: IndentDecreaseIcon,
                    disabled: !active.canLift,
                    action: () =>
                      editor.chain().liftListItem(listItemType(editor)).run(),
                  },
                  {
                    label: "Increase indent",
                    icon: IndentIncreaseIcon,
                    disabled: !active.canSink,
                    action: () =>
                      editor.chain().sinkListItem(listItemType(editor)).run(),
                  },
                  null,
                  {
                    label: "Clear formatting",
                    icon: RemoveFormattingIcon,
                    disabled: false,
                    action: () => editor.chain().unsetAllMarks().run(),
                  },
                ] as const
              ).map((entry) =>
                entry ? (
                  <DropdownMenu.Item
                    key={entry.label}
                    aria-label={entry.label}
                    disabled={entry.disabled}
                    // Keep the menu open, like a toolbar.
                    onSelect={(e) => {
                      e.preventDefault();
                      entry.action();
                    }}
                    className={cn(
                      MENU_BUTTON,
                      "data-highlighted:bg-menu-hover data-highlighted:text-text data-disabled:text-text/40 data-disabled:bg-transparent data-disabled:cursor-default",
                    )}
                  >
                    <entry.icon className={MENU_ICON} />
                  </DropdownMenu.Item>
                ) : (
                  <Fragment key="sep">{SEPARATOR}</Fragment>
                ),
              )}
            </MenuContent>
          </DropdownMenu.Root>
        </div>
      )}
    </BubbleMenu>
  );
}

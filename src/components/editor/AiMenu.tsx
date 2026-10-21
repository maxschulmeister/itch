import {
  memo,
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent,
} from "react";
import { invoke } from "@tauri-apps/api/core";
import * as DropdownMenu from "@radix-ui/react-dropdown-menu";
import {
  ArrowUpIcon,
  CheckIcon,
  ChevronRightIcon,
  CircleStopIcon,
  CopyIcon,
  MicVocalIcon,
  RotateCcwIcon,
  SparklesIcon,
  XIcon,
} from "../icons";
import {
  abortPi,
  closePiSession,
  fitThinkingLevel,
  promptPi,
  PI_THINKING_LEVELS,
  type PiPromptRequest,
} from "../../services/pi";
import {
  findPiModel,
  PiModelOptions,
  usePiModels,
} from "../ai/PiModelOptions";
import type { Settings } from "../../types/note";
import { cn } from "../../lib/utils";
import type { AiRun } from "./AiAssist";
import {
  CURSOR_SKILL_GROUPS,
  SELECTION_SKILL_GROUPS,
  TONES,
  toneInstruction,
  type AiSkill,
} from "./aiSkills";
import {
  CARD_BOX,
  MENU_BUTTON,
  MENU_BUTTON_PRIMARY,
  MENU_BUTTON_TEXT,
  MENU_ICON,
  MenuContent,
  MenuRadioItem,
} from "./menuParts";

export type AiMenuMode = "selection" | "cursor";
export type AiApplyAction = "replace" | "below" | "insert";
export type AiContext = Pick<PiPromptRequest, "before" | "after" | "selection">;

const ALL_THINKING_LEVELS = PI_THINKING_LEVELS.map((l) => l.value).filter(
  Boolean,
);

/** Saves one AI setting, so that the next AI menus start with it. */
function saveSetting(key: "piModel" | "piThinking", value: string) {
  invoke<Settings>("get_settings")
    .then((settings) =>
      invoke("update_settings", {
        newSettings: { ...settings, [key]: value || undefined },
      }),
    )
    .catch((err) => console.error(`Failed to save ${key}:`, err));
}

export interface AiMenuProps {
  mode: AiMenuMode;
  context: AiContext;
  /** Sent when the settings are loaded, without user input. */
  initialRun?: AiRun;
  /** Renders markdown into `target` with the editor schema. */
  renderMarkdown: (markdown: string, target: HTMLElement) => void;
  /** Shows the answer in the document (null: the original text). */
  onPreview: (markdown: string | null) => void;
  onApply: (markdown: string, action: AiApplyAction) => void;
  onClose: () => void;
  /** Called when the menu size changes, so the position can update. */
  onLayoutChange: () => void;
}

type Status = "idle" | "streaming" | "done" | "error";

const SELECT_WRAPPER = cn(
  "relative flex items-center h-8 px-2 gap-1 rounded-xl text-sm font-medium text-menu-fg",
  "transition-colors duration-200 ease-menu hover:bg-menu-hover",
  "has-focus-visible:outline-solid has-focus-visible:outline-2 has-focus-visible:-outline-offset-2 has-focus-visible:outline-ai",
);
// The select covers its button; the button shows the value at its own width.
const SELECT =
  "absolute inset-0 w-full opacity-0 cursor-pointer [&_option]:bg-bg [&_option]:text-text";

const LIST_BOX = cn(
  "absolute w-52 p-1.5 rounded-[19px] overflow-y-auto overscroll-contain",
  CARD_BOX,
  "animate-fade-in motion-reduce:animate-none",
);
const LIST_ITEM = cn(
  "flex w-full items-center h-8 p-2 gap-1 rounded-xl cursor-pointer select-none",
  "text-sm font-medium text-menu-fg transition-[background-color,color] duration-200 ease-menu",
  "aria-selected:bg-menu-hover aria-selected:text-text",
);
const LIST_LABEL =
  "mt-3 mx-2 mb-1 text-2xs leading-[15px] font-semibold text-text";

interface ListGroup {
  label: string;
  skills: AiSkill[];
}

const optionId = (list: string, index: number) => `ai-menu-${list}-${index}`;

export const AiMenu = memo(function AiMenu({
  mode,
  context,
  initialRun,
  renderMarkdown,
  onPreview,
  onApply,
  onClose,
  onLayoutChange,
}: AiMenuProps) {
  const [sessionId] = useState(() => crypto.randomUUID());
  const [prompt, setPrompt] = useState("");
  // A skill run shows the progress at once, while the settings load.
  const [status, setStatus] = useState<Status>(
    initialRun ? "streaming" : "idle",
  );
  const [text, setText] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [lastRun, setLastRun] = useState<AiRun | null>(null);
  const [tone, setTone] = useState("");
  const [thinking, setThinking] = useState("");
  const [model, setModel] = useState("");
  const [settingsLoaded, setSettingsLoaded] = useState(false);
  const [expanded, setExpanded] = useState(!initialRun);
  const [inputFocused, setInputFocused] = useState(false);
  const [activeIndex, setActiveIndex] = useState(0);
  const [subSkill, setSubSkill] = useState<AiSkill | null>(null);
  const [subIndex, setSubIndex] = useState(0);
  const [listPlacement, setListPlacement] = useState({
    above: false,
    maxHeight: 400,
  });
  const [copyState, setCopyState] = useState<"idle" | "copied" | "failed">(
    "idle",
  );
  const [models] = usePiModels();
  const runIdRef = useRef(0);
  const initialSentRef = useRef(false);
  const rootRef = useRef<HTMLDivElement>(null);
  const cardRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLTextAreaElement>(null);
  const answerRef = useRef<HTMLDivElement>(null);

  const busy = status === "streaming";
  const readOnly = !!lastRun?.readOnly;
  const hasResult = status === "done" && text.trim().length > 0;
  // Without the model list (pi default, loading, error) pi fits the level.
  const supportedLevels =
    findPiModel(models, model)?.thinkingLevels ?? ALL_THINKING_LEVELS;
  const effectiveThinking = fitThinkingLevel(thinking, supportedLevels);

  // The suggestion list: the skills whose label contains the prompt.
  const groups = useMemo<ListGroup[]>(() => {
    const query = prompt.trim().toLowerCase();
    const all = mode === "selection" ? SELECTION_SKILL_GROUPS : CURSOR_SKILL_GROUPS;
    return all
      .map((group) => ({
        label: group.label,
        skills: group.skills.filter(
          (skill) => !query || skill.label.toLowerCase().includes(query),
        ),
      }))
      .filter((group) => group.skills.length > 0);
  }, [prompt, mode]);
  const flatSkills = useMemo(() => groups.flatMap((g) => g.skills), [groups]);
  const listOpen =
    expanded &&
    inputFocused &&
    !busy &&
    flatSkills.length > 0;
  const active = flatSkills[Math.min(activeIndex, flatSkills.length - 1)];
  const subItems = listOpen && subSkill ? (subSkill.items ?? []) : null;

  // One pi process per AI menu: stop it when the menu closes.
  useEffect(() => {
    return () => {
      runIdRef.current += 1;
      closePiSession(sessionId).catch((err) =>
        console.error("Failed to close pi session:", err),
      );
    };
  }, [sessionId]);

  // The prompt input takes the focus; a skill run focuses the menu, so that
  // Esc works and keys do not go to the document.
  useEffect(() => {
    const id = requestAnimationFrame(() =>
      (initialRun ? rootRef.current : inputRef.current)?.focus(),
    );
    return () => cancelAnimationFrame(id);
  }, [initialRun]);

  // Start with the model and thinking level from the settings.
  useEffect(() => {
    invoke<Settings>("get_settings")
      .then((settings) => {
        setModel(settings.piModel ?? "");
        setThinking(settings.piThinking ?? "");
      })
      .catch((err) => console.error("Failed to load the AI settings:", err))
      .finally(() => setSettingsLoaded(true));
  }, []);

  const changeModel = useCallback((value: string) => {
    setModel(value);
    saveSetting("piModel", value);
  }, []);

  const changeThinking = useCallback((level: string) => {
    setThinking(level);
    saveSetting("piThinking", level);
  }, []);

  // Show the answer in the document (edits) or in the menu (read-only),
  // at most once per frame while it streams. The previous answer stays until
  // the next one starts.
  useEffect(() => {
    if (!text && busy) return;
    const id = requestAnimationFrame(() => {
      if (readOnly) {
        if (answerRef.current) renderMarkdown(text, answerRef.current);
      } else {
        onPreview(status === "error" ? null : text);
      }
    });
    return () => cancelAnimationFrame(id);
  }, [text, busy, status, readOnly, renderMarkdown, onPreview]);

  useLayoutEffect(() => {
    const root = rootRef.current;
    if (!root) return;
    const observer = new ResizeObserver(() => onLayoutChange());
    observer.observe(root);
    return () => observer.disconnect();
  }, [onLayoutChange]);

  // Grow the prompt input with its content (Shift+Enter adds lines).
  useLayoutEffect(() => {
    const input = inputRef.current;
    if (!input) return;
    input.style.height = "auto";
    // scrollHeight is 0 before the menu is in the document.
    if (input.scrollHeight > 0) {
      input.style.height = `${input.scrollHeight}px`;
    }
  }, [prompt, expanded, status]);

  // The suggestion list goes below the card, or above it without room.
  useLayoutEffect(() => {
    if (!listOpen) return;
    const card = cardRef.current?.getBoundingClientRect();
    if (!card) return;
    const below = window.innerHeight - card.bottom - 14;
    const above = card.top - 14;
    const placeAbove = below < 200 && above > below;
    setListPlacement({
      above: placeAbove,
      maxHeight: Math.max(placeAbove ? above : below, 120),
    });
  }, [listOpen, expanded]);

  const run = useCallback(
    (aiRun: AiRun) => {
      const runId = ++runIdRef.current;
      setLastRun(aiRun);
      setText("");
      setError(null);
      setStatus("streaming");
      setPrompt("");
      setExpanded(false);
      setSubSkill(null);
      setActiveIndex(0);
      // The input goes away while pi writes: keep the focus in the menu.
      rootRef.current?.focus();
      if (aiRun.readOnly) onPreview(null);
      promptPi(
        sessionId,
        {
          instruction: aiRun.instruction,
          ...context,
          thinking: effectiveThinking || undefined,
          model: model || undefined,
          readOnly: !!aiRun.readOnly && mode === "selection",
        },
        (event) => {
          if (runIdRef.current !== runId) return;
          switch (event.type) {
            case "delta":
              setText((prev) => prev + event.text);
              break;
            case "done":
              setText((prev) => event.text || prev);
              setStatus("done");
              break;
            case "error":
              setError(event.message);
              setStatus("error");
              break;
          }
        },
      ).catch((err) => {
        if (runIdRef.current !== runId) return;
        setError(String(err));
        setStatus("error");
      });
    },
    [sessionId, context, effectiveThinking, model, mode, onPreview],
  );

  // A skill from the selection toolbar runs at once, with the saved settings.
  useEffect(() => {
    if (!settingsLoaded || !initialRun || initialSentRef.current) return;
    initialSentRef.current = true;
    run(initialRun);
  }, [settingsLoaded, initialRun, run]);

  const withTone = useCallback(
    (aiRun: AiRun): AiRun =>
      tone
        ? { ...aiRun, instruction: `${aiRun.instruction} ${toneInstruction(tone)}` }
        : aiRun,
    [tone],
  );

  const submit = useCallback(() => {
    const instruction = prompt.trim();
    if (!instruction || busy) return;
    // A follow-up keeps the kind of the conversation (edit or read).
    run(withTone({ instruction, readOnly: lastRun?.readOnly }));
  }, [prompt, busy, run, withTone, lastRun]);

  const chooseSkill = useCallback(
    (skill: AiSkill) => {
      if (skill.items) {
        setSubSkill(skill);
        setSubIndex(0);
      } else if (skill.run) {
        run(withTone(skill.run));
      }
    },
    [run, withTone],
  );

  const stop = useCallback(() => {
    abortPi(sessionId).catch((err) =>
      console.error("Failed to stop pi:", err),
    );
    onClose();
  }, [sessionId, onClose]);

  const copy = useCallback(() => {
    invoke("copy_to_clipboard", { text })
      .then(() => setCopyState("copied"))
      .catch((err) => {
        console.error("Failed to copy the AI answer:", err);
        setCopyState("failed");
      })
      .finally(() => setTimeout(() => setCopyState("idle"), 1500));
  }, [text]);

  const handleKeyDown = useCallback(
    (e: KeyboardEvent<HTMLDivElement>) => {
      // Keep keys away from the global app and editor shortcuts.
      e.stopPropagation();
      if (e.key !== "Escape" || e.defaultPrevented) return;
      // Keys of the tone dropdown (a portal) also come here.
      if (!rootRef.current?.contains(e.target as Node)) return;
      e.preventDefault();
      if (busy) stop();
      else onClose();
    },
    [busy, stop, onClose],
  );

  const handleInputKeyDown = (e: KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.nativeEvent.isComposing) return;
    const moves: Record<string, number> = { ArrowDown: 1, ArrowUp: -1 };
    if (listOpen && e.key in moves) {
      e.preventDefault();
      if (subItems) {
        setSubIndex((i) => (i + moves[e.key] + subItems.length) % subItems.length);
      } else {
        setActiveIndex(
          (i) => (i + moves[e.key] + flatSkills.length) % flatSkills.length,
        );
      }
    } else if (listOpen && e.key === "ArrowRight" && !subItems && active?.items) {
      e.preventDefault();
      chooseSkill(active);
    } else if (listOpen && e.key === "ArrowLeft" && subItems) {
      e.preventDefault();
      setSubSkill(null);
    } else if (e.key === "Escape" && listOpen && subItems) {
      e.preventDefault();
      setSubSkill(null);
    } else if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      const item = subItems ? subItems[subIndex] : listOpen ? active : null;
      if (item) chooseSkill(item);
      else submit();
    }
  };

  const statusMessage =
    status === "streaming"
      ? "AI is writing"
      : status === "done"
        ? "Response ready"
        : "";

  const placeholder =
    status === "done" || status === "error"
      ? readOnly
        ? "Ask a follow-up question..."
        : "Tell AI what else needs to be changed..."
      : "Ask AI what you want...";

  const renderOption = (
    skill: AiSkill,
    index: number,
    list: "main" | "sub",
  ) => {
    const selected =
      list === "sub" ? index === subIndex : !subItems && skill === active;
    return (
      <div
        key={skill.label}
        id={optionId(list, index)}
        role="option"
        aria-selected={selected}
        aria-haspopup={skill.items ? "listbox" : undefined}
        aria-expanded={skill.items ? subSkill === skill : undefined}
        onMouseEnter={() => {
          if (list === "sub") {
            setSubIndex(index);
            return;
          }
          setActiveIndex(flatSkills.indexOf(skill));
          setSubSkill(skill.items ? skill : null);
        }}
        onClick={() => chooseSkill(skill)}
        className={cn(LIST_ITEM, skill === subSkill && "bg-menu-hover text-text")}
      >
        {skill.icon && <skill.icon className={MENU_ICON} />}
        <span className={cn(MENU_BUTTON_TEXT, "flex-1 truncate")}>
          {skill.label}
        </span>
        {skill.items && <ChevronRightIcon className="w-3.5 h-3.5 shrink-0" />}
      </div>
    );
  };

  const listStyle = {
    maxHeight: listPlacement.maxHeight,
  };
  const listPosition = listPlacement.above ? "bottom-full mb-1.5" : "top-full mt-1.5";

  let optionIndex = 0;

  return (
    <div
      ref={rootRef}
      role="dialog"
      aria-label="Ask AI"
      tabIndex={-1}
      onKeyDown={handleKeyDown}
      // Clicks keep the focus in the prompt input and the document selection
      // (WebKit moves it to the click point).
      onMouseDown={(e) => {
        if (!(e.target as HTMLElement).closest("textarea, select, .prose")) {
          e.preventDefault();
        }
      }}
      className="relative w-full outline-none animate-fade-in motion-reduce:animate-none"
    >
      <div
        ref={cardRef}
        className={cn(CARD_BOX, "flex flex-col rounded-[18px] overflow-hidden")}
      >
        {readOnly && text && (
          <div className="max-h-80 overflow-y-auto overscroll-contain px-3.5 pt-3 pb-2 border-b border-border">
            <div
              ref={answerRef}
              aria-busy={busy}
              className="prose dark:prose-invert max-w-none text-sm [&_a]:pointer-events-none [&>*:first-child]:mt-0 [&>*:last-child]:mb-0"
            />
          </div>
        )}

        {busy && (
          <div className="flex items-center justify-between p-1.5">
            <div className="flex items-center gap-3 p-2 text-2xs leading-[15px] font-semibold text-ai">
              <span>AI is writing</span>
              <div aria-hidden="true" className="flex items-center gap-[5px] h-3">
                <span className="ai-dot" />
                <span className="ai-dot" />
                <span className="ai-dot" />
              </div>
            </div>
            <button
              type="button"
              title="Stop"
              aria-label="Stop"
              onClick={stop}
              className={MENU_BUTTON}
            >
              <CircleStopIcon className={MENU_ICON} />
            </button>
          </div>
        )}

        {status === "error" && error && (
          <p
            id="ai-menu-error"
            role="alert"
            className="mx-1.5 mt-1.5 rounded-xl bg-red-500/10 px-3 py-2 text-sm text-red-500 whitespace-pre-wrap break-words"
          >
            {error}
          </p>
        )}

        {status === "done" && !text.trim() && (
          <p className="px-3.5 pt-3 pb-1 text-sm text-text-muted">
            pi returned no text.
          </p>
        )}

        {!busy && (
          <div
            className={cn(
              "flex gap-1.5 p-1.5 rounded-xl border bg-bg-elevated",
              expanded ? "flex-col" : "items-center",
              expanded && inputFocused ? "border-ai" : "border-border",
            )}
          >
            <div
              className={cn(
                "flex flex-1 min-w-0",
                !expanded && "items-center gap-2 px-2 py-1.5",
              )}
            >
              {!expanded && (
                <SparklesIcon className={cn(MENU_ICON, "text-text-muted")} />
              )}
              <label htmlFor="ai-menu-input" className="sr-only">
                Ask AI
              </label>
              <textarea
                id="ai-menu-input"
                ref={inputRef}
                rows={1}
                value={prompt}
                onChange={(e) => {
                  setPrompt(e.target.value);
                  setActiveIndex(0);
                  setSubSkill(null);
                }}
                onKeyDown={handleInputKeyDown}
                onFocus={() => {
                  setInputFocused(true);
                  setExpanded(true);
                }}
                onBlur={() => setInputFocused(false)}
                role="combobox"
                aria-autocomplete="list"
                aria-expanded={listOpen}
                aria-controls={listOpen ? "ai-menu-list" : undefined}
                aria-activedescendant={
                  !listOpen
                    ? undefined
                    : subItems
                      ? optionId("sub", subIndex)
                      : optionId("main", flatSkills.indexOf(active))
                }
                aria-describedby={error ? "ai-menu-error" : undefined}
                placeholder={placeholder}
                autoComplete="off"
                spellCheck={false}
                className={cn(
                  "flex-1 min-w-0 resize-none max-h-32 bg-transparent text-sm text-text placeholder:text-text-muted outline-none",
                  expanded
                    ? "px-2 py-1.5 leading-[21px]"
                    : "leading-[17px] placeholder:font-medium",
                )}
              />
            </div>
            <div className="flex flex-wrap items-center gap-1">
              {expanded && (
                <>
                  <DropdownMenu.Root modal={false}>
                    <DropdownMenu.Trigger asChild>
                      <button
                        type="button"
                        aria-label="Tone adjustment options"
                        aria-pressed={!!tone}
                        className={cn(MENU_BUTTON, "gap-1")}
                      >
                        <MicVocalIcon className={MENU_ICON} />
                        <span className={MENU_BUTTON_TEXT}>{tone || "Tone"}</span>
                      </button>
                    </DropdownMenu.Trigger>
                    <MenuContent
                      align="start"
                      aria-label="Tone"
                      className="w-max"
                      onCloseAutoFocus={(e) => {
                        e.preventDefault();
                        inputRef.current?.focus();
                      }}
                    >
                      <DropdownMenu.RadioGroup value={tone} onValueChange={setTone}>
                        <MenuRadioItem value="" label="Default tone" />
                        {TONES.map((name) => (
                          <MenuRadioItem key={name} value={name} label={name} />
                        ))}
                      </DropdownMenu.RadioGroup>
                    </MenuContent>
                  </DropdownMenu.Root>
                  <div className={SELECT_WRAPPER}>
                    <label htmlFor="ai-menu-model">Model</label>
                    <span aria-hidden="true" className="max-w-44 truncate text-text">
                      {findPiModel(models, model)?.name || model || "Default (pi)"}
                    </span>
                    <select
                      id="ai-menu-model"
                      value={model}
                      onChange={(e) => changeModel(e.target.value)}
                      aria-describedby={
                        models.status === "error" ? "ai-menu-models-error" : undefined
                      }
                      className={SELECT}
                    >
                      <PiModelOptions state={models} value={model} />
                    </select>
                  </div>
                  <div className={SELECT_WRAPPER}>
                    <label htmlFor="ai-menu-thinking">Thinking</label>
                    <span aria-hidden="true" className="text-text">
                      {PI_THINKING_LEVELS.find((l) => l.value === effectiveThinking)
                        ?.label ?? "Default"}
                    </span>
                    <select
                      id="ai-menu-thinking"
                      value={effectiveThinking}
                      onChange={(e) => changeThinking(e.target.value)}
                      className={SELECT}
                    >
                      {PI_THINKING_LEVELS.filter(
                        (level) =>
                          !level.value || supportedLevels.includes(level.value),
                      ).map((level) => (
                        <option key={level.value} value={level.value}>
                          {level.label}
                        </option>
                      ))}
                    </select>
                  </div>
                  <div className="flex-1" />
                </>
              )}
              <button
                type="button"
                aria-label="Submit prompt"
                onClick={submit}
                disabled={!prompt.trim()}
                className={MENU_BUTTON_PRIMARY}
              >
                <ArrowUpIcon className={MENU_ICON} />
              </button>
            </div>
            {expanded && models.status === "error" && (
              <p
                id="ai-menu-models-error"
                className="px-2 pb-1 text-2xs text-red-500 break-words"
              >
                {models.message}
              </p>
            )}
          </div>
        )}

        {(status === "done" || status === "error") && (
          <div className="flex flex-wrap items-center gap-2 px-2 py-1.5">
            {lastRun && (
              <button
                type="button"
                onClick={() => run(lastRun)}
                className={cn(MENU_BUTTON, "gap-0.5")}
              >
                <RotateCcwIcon className={MENU_ICON} />
                <span className={MENU_BUTTON_TEXT}>Try again</span>
              </button>
            )}
            <div className="flex-1" />
            <div className="flex items-center gap-0.5">
              <button
                type="button"
                onClick={onClose}
                className={cn(MENU_BUTTON, "gap-0.5")}
              >
                <XIcon className={MENU_ICON} />
                <span className={MENU_BUTTON_TEXT}>Discard</span>
              </button>
              {hasResult && readOnly && (
                <>
                  <button
                    type="button"
                    onClick={() => onApply(text, "below")}
                    className={cn(MENU_BUTTON, "gap-0.5")}
                  >
                    <span className={MENU_BUTTON_TEXT}>Insert below</span>
                  </button>
                  <button
                    type="button"
                    onClick={copy}
                    className={cn(MENU_BUTTON_PRIMARY, "gap-0.5")}
                  >
                    <CopyIcon className={MENU_ICON} />
                    <span className={MENU_BUTTON_TEXT}>
                      {copyState === "copied"
                        ? "Copied"
                        : copyState === "failed"
                          ? "Unable to copy"
                          : "Copy"}
                    </span>
                  </button>
                </>
              )}
              {hasResult && !readOnly && (
                <button
                  type="button"
                  onClick={() =>
                    onApply(text, mode === "cursor" ? "insert" : "replace")
                  }
                  className={cn(MENU_BUTTON_PRIMARY, "gap-0.5")}
                >
                  <CheckIcon className={MENU_ICON} />
                  <span className={MENU_BUTTON_TEXT}>Apply</span>
                </button>
              )}
            </div>
          </div>
        )}
      </div>

      {listOpen && (
        <div
          id="ai-menu-list"
          role="listbox"
          aria-label="Suggestions"
          style={listStyle}
          className={cn(LIST_BOX, "left-0", listPosition)}
        >
          {groups.map((group, index) => (
            <div
              key={group.label}
              role="group"
              aria-labelledby={`ai-menu-group-${index}`}
              className={cn(
                "flex flex-col gap-px",
                index > 0 && "mt-2 pt-2 border-t border-bg-emphasis",
              )}
            >
              <div
                id={`ai-menu-group-${index}`}
                aria-hidden="true"
                className={LIST_LABEL}
              >
                {group.label}
              </div>
              {group.skills.map((skill) => renderOption(skill, optionIndex++, "main"))}
            </div>
          ))}
        </div>
      )}

      {subItems && subSkill && (
        <div
          role="listbox"
          aria-label={subSkill.label}
          style={listStyle}
          className={cn(LIST_BOX, "left-[calc(13rem-2px)]", listPosition)}
        >
          <div aria-hidden="true" className={LIST_LABEL}>
            {subSkill.label}
          </div>
          {subItems.map((skill, index) => renderOption(skill, index, "sub"))}
        </div>
      )}

      <p role="status" className="sr-only">
        {copyState === "copied" ? "Copied" : statusMessage}
      </p>
    </div>
  );
});

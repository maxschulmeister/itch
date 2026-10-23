import {
  memo,
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type KeyboardEvent,
} from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  ArrowUpIcon,
  CheckIcon,
  CircleStopIcon,
  CopyIcon,
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
  PiThinkingOptions,
  resolvePiDefaults,
  usePiModels,
} from "../ai/PiModelOptions";
import type { Settings } from "../../types/note";
import { cn } from "../../lib/utils";
import type { AiRun } from "./AiAssist";
import { CURSOR_SKILLS, type AiSkill } from "./aiSkills";
import { ESCAPE_LAYER, useEscapeLayer } from "./escapeLayers";
import {
  CARD_BOX,
  MENU_BUTTON,
  MENU_BUTTON_PRIMARY,
  MENU_BUTTON_TEXT,
  MENU_ICON,
} from "./menuParts";

export type AiMenuMode = "selection" | "cursor";
export type AiApplyAction = "replace" | "below" | "insert";
export type AiContext = Pick<PiPromptRequest, "before" | "after" | "selection">;

const ALL_THINKING_LEVELS = PI_THINKING_LEVELS.map((l) => l.value);

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
  /** Called when pi starts or stops writing an answer. */
  onStreamingChange: (streaming: boolean) => void;
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

const optionId = (index: number) => `ai-menu-option-${index}`;

export const AiMenu = memo(function AiMenu({
  mode,
  context,
  initialRun,
  renderMarkdown,
  onPreview,
  onApply,
  onClose,
  onLayoutChange,
  onStreamingChange,
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
  const [thinking, setThinking] = useState("");
  const [model, setModel] = useState("");
  // The default model and thinking level are set (from the settings and pi).
  const [defaultsSet, setDefaultsSet] = useState(false);
  const [settings, setSettings] = useState<Pick<
    Settings,
    "piModel" | "piThinking"
  > | null>(null);
  const [expanded, setExpanded] = useState(!initialRun);
  const [inputFocused, setInputFocused] = useState(false);
  const [activeIndex, setActiveIndex] = useState(0);
  const [copyState, setCopyState] = useState<"idle" | "copied" | "failed">(
    "idle",
  );
  const [models] = usePiModels();
  const runIdRef = useRef(0);
  const initialSentRef = useRef(false);
  const rootRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLTextAreaElement>(null);
  const answerRef = useRef<HTMLDivElement>(null);

  const busy = status === "streaming";
  const readOnly = !!lastRun?.readOnly;
  const hasResult = status === "done" && text.trim().length > 0;
  // Without the model list (loading, error) pi fits the level.
  const supportedLevels =
    findPiModel(models, model)?.thinkingLevels ?? ALL_THINKING_LEVELS;
  const effectiveThinking = thinking
    ? fitThinkingLevel(thinking, supportedLevels)
    : "";

  // At the cursor, a short list of skills whose label contains the prompt.
  // With a selection, the "Improve" dropdown has the skills.
  const query = prompt.trim().toLowerCase();
  const skills =
    mode === "cursor"
      ? CURSOR_SKILLS.filter(
          (skill) => !query || skill.label.toLowerCase().includes(query),
        )
      : [];
  const listOpen = expanded && inputFocused && !busy && skills.length > 0;
  const active = skills[Math.min(activeIndex, skills.length - 1)];

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
      // The editor scrolls the menu into view, not the focus.
      (initialRun ? rootRef.current : inputRef.current)?.focus({
        preventScroll: true,
      }),
    );
    return () => cancelAnimationFrame(id);
  }, [initialRun]);

  useEffect(() => {
    invoke<Settings>("get_settings")
      .then(setSettings)
      .catch((err) => {
        console.error("Failed to load the AI settings:", err);
        setSettings({});
      });
  }, []);

  // Start with the default model and thinking level of Itch. A change in
  // the menu is only for this conversation: the settings do not change.
  useEffect(() => {
    if (defaultsSet || !settings || models.status === "loading") return;
    const defaults =
      models.status === "ready"
        ? resolvePiDefaults(models, settings)
        : { model: settings.piModel ?? "", thinking: settings.piThinking ?? "" };
    setModel(defaults.model);
    setThinking(defaults.thinking);
    setDefaultsSet(true);
  }, [defaultsSet, settings, models]);

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

  useEffect(() => onStreamingChange(busy), [busy, onStreamingChange]);

  // The suggestion list is below the card, outside of its size.
  useLayoutEffect(() => {
    if (listOpen) onLayoutChange();
  }, [listOpen, onLayoutChange]);

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

  const run = useCallback(
    (aiRun: AiRun) => {
      const runId = ++runIdRef.current;
      setLastRun(aiRun);
      setText("");
      setError(null);
      setStatus("streaming");
      setPrompt("");
      setExpanded(false);
      setActiveIndex(0);
      // The input goes away while pi writes: keep the focus in the menu.
      rootRef.current?.focus({ preventScroll: true });
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

  // A skill from the selection toolbar runs at once, with the defaults.
  useEffect(() => {
    if (!defaultsSet || !initialRun || initialSentRef.current) return;
    initialSentRef.current = true;
    run(initialRun);
  }, [defaultsSet, initialRun, run]);

  const submit = useCallback(() => {
    const instruction = prompt.trim();
    if (!instruction || busy) return;
    // A follow-up keeps the kind of the conversation (edit or read).
    run({ instruction, readOnly: lastRun?.readOnly });
  }, [prompt, busy, run, lastRun]);

  const chooseSkill = useCallback(
    (skill: AiSkill) => {
      if (skill.run) run(skill.run);
    },
    [run],
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

  // Esc stops pi while it writes, else it discards the answer.
  useEscapeLayer(true, ESCAPE_LAYER.menu, busy ? stop : onClose);

  // Keep keys away from the global app and editor shortcuts.
  const handleKeyDown = (e: KeyboardEvent<HTMLDivElement>) => e.stopPropagation();

  const handleInputKeyDown = (e: KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.nativeEvent.isComposing) return;
    const moves: Record<string, number> = { ArrowDown: 1, ArrowUp: -1 };
    if (listOpen && e.key in moves) {
      e.preventDefault();
      setActiveIndex((i) => (i + moves[e.key] + skills.length) % skills.length);
    } else if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      if (listOpen && active) chooseSkill(active);
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
        className={cn(CARD_BOX, "flex flex-col rounded-[18px] overflow-hidden")}
      >
        {readOnly && text && (
          <div className="max-h-80 overflow-y-auto overscroll-contain px-3.5 pt-3 pb-2 border-b border-border">
            <div
              ref={answerRef}
              aria-busy={busy}
              className="prose dark:prose-invert max-w-none text-sm whitespace-break-spaces [&_a]:pointer-events-none [&>*:first-child]:mt-0 [&>*:last-child]:mb-0"
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
              // Concentric with the card: 18px radius minus its 1px border.
              "flex gap-1.5 p-1.5 rounded-[17px] border border-border bg-bg-elevated",
              expanded ? "flex-col" : "items-center",
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
                }}
                onKeyDown={handleInputKeyDown}
                onFocus={() => {
                  setInputFocused(true);
                  setExpanded(true);
                }}
                onBlur={() => setInputFocused(false)}
                role={mode === "cursor" ? "combobox" : undefined}
                aria-autocomplete={mode === "cursor" ? "list" : undefined}
                aria-expanded={mode === "cursor" ? listOpen : undefined}
                aria-controls={listOpen ? "ai-menu-list" : undefined}
                aria-activedescendant={
                  listOpen ? optionId(skills.indexOf(active)) : undefined
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
                  <div className={SELECT_WRAPPER}>
                    <label htmlFor="ai-menu-model">Model</label>
                    <span aria-hidden="true" className="max-w-44 truncate text-text">
                      {findPiModel(models, model)?.name ??
                        (models.status === "loading" ? "Loading…" : model || "Unavailable")}
                    </span>
                    <select
                      id="ai-menu-model"
                      value={findPiModel(models, model) ? model : ""}
                      disabled={!findPiModel(models, model)}
                      onChange={(e) => setModel(e.target.value)}
                      aria-describedby={
                        models.status === "error" ? "ai-menu-models-error" : undefined
                      }
                      className={SELECT}
                    >
                      <PiModelOptions state={models} />
                    </select>
                  </div>
                  <div className={SELECT_WRAPPER}>
                    <label htmlFor="ai-menu-thinking">Thinking</label>
                    <span aria-hidden="true" className="text-text">
                      {PI_THINKING_LEVELS.find((l) => l.value === effectiveThinking)
                        ?.label ?? "…"}
                    </span>
                    <select
                      id="ai-menu-thinking"
                      value={effectiveThinking}
                      disabled={!findPiModel(models, model)}
                      onChange={(e) => setThinking(e.target.value)}
                      className={SELECT}
                    >
                      <PiThinkingOptions levels={supportedLevels} />
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
          className={cn(LIST_BOX, "top-full mt-1.5 left-0 flex flex-col gap-px")}
        >
          {skills.map((skill, index) => (
            <div
              key={skill.label}
              id={optionId(index)}
              role="option"
              aria-selected={skill === active}
              onMouseEnter={() => setActiveIndex(index)}
              onClick={() => chooseSkill(skill)}
              className={LIST_ITEM}
            >
              {skill.icon && <skill.icon className={MENU_ICON} />}
              <span className={cn(MENU_BUTTON_TEXT, "flex-1 truncate")}>
                {skill.label}
              </span>
            </div>
          ))}
        </div>
      )}

      <p role="status" className="sr-only">
        {copyState === "copied" ? "Copied" : statusMessage}
      </p>
    </div>
  );
});

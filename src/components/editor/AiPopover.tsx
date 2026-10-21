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
import { Button } from "../ui";
import { SparklesIcon, SpinnerIcon } from "../icons";
import {
  abortPi,
  closePiSession,
  promptPi,
  PI_THINKING_LEVELS,
  type PiPromptRequest,
} from "../../services/pi";
import type { Settings } from "../../types/note";

export type AiPopoverMode = "selection" | "cursor";
export type AiApplyAction = "replace" | "below" | "insert";
export type AiContext = Omit<PiPromptRequest, "instruction" | "thinking">;

export interface AiPopoverProps {
  mode: AiPopoverMode;
  context: AiContext;
  /** Renders markdown into `target` with the editor schema. */
  renderMarkdown: (markdown: string, target: HTMLElement) => void;
  onApply: (markdown: string, action: AiApplyAction) => void;
  onClose: () => void;
  /** Called when the popover size changes, so the position can update. */
  onLayoutChange: () => void;
}

type Status = "idle" | "streaming" | "stopping" | "done" | "error";

export const AiPopover = memo(function AiPopover({
  mode,
  context,
  renderMarkdown,
  onApply,
  onClose,
  onLayoutChange,
}: AiPopoverProps) {
  const [sessionId] = useState(() => crypto.randomUUID());
  const [prompt, setPrompt] = useState("");
  const [status, setStatus] = useState<Status>("idle");
  const [text, setText] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [aborted, setAborted] = useState(false);
  const [lastInstruction, setLastInstruction] = useState("");
  const [thinking, setThinking] = useState("");
  const runIdRef = useRef(0);
  const rootRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLTextAreaElement>(null);
  const previewRef = useRef<HTMLDivElement>(null);

  const busy = status === "streaming" || status === "stopping";
  const hasResult = status === "done" && text.trim().length > 0;

  // One pi process per popover: stop it when the popover closes.
  useEffect(() => {
    return () => {
      runIdRef.current += 1;
      closePiSession(sessionId).catch((err) =>
        console.error("Failed to close pi session:", err),
      );
    };
  }, [sessionId]);

  useEffect(() => {
    const id = requestAnimationFrame(() => inputRef.current?.focus());
    return () => cancelAnimationFrame(id);
  }, []);

  // Start with the thinking level from the settings.
  useEffect(() => {
    invoke<Settings>("get_settings")
      .then((settings) => setThinking(settings.piThinking ?? ""))
      .catch((err) => console.error("Failed to load the thinking level:", err));
  }, []);

  // Keep the chosen level as the start level for the next popovers.
  const changeThinking = useCallback((level: string) => {
    setThinking(level);
    invoke<Settings>("get_settings")
      .then((settings) =>
        invoke("update_settings", {
          newSettings: { ...settings, piThinking: level || undefined },
        }),
      )
      .catch((err) => console.error("Failed to save the thinking level:", err));
  }, []);

  // Render the markdown at most once per frame while it streams.
  useEffect(() => {
    const target = previewRef.current;
    if (!target) return;
    const id = requestAnimationFrame(() => renderMarkdown(text, target));
    return () => cancelAnimationFrame(id);
  }, [text, renderMarkdown]);

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
    // scrollHeight is 0 before the popover is in the document.
    if (input.scrollHeight > 0) {
      input.style.height = `${input.scrollHeight}px`;
    }
  }, [prompt]);

  const run = useCallback(
    (instruction: string) => {
      const runId = ++runIdRef.current;
      setLastInstruction(instruction);
      setText("");
      setError(null);
      setAborted(false);
      setStatus("streaming");
      // The clicked button (Send, Try again) goes away: keep focus in the
      // popover so Esc still stops the run.
      inputRef.current?.focus();
      promptPi(
        sessionId,
        { instruction, ...context, thinking: thinking || undefined },
        (event) => {
          if (runIdRef.current !== runId) return;
          switch (event.type) {
            case "delta":
              setText((prev) => prev + event.text);
              break;
            case "done":
              setText((prev) => event.text || prev);
              setAborted(event.aborted);
              setStatus("done");
              inputRef.current?.focus();
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
    [sessionId, context, thinking],
  );

  const submit = useCallback(() => {
    const instruction = prompt.trim();
    if (!instruction || busy) return;
    setPrompt("");
    run(instruction);
  }, [prompt, busy, run]);

  const stop = useCallback(() => {
    if (status !== "streaming") return;
    setStatus("stopping");
    abortPi(sessionId).catch((err) => {
      setError(String(err));
      setStatus("error");
    });
  }, [status, sessionId]);

  const handleKeyDown = useCallback(
    (e: KeyboardEvent<HTMLDivElement>) => {
      // Keep keys away from the global app and editor shortcuts.
      e.stopPropagation();
      if (e.key === "Escape") {
        e.preventDefault();
        if (busy) stop();
        else onClose();
      }
    },
    [busy, stop, onClose],
  );

  const handleInputKeyDown = useCallback(
    (e: KeyboardEvent<HTMLTextAreaElement>) => {
      if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
        e.preventDefault();
        submit();
      }
    },
    [submit],
  );

  const statusMessage =
    status === "streaming"
      ? "Writing…"
      : status === "stopping"
        ? "Stopping…"
        : status === "done"
          ? aborted
            ? "Stopped"
            : "Response ready"
          : "";

  return (
    <div
      ref={rootRef}
      role="dialog"
      aria-label="Ask AI"
      onKeyDown={handleKeyDown}
      className="w-[min(32rem,calc(100vw-2rem))] bg-bg border border-border rounded-lg shadow-md p-1.5 flex flex-col gap-1.5"
    >
      {(busy || status === "done") && (
        <div className="max-h-80 overflow-y-auto overscroll-contain rounded-md px-2.5 py-2">
          {text ? (
            <div
              ref={previewRef}
              aria-busy={busy}
              className="prose dark:prose-invert max-w-none text-sm [&_a]:pointer-events-none [&>*:first-child]:mt-0 [&>*:last-child]:mb-0"
            />
          ) : (
            <p className="text-sm text-text-muted flex items-center gap-2">
              {busy && (
                <SpinnerIcon className="w-3.5 h-3.5 animate-spin motion-reduce:animate-none" />
              )}
              {busy
                ? "Writing…"
                : aborted
                  ? "Stopped before any text."
                  : "pi returned no text."}
            </p>
          )}
        </div>
      )}

      {status === "error" && error && (
        <p
          id="ai-popover-error"
          className="text-sm text-red-500 bg-red-500/10 rounded-md px-2.5 py-2 whitespace-pre-wrap break-words"
        >
          {error}
        </p>
      )}

      <div className="flex items-start gap-2 rounded-md border border-border px-2.5 py-1.5 focus-within:border-accent">
        <label htmlFor="ai-popover-input" className="pt-1 text-text-muted">
          <SparklesIcon className="w-4 h-4 stroke-[1.5]" />
          <span className="sr-only">Ask AI</span>
        </label>
        <textarea
          id="ai-popover-input"
          ref={inputRef}
          rows={1}
          value={prompt}
          onChange={(e) => setPrompt(e.target.value)}
          onKeyDown={handleInputKeyDown}
          aria-describedby={error ? "ai-popover-error" : undefined}
          placeholder={
            hasResult
              ? "Tell AI what to change…"
              : mode === "selection"
                ? "Edit the selection, for example: make it shorter"
                : "Write with AI, for example: continue writing"
          }
          className="flex-1 min-w-0 resize-none max-h-32 bg-transparent text-sm text-text placeholder:text-text-muted outline-none py-0.5"
        />
        {busy ? (
          <Button
            type="button"
            variant="outline"
            size="xs"
            onClick={stop}
          >
            Stop
          </Button>
        ) : (
          <Button
            type="button"
            variant="primary"
            size="xs"
            onClick={submit}
            disabled={!prompt.trim()}
          >
            Send
          </Button>
        )}
      </div>

      <div className="flex items-center px-1 text-xs text-text-muted">
        <label htmlFor="ai-popover-thinking">Thinking</label>
        <select
          id="ai-popover-thinking"
          value={thinking}
          onChange={(e) => changeThinking(e.target.value)}
          disabled={busy}
          className="ml-1 bg-transparent text-text-muted hover:text-text rounded px-1 py-0.5 cursor-pointer outline-none focus-visible:ring-2 focus-visible:ring-accent disabled:cursor-not-allowed disabled:opacity-50 [&>option]:bg-bg [&>option]:text-text"
        >
          {PI_THINKING_LEVELS.map((level) => (
            <option key={level.value} value={level.value}>
              {level.label}
            </option>
          ))}
        </select>
      </div>

      {(status === "done" || status === "error") && (
        <div className="flex flex-wrap items-center gap-1">
          {hasResult && mode === "selection" && (
            <>
              <Button
                type="button"
                size="xs"
                variant="primary"
                onClick={() => onApply(text, "replace")}
              >
                Replace selection
              </Button>
              <Button
                type="button"
                size="xs"
                onClick={() => onApply(text, "below")}
              >
                Insert below
              </Button>
            </>
          )}
          {hasResult && mode === "cursor" && (
            <Button
              type="button"
              size="xs"
              variant="primary"
              onClick={() => onApply(text, "insert")}
            >
              Insert
            </Button>
          )}
          <Button
            type="button"
            size="xs"
            variant="ghost"
            onClick={() => run(lastInstruction)}
          >
            Try again
          </Button>
          <Button type="button" size="xs" variant="ghost" onClick={onClose}>
            Discard
          </Button>
        </div>
      )}

      <p role="status" className="sr-only">
        {statusMessage}
      </p>
    </div>
  );
});

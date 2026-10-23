import { Channel, invoke } from "@tauri-apps/api/core";

/** An AI tool that can run the inline AI menu. */
export type AiProvider = "pi" | "claude" | "codex" | "opencode" | "ollama";

/** All AI tools, in the order of the automatic choice. */
export const AI_PROVIDERS: ReadonlyArray<{ id: AiProvider; name: string }> = [
  { id: "pi", name: "pi" },
  { id: "claude", name: "Claude Code" },
  { id: "codex", name: "Codex" },
  { id: "opencode", name: "OpenCode" },
  { id: "ollama", name: "Ollama" },
];

export function providerName(provider: AiProvider): string {
  return AI_PROVIDERS.find((p) => p.id === provider)?.name ?? provider;
}

/** The installed AI tools, in the order of `AI_PROVIDERS`. */
export function listAiProviders(): Promise<AiProvider[]> {
  return invoke("ai_providers");
}

/**
 * The AI tool of the inline AI menu: the tool from the settings when it is
 * installed, else the first installed tool. Null when no tool is installed.
 */
export function resolveProvider(
  setting: string | undefined,
  installed: AiProvider[],
): AiProvider | null {
  return installed.find((p) => p === setting) ?? installed[0] ?? null;
}

/** Document context for the first prompt of a conversation. */
export interface AiPromptRequest {
  instruction: string;
  /** Document markdown before the selection or cursor. */
  before: string;
  /** Document markdown after the selection or cursor. */
  after: string;
  /** Selected text as markdown, or null in cursor mode. */
  selection: string | null;
  /** Thinking level for this prompt. Omit to keep the current level. */
  thinking?: string;
  /** Model id (`AiModel.id`). Omit to keep the current model. */
  model?: string;
  /** The answer is for reading. It does not replace the selection. */
  readOnly?: boolean;
}

/** A model that an AI tool can use. */
export interface AiModel {
  /** The value for `AiPromptRequest.model` and the model settings. */
  id: string;
  name: string;
  /** The group in the model select, for example the model provider. */
  group: string;
  /** Thinking levels that the model supports. Can be empty. */
  thinkingLevels: string[];
}

/** The models of an AI tool, and its start values if the tool tells. */
export interface AiModelList {
  models: AiModel[];
  /** The model that the tool uses when no model is set. */
  defaultModel: string | null;
  /** The thinking level that the tool uses when no level is set. */
  defaultThinking: string | null;
}

/**
 * The models of an AI tool. The backend asks the tool once per app session,
 * or again with `refresh`.
 */
export function listAiModels(
  provider: AiProvider,
  refresh = false,
): Promise<AiModelList> {
  return invoke("ai_list_models", { provider, refresh });
}

/** Known thinking levels, from lowest to highest, with their labels. */
const THINKING_LEVELS: ReadonlyArray<[string, string]> = [
  ["off", "Off"],
  ["none", "None"],
  ["on", "On"],
  ["minimal", "Minimal"],
  ["low", "Low"],
  ["medium", "Medium"],
  ["high", "High"],
  ["xhigh", "Extra high"],
  ["max", "Max"],
  ["ultra", "Ultra"],
];

export function thinkingLabel(level: string): string {
  return (
    THINKING_LEVELS.find(([value]) => value === level)?.[1] ??
    level.charAt(0).toUpperCase() + level.slice(1)
  );
}

/**
 * The supported level nearest to `level`: the next higher level, else the
 * next lower level (the same rule as pi). An empty string (the tool
 * default) when the level is not known or no level fits.
 */
export function fitThinkingLevel(level: string, supported: string[]): string {
  if (!level || supported.includes(level)) return level;
  const order = THINKING_LEVELS.map(([value]) => value);
  const index = order.indexOf(level);
  if (index < 0) return "";
  const higher = order.slice(index + 1).find((l) => supported.includes(l));
  const lower = order
    .slice(0, index)
    .reverse()
    .find((l) => supported.includes(l));
  return higher ?? lower ?? "";
}

export type AiEvent =
  | { type: "delta"; text: string }
  | { type: "done"; text: string; aborted: boolean }
  | { type: "error"; message: string };

/**
 * Sends a prompt in the conversation `sessionId`. The backend starts the
 * tool for the first prompt and keeps it until `closeAiSession`.
 */
export function promptAi(
  provider: AiProvider,
  sessionId: string,
  request: AiPromptRequest,
  onEvent: (event: AiEvent) => void,
): Promise<void> {
  return invoke("ai_prompt", {
    provider,
    sessionId,
    request,
    onEvent: new Channel<AiEvent>(onEvent),
  });
}

export function abortAi(sessionId: string): Promise<void> {
  return invoke("ai_abort", { sessionId });
}

export function closeAiSession(sessionId: string): Promise<void> {
  return invoke("ai_close", { sessionId });
}

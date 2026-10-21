import { Channel, invoke } from "@tauri-apps/api/core";

/** Document context for the first prompt of a conversation. */
export interface PiPromptRequest {
  instruction: string;
  /** Document markdown before the selection or cursor. */
  before: string;
  /** Document markdown after the selection or cursor. */
  after: string;
  /** Selected text as markdown, or null in cursor mode. */
  selection: string | null;
  /** Thinking level for this prompt. Omit to keep the current level. */
  thinking?: string;
  /** Model as "provider/modelId". Omit to keep the current model. */
  model?: string;
  /** The answer is for reading. It does not replace the selection. */
  readOnly?: boolean;
}

/** A model that pi can use. */
export interface PiModel {
  provider: string;
  id: string;
  name: string;
  /** Thinking levels that the model supports, from lowest to highest. */
  thinkingLevels: string[];
}

/** The value for `PiPromptRequest.model` and the `piModel` setting. */
export const piModelKey = (model: PiModel) => `${model.provider}/${model.id}`;

/** The models of pi. The backend asks pi once per app session. */
export function listPiModels(): Promise<PiModel[]> {
  return invoke("pi_list_models");
}

/** pi thinking levels. The empty value uses the default level of pi. */
export const PI_THINKING_LEVELS = [
  { value: "", label: "Default" },
  { value: "off", label: "Off" },
  { value: "minimal", label: "Minimal" },
  { value: "low", label: "Low" },
  { value: "medium", label: "Medium" },
  { value: "high", label: "High" },
  { value: "xhigh", label: "Extra high" },
  { value: "max", label: "Max" },
];

/**
 * The supported level nearest to `level`: the next higher level, else the
 * next lower level (the same rule as pi). The empty value (the pi default)
 * is always supported.
 */
export function fitThinkingLevel(level: string, supported: string[]): string {
  if (!level || supported.includes(level)) return level;
  const order = PI_THINKING_LEVELS.map((l) => l.value).filter(Boolean);
  const index = order.indexOf(level);
  const higher = order.slice(index + 1).find((l) => supported.includes(l));
  const lower = order
    .slice(0, Math.max(index, 0))
    .reverse()
    .find((l) => supported.includes(l));
  return higher ?? lower ?? "";
}

export type PiEvent =
  | { type: "delta"; text: string }
  | { type: "done"; text: string; aborted: boolean }
  | { type: "error"; message: string };

/**
 * Sends a prompt in the conversation `sessionId`. The backend starts pi for
 * the first prompt and keeps it until `closePiSession`.
 */
export function promptPi(
  sessionId: string,
  request: PiPromptRequest,
  onEvent: (event: PiEvent) => void,
): Promise<void> {
  return invoke("pi_prompt", {
    sessionId,
    request,
    onEvent: new Channel<PiEvent>(onEvent),
  });
}

export function abortPi(sessionId: string): Promise<void> {
  return invoke("pi_abort", { sessionId });
}

export function closePiSession(sessionId: string): Promise<void> {
  return invoke("pi_close", { sessionId });
}

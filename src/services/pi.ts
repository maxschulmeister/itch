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

import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import type { ConnectionInfo } from "./types";

export class ConnectionError extends Error {
  constructor(
    message: string,
    readonly status: number,
  ) {
    super(message);
  }
}

interface RawResponse {
  status: number;
  body: unknown;
  retryAfter: number | null;
}

const MAX_RETRIES = 3;

const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

/**
 * POST a JSON body to a path on the connection server. Rust adds the token.
 * Waits and tries again when the server returns 429.
 */
export async function request<T>(
  conn: ConnectionInfo,
  path: string,
  body: object,
): Promise<{ status: number; body: T }> {
  for (let attempt = 0; ; attempt++) {
    const response = await invoke<RawResponse>("connection_request", {
      id: conn.id,
      path,
      body,
    });
    if (response.status === 429 && attempt < MAX_RETRIES) {
      await sleep((response.retryAfter ?? 2 ** attempt) * 1000);
      continue;
    }
    return { status: response.status, body: response.body as T };
  }
}

export function listConnections(): Promise<ConnectionInfo[]> {
  return invoke("connections_list");
}

export function addConnection(
  kind: string,
  name: string,
  baseUrl: string,
  token: string,
): Promise<ConnectionInfo> {
  return invoke("connections_add", { kind, name, baseUrl, token });
}

export function renameConnection(id: string, name: string): Promise<void> {
  return invoke("connections_rename", { id, name });
}

export function removeConnection(id: string): Promise<void> {
  return invoke("connections_remove", { id });
}

export async function upload(
  conn: ConnectionInfo,
  url: string,
  fields: Record<string, string>,
  file: Blob,
  fileName: string,
): Promise<number> {
  const bytes = new Uint8Array(await file.arrayBuffer());
  let binary = "";
  for (let i = 0; i < bytes.length; i += 0x8000) {
    binary += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  }
  return invoke("connection_upload", {
    id: conn.id,
    url,
    fields: Object.entries(fields),
    fileName,
    contentType: file.type || "application/octet-stream",
    dataBase64: btoa(binary),
  });
}

/** Display URL for an image that needs the connection token. */
export function assetUrl(conn: ConnectionInfo, src: string): string {
  return convertFileSrc(`${conn.id}/${src}`, "itch-conn");
}

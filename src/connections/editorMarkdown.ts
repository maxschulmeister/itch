/** Shown when local and remote changes touch the same text. */
export const CONFLICT_MESSAGE =
  "Someone changed the same text. Your version was kept. The other version is in the document history.";

import type { MarkdownCodec } from "./types";

/** The editor shows a document as `# Title` and the body. */
export function documentToMarkdown(doc: { title: string; text: string }): string {
  const title = doc.title.replace(/[\\`*_[\]~<>#$=]/g, "\\$&");
  return `# ${title}\n\n${doc.text}`;
}

/** Split editor markdown into title (first H1) and body. */
export function markdownToDocument(
  codec: MarkdownCodec,
  markdown: string,
  fallbackTitle: string,
): { title: string; text: string } {
  const json = codec.parse(markdown);
  const [first, ...rest] = json.content ?? [];
  if (first?.type !== "heading" || first.attrs?.level !== 1) {
    return { title: fallbackTitle, text: markdown };
  }
  const title = (first.content ?? []).map((node) => node.text ?? "").join("").trim();
  return { title, text: codec.serialize({ type: "doc", content: rest }) };
}

import type { JSONContent } from "@tiptap/core";
import type { MarkdownCodec } from "../types";

/** One `documents.update` call with `editMode: "patch"`. */
export interface Patch {
  findText: string;
  text: string;
}

interface Hunk {
  baseStart: number;
  baseEnd: number;
  nextStart: number;
  nextEnd: number;
}

/** Hunks where two block lists differ (longest common subsequence). */
function diffBlocks(base: string[], next: string[]): Hunk[] {
  const n = base.length;
  const m = next.length;
  const lcs: number[][] = Array.from({ length: n + 1 }, () => new Array<number>(m + 1).fill(0));
  for (let i = n - 1; i >= 0; i--) {
    for (let j = m - 1; j >= 0; j--) {
      lcs[i][j] =
        base[i] === next[j] ? lcs[i + 1][j + 1] + 1 : Math.max(lcs[i + 1][j], lcs[i][j + 1]);
    }
  }
  const hunks: Hunk[] = [];
  let i = 0;
  let j = 0;
  let open: Hunk | null = null;
  while (i < n || j < m) {
    if (i < n && j < m && base[i] === next[j]) {
      if (open) hunks.push(open);
      open = null;
      i++;
      j++;
      continue;
    }
    open ??= { baseStart: i, baseEnd: i, nextStart: j, nextEnd: j };
    if (j < m && (i === n || lcs[i][j + 1] >= lcs[i + 1][j])) {
      j++;
      open.nextEnd = j;
    } else {
      i++;
      open.baseEnd = i;
    }
  }
  if (open) hunks.push(open);
  return hunks;
}

/**
 * Compute patches that turn the Outline text `base` into the editor text
 * `next`. Blocks are compared after the editor's normalization, so a block
 * that itch writes differently but did not change is never sent. Each patch
 * uses the original Outline text of one block as `findText`.
 *
 * Returns null when a safe patch is not possible. The caller then replaces
 * the full text.
 */
export function computePatches(
  codec: MarkdownCodec,
  base: string,
  next: string,
): Patch[] | null {
  const blockText = (node: JSONContent) =>
    codec.serialize({ type: "doc", content: [node] }).trim();

  const tokens = codec.lex(base).filter((token) => token.type !== "space");
  const baseNodes = codec.parse(base).content ?? [];
  if (tokens.length !== baseNodes.length) return null;

  const offsets: number[] = [];
  let offset = 0;
  for (const token of codec.lex(base)) {
    if (token.type !== "space") offsets.push(offset);
    offset += token.raw.length;
  }
  if (offset !== base.length) return null;

  const raw = (index: number) => tokens[index].raw.trimEnd();
  const baseBlocks = baseNodes.map(blockText);
  const nextBlocks = (codec.parse(next).content ?? []).map(blockText);

  // The exact base text of blocks [from, to). Outline writes an empty
  // paragraph as `\` and documents.info shows it as a blank line, so a span
  // across an empty paragraph would not match on the server.
  const span = (from: number, to: number) => {
    const text = base.slice(offsets[from], offsets[to - 1] + raw(to - 1).length);
    return to - from > 1 && text.includes("\n\n\n\n") ? null : text;
  };

  const planned: { index: number; patch: Patch }[] = [];
  const plan = (from: number, to: number, text: string) => {
    const findText = span(from, to);
    if (findText === null) return false;
    planned.push({ index: from, patch: { findText, text } });
    return true;
  };

  for (const hunk of diffBlocks(baseBlocks, nextBlocks)) {
    const { baseStart, baseEnd } = hunk;
    const inserted = nextBlocks.slice(hunk.nextStart, hunk.nextEnd).join("\n\n");
    let ok: boolean;
    if (baseEnd > baseStart && inserted) {
      ok = plan(baseStart, baseEnd, inserted);
    } else if (baseEnd > baseStart) {
      // Delete together with a neighbor block, so no empty paragraph stays.
      if (baseStart > 0) ok = plan(baseStart - 1, baseEnd, raw(baseStart - 1));
      else if (baseEnd < tokens.length) ok = plan(baseStart, baseEnd + 1, raw(baseEnd));
      else ok = plan(baseStart, baseEnd, "");
    } else if (baseStart > 0) {
      ok = plan(baseStart - 1, baseStart, `${raw(baseStart - 1)}\n\n${inserted}`);
    } else if (tokens.length > 0) {
      ok = plan(0, 1, `${inserted}\n\n${raw(0)}`);
    } else {
      ok = false;
    }
    if (!ok) return null;
  }

  // Outline replaces the first match of findText. Check that the first match
  // is the intended block, in the text as it is after the earlier patches.
  let current = base;
  let delta = 0;
  for (const { index, patch } of planned) {
    const expected = offsets[index] + delta;
    if (current.indexOf(patch.findText) !== expected) return null;
    current =
      current.slice(0, expected) + patch.text + current.slice(expected + patch.findText.length);
    delta += patch.text.length - patch.findText.length;
  }
  return planned.map(({ patch }) => patch);
}

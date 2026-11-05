import { diff_match_patch, DIFF_DELETE, DIFF_EQUAL } from "diff-match-patch";

const dmp = new diff_match_patch();

/** Replace base[start, end) with text. */
interface Edit {
  start: number;
  end: number;
  text: string;
}

/** The changes from `base` to `other`, as edits in base positions. */
function editsBetween(base: string, other: string): Edit[] {
  const diffs = dmp.diff_main(base, other);
  dmp.diff_cleanupSemantic(diffs);
  const edits: Edit[] = [];
  let position = 0;
  let current: Edit | null = null;
  for (const [op, text] of diffs) {
    if (op === DIFF_EQUAL) {
      if (current) edits.push(current);
      current = null;
      position += text.length;
      continue;
    }
    current ??= { start: position, end: position, text: "" };
    if (op === DIFF_DELETE) {
      current.end += text.length;
      position += text.length;
    } else {
      current.text += text;
    }
  }
  if (current) edits.push(current);
  return edits;
}

/** Apply edits (sorted, inside [start, end)) to base[start, end). */
function applyEdits(base: string, start: number, end: number, edits: Edit[]): string {
  let result = "";
  let position = start;
  for (const edit of edits) {
    result += base.slice(position, edit.start) + edit.text;
    position = edit.end;
  }
  return result + base.slice(position, end);
}

/**
 * Three-way merge of markdown text. Local and remote changes that do not
 * overlap are both kept. Where they overlap or touch, the local text stays and
 * `conflict` is true, unless both sides made the same change.
 */
export function merge3(
  base: string,
  local: string,
  remote: string,
): { text: string; conflict: boolean } {
  if (local === base || local === remote) return { text: remote, conflict: false };
  if (remote === base) return { text: local, conflict: false };

  const edits = [
    ...editsBetween(base, local).map((edit) => ({ ...edit, local: true })),
    ...editsBetween(base, remote).map((edit) => ({ ...edit, local: false })),
  ].sort((a, b) => a.start - b.start || a.end - b.end);

  let text = "";
  let position = 0;
  let conflict = false;
  for (let i = 0; i < edits.length; ) {
    // A group of edits whose base ranges overlap or touch
    const start = edits[i].start;
    let end = edits[i].end;
    const group = [edits[i++]];
    while (i < edits.length && edits[i].start <= end) {
      end = Math.max(end, edits[i].end);
      group.push(edits[i++]);
    }
    const mine = applyEdits(base, start, end, group.filter((edit) => edit.local));
    const theirs = applyEdits(base, start, end, group.filter((edit) => !edit.local));
    const hasMine = group.some((edit) => edit.local);
    const hasTheirs = group.some((edit) => !edit.local);
    if (hasMine && hasTheirs && mine !== theirs) conflict = true;
    text += base.slice(position, start) + (hasMine ? mine : theirs);
    position = end;
  }
  return { text: text + base.slice(position), conflict };
}

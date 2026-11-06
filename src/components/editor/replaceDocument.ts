import type { Editor, JSONContent } from "@tiptap/core";

/**
 * Replace the editor document with `json`, but change only the range that
 * differs. The cursor and scroll position stay where they are. The change is
 * not added to the undo history, so undo never reverts other people's edits.
 */
export function replaceDocumentKeepingSelection(editor: Editor, json: JSONContent) {
  const next = editor.schema.nodeFromJSON(json);
  const { doc } = editor.state;
  const start = doc.content.findDiffStart(next.content);
  const end = doc.content.findDiffEnd(next.content);
  if (start === null || !end) return;
  let { a: endA, b: endB } = end;
  const overlap = start - Math.min(endA, endB);
  if (overlap > 0) {
    endA += overlap;
    endB += overlap;
  }
  editor.view.dispatch(
    editor.state.tr.replace(start, endA, next.slice(start, endB)).setMeta("addToHistory", false),
  );
}

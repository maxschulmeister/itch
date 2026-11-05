import { diff_match_patch } from "diff-match-patch";

const dmp = new diff_match_patch();
// Apply a remote change only where its context still matches closely. A loose
// match can apply a change twice or in the wrong place.
dmp.Match_Threshold = 0.1;

/**
 * Three-way merge of markdown text. Applies the remote changes (base → remote)
 * to the local text. When a remote change cannot be applied because the local
 * text changed the same words, the local words stay and `conflict` is true.
 */
export function merge3(
  base: string,
  local: string,
  remote: string,
): { text: string; conflict: boolean } {
  if (local === base || local === remote) return { text: remote, conflict: false };
  if (remote === base) return { text: local, conflict: false };

  const patches = dmp.patch_make(base, remote);
  const [text, applied] = dmp.patch_apply(patches, local) as [string, boolean[]];
  return { text, conflict: applied.some((ok) => !ok) };
}

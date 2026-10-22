import { useEffect, useLayoutEffect, useRef } from "react";

/**
 * The open layers of the editor menus. Esc closes only the innermost layer,
 * one layer per key press.
 */
export const ESCAPE_LAYER = {
  /** The selection toolbar. */
  toolbar: 0,
  /** A dropdown of the toolbar, or the AI menu. */
  menu: 1,
  /** A sub-menu of a dropdown. */
  subMenu: 2,
} as const;

interface Layer {
  depth: number;
  close: () => void;
}

const layers: Layer[] = [];

/** The deepest layer; of equal layers, the last one that opened. */
function innermost(): Layer | undefined {
  return layers.reduce<Layer | undefined>(
    (inner, layer) => (!inner || layer.depth >= inner.depth ? layer : inner),
    undefined,
  );
}

// A window capture listener gets the key before all other listeners: the
// editor, the Radix menus and the global app shortcuts (for example "Esc
// goes back to the note list"). So no other listener acts on this Esc.
function onKeyDown(e: KeyboardEvent) {
  if (e.key !== "Escape" || e.isComposing) return;
  const layer = innermost();
  if (!layer) return;
  e.preventDefault();
  e.stopImmediatePropagation();
  layer.close();
}

/** Registers an open layer that Esc closes with `close`. */
export function useEscapeLayer(
  open: boolean,
  depth: number,
  close: () => void,
) {
  const closeRef = useRef(close);
  useLayoutEffect(() => {
    closeRef.current = close;
  });

  useEffect(() => {
    if (!open) return;
    const layer: Layer = { depth, close: () => closeRef.current() };
    if (layers.length === 0) {
      window.addEventListener("keydown", onKeyDown, true);
    }
    layers.push(layer);
    return () => {
      layers.splice(layers.indexOf(layer), 1);
      if (layers.length === 0) {
        window.removeEventListener("keydown", onKeyDown, true);
      }
    };
  }, [open, depth]);
}

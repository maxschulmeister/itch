import Image, { type ImageOptions } from "@tiptap/extension-image";
import { convertFileSrc } from "@tauri-apps/api/core";

const URL_SCHEME = /^[a-z][a-z\d+.-]*:/i;
const WINDOWS_ABSOLUTE = /^([a-z]:[\\/]|\\\\)/i;

function decodePath(src: string): string {
  try {
    return decodeURIComponent(src);
  } catch {
    return src;
  }
}

function joinPath(dir: string, relative: string): string {
  const separator = dir.includes("\\") && !dir.includes("/") ? "\\" : "/";
  const parts = dir.split(/[\\/]/).filter((part, i) => i === 0 || part !== "");
  for (const segment of relative.split(/[\\/]/)) {
    if (segment === "" || segment === ".") continue;
    if (segment === "..") {
      if (parts.length > 1) parts.pop();
    } else {
      parts.push(segment);
    }
  }
  return parts.join(separator);
}

/**
 * Convert a markdown image src to a URL the webview can load.
 * Relative paths resolve against the folder of the note.
 */
export function resolveImageSrc(src: string, noteDir: string | null): string {
  const isWindowsAbsolute = WINDOWS_ABSOLUTE.test(src);
  if (!src || src.startsWith("//") || (URL_SCHEME.test(src) && !isWindowsAbsolute)) {
    return src;
  }
  const path = decodePath(src);
  if (path.startsWith("/") || isWindowsAbsolute) return convertFileSrc(path);
  if (!noteDir) return src;
  return convertFileSrc(joinPath(noteDir, path));
}

interface LocalImageOptions extends ImageOptions {
  getNoteDir: () => string | null;
}

/**
 * Image node that shows local images. The node keeps the original src,
 * so the saved markdown does not change.
 */
export const LocalImage = Image.extend<LocalImageOptions>({
  addOptions() {
    return {
      ...this.parent!(),
      getNoteDir: () => null,
    };
  },

  addNodeView() {
    return ({ HTMLAttributes }) => {
      const img = document.createElement("img");
      for (const [name, value] of Object.entries(HTMLAttributes)) {
        if (value !== null && value !== undefined) img.setAttribute(name, String(value));
      }
      img.src = resolveImageSrc(String(HTMLAttributes.src ?? ""), this.options.getNoteDir());
      return { dom: img };
    };
  },
});

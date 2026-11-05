import { Extension, Mark, Node, mergeAttributes, type JSONContent } from "@tiptap/core";
import HardBreak from "@tiptap/extension-hard-break";
import { Lexer, Marked, type marked as MarkedFunction } from "marked";
import Underline from "@tiptap/extension-underline";
import type { EditorDialect } from "../types";

/**
 * Outline markdown that differs from CommonMark. Source:
 * outline/shared/editor (nodes, marks, lib/markdown/serializer.ts).
 */

/** `__text__` is underline in Outline, not bold. */
const OutlineUnderline = Underline.extend({
  renderMarkdown: (node, h) => `__${h.renderChildren(node)}__`,
  markdownTokenizer: {
    name: "underline",
    level: "inline",
    start: (src) => src.indexOf("__"),
    tokenize(src, _tokens, lexer) {
      const match = /^__(?=\S)([\s\S]*?\S)__(?!_)/.exec(src);
      if (!match) return undefined;
      return {
        type: "underline",
        raw: match[0],
        text: match[1],
        tokens: lexer.inlineTokens(match[1]),
      };
    },
  },
});

/** Outline writes a hard break as a literal `\n`. */
const OutlineHardBreak = HardBreak.extend({
  renderMarkdown: () => "\\n",
});

const OutlineHardBreakSyntax = Extension.create({
  name: "outlineHardBreakSyntax",
  markdownTokenizer: {
    name: "outlineHardBreak",
    level: "inline",
    start: (src) => src.indexOf("\\n"),
    tokenize: (src) => (src.startsWith("\\n") ? { type: "br", raw: "\\n" } : undefined),
  },
});

const OutlineHighlight = Mark.create({
  name: "highlight",
  parseHTML: () => [{ tag: "mark" }],
  renderHTML: ({ HTMLAttributes }) => ["mark", mergeAttributes(HTMLAttributes), 0],
  parseMarkdown: (token, h) => h.applyMark("highlight", h.parseInline(token.tokens ?? [])),
  renderMarkdown: (node, h) => `==${h.renderChildren(node)}==`,
  markdownTokenizer: {
    name: "highlight",
    level: "inline",
    start: (src) => src.indexOf("=="),
    tokenize(src, _tokens, lexer) {
      const match = /^==(?=\S)([\s\S]*?\S)==/.exec(src);
      if (!match) return undefined;
      return {
        type: "highlight",
        raw: match[0],
        text: match[1],
        tokens: lexer.inlineTokens(match[1]),
      };
    },
  },
});

/**
 * Lex nested block content. The lexer gets its own tokenizer: Marked keeps one
 * tokenizer in its shared defaults, and a nested lexer would take it over from
 * the outer lexer.
 */
const lexBlocks = (marked: typeof MarkedFunction, src: string) =>
  new Lexer({ ...marked.defaults, tokenizer: undefined }).lex(src);

/** `:::info|warning|tip|success` … `:::` */
const createNotice = (marked: typeof MarkedFunction) => Node.create({
  name: "outlineNotice",
  group: "block",
  content: "block+",
  defining: true,
  addAttributes: () => ({ style: { default: "info" } }),
  parseHTML: () => [
    {
      tag: "div[data-notice]",
      getAttrs: (el) => ({ style: (el as HTMLElement).dataset.notice ?? "info" }),
    },
  ],
  renderHTML: ({ node, HTMLAttributes }) => [
    "div",
    mergeAttributes(HTMLAttributes, {
      "data-notice": node.attrs.style,
      class: `outline-notice outline-notice-${node.attrs.style}`,
    }),
    0,
  ],
  markdownTokenName: "outlineNotice",
  markdownTokenizer: {
    name: "outlineNotice",
    level: "block",
    start: (src) => src.match(/^:::\w*[ \t]*$/m)?.index ?? -1,
    tokenize(src) {
      const match = /^:::(\w*)[ \t]*\n([\s\S]*?)\n?:::[ \t]*(?:\n|$)/.exec(src);
      if (!match) return undefined;
      return {
        type: "outlineNotice",
        raw: match[0],
        style: match[1] || "info",
        tokens: lexBlocks(marked, match[2]),
      };
    },
  },
  parseMarkdown: (token, h) =>
    h.createNode("outlineNotice", { style: token.style }, h.parseChildren(token.tokens ?? [])),
  renderMarkdown: (node, h) =>
    `:::${node.attrs?.style ?? "info"}\n${h.renderChildren(node.content ?? [], "\n\n")}\n:::`,
});

/** Toggle block. The fence is 3 or more `+`, longer when nested. */
const createToggle = (marked: typeof MarkedFunction) => Node.create({
  name: "outlineToggle",
  group: "block",
  content: "block+",
  defining: true,
  addAttributes: () => ({ fence: { default: "+++" } }),
  parseHTML: () => [{ tag: "div[data-toggle]" }],
  renderHTML: ({ HTMLAttributes }) => [
    "div",
    mergeAttributes(HTMLAttributes, { "data-toggle": "", class: "outline-toggle" }),
    0,
  ],
  markdownTokenName: "outlineToggle",
  markdownTokenizer: {
    name: "outlineToggle",
    level: "block",
    start: (src) => src.match(/^\+{3,}[ \t]*$/m)?.index ?? -1,
    tokenize(src) {
      const match = /^(\+{3,})[ \t]*\n([\s\S]*?)\n?\1[ \t]*(?:\n|$)/.exec(src);
      if (!match) return undefined;
      return {
        type: "outlineToggle",
        raw: match[0],
        fence: match[1],
        tokens: lexBlocks(marked, match[2]),
      };
    },
  },
  parseMarkdown: (token, h) =>
    h.createNode("outlineToggle", { fence: token.fence }, h.parseChildren(token.tokens ?? [])),
  renderMarkdown: (node, h) => {
    const fence = node.attrs?.fence ?? "+++";
    return `${fence}\n${h.renderChildren(node.content ?? [], "\n\n")}\n${fence}`;
  },
});

/** `$latex$`. Kept as source text, so the editor never escapes it. */
const OutlineInlineMath = Node.create({
  name: "outlineInlineMath",
  group: "inline",
  inline: true,
  atom: true,
  addAttributes: () => ({ latex: { default: "" } }),
  parseHTML: () => [
    {
      tag: "span[data-inline-math]",
      getAttrs: (el) => ({ latex: (el as HTMLElement).textContent ?? "" }),
    },
  ],
  renderHTML: ({ node }) => [
    "span",
    { "data-inline-math": "", class: "outline-inline-math" },
    node.attrs.latex,
  ],
  markdownTokenName: "outlineInlineMath",
  markdownTokenizer: {
    name: "outlineInlineMath",
    level: "inline",
    start: (src) => src.search(/\$(?=[^\s$])/),
    tokenize(src) {
      const match = /^\$(?=[^\s$])([^$\n]*?[^\s\\])\$(?!\d)/.exec(src);
      if (!match) return undefined;
      return { type: "outlineInlineMath", raw: match[0], latex: match[1] };
    },
  },
  parseMarkdown: (token, h) => h.createNode("outlineInlineMath", { latex: token.latex }),
  renderMarkdown: (node) => `$${node.attrs?.latex ?? ""}$`,
});

/** `@tiptap/markdown` drops escape tokens (`\*`). Keep the escaped character. */
const EscapedCharacter = Extension.create({
  name: "escapedCharacter",
  markdownTokenName: "escape",
  parseMarkdown: (token) => ({ type: "text", text: token.text }),
});

const VERBATIM_NODES = new Set(["codeBlock", "blockMath", "outlineInlineMath", "frontmatter"]);
const LINE_NODES = new Set(["paragraph", "heading", "tableCell", "tableHeader"]);

/**
 * The rules of `esc()` in Outline's markdown serializer, plus escapes for the
 * other delimiters this dialect reads (`_`, `==`, `$`, `<`, `>` at line start).
 * Without them, literal text would become formatting when it is read again.
 */
function escapeText(text: string, startOfLine: boolean, inTable: boolean): string {
  let out = text
    .replace(/[`*\\~_$<]/g, "\\$&")
    .replace(/={2,}/g, (run) => run.replace(/=/g, "\\="));
  out = out
    .split("\n")
    .map((line, index) => {
      const last = line.lastIndexOf("](");
      let result =
        last === -1 ? line : line.slice(0, last).replace(/\[/g, "\\$&") + line.slice(last);
      // Every line after a newline starts a line, as does the first if startOfLine
      if (index > 0 || startOfLine) {
        result = result
          .replace(/^[:#\-*+>]/, "\\$&")
          .replace(/^(\d+)\.(?=\s|$)/, "$1\\.")
          .replace(/^([a-z])\.(?=\s)/i, "$1\\.");
      }
      return result;
    })
    .join("\n");
  if (inTable) out = out.replace(/\|/g, "\\$&");
  return out;
}

/** Join neighbor text nodes with the same marks, so escapes see whole runs like `==`. */
function joinText(content: JSONContent[]): JSONContent[] {
  const joined: JSONContent[] = [];
  for (const child of content) {
    const last = joined[joined.length - 1];
    if (
      child.type === "text" &&
      last?.type === "text" &&
      JSON.stringify(last.marks ?? []) === JSON.stringify(child.marks ?? [])
    ) {
      joined[joined.length - 1] = { ...last, text: (last.text ?? "") + (child.text ?? "") };
    } else {
      joined.push(child);
    }
  }
  return joined;
}

function escapeNode(node: JSONContent, inTable: boolean): JSONContent {
  if (VERBATIM_NODES.has(node.type ?? "")) return node;
  const table = inTable || node.type === "table";
  if (!node.content) return node;
  let lineStart = LINE_NODES.has(node.type ?? "");
  const content = joinText(node.content).map((child) => {
    if (child.type === "text") {
      const isCode = child.marks?.some((m) => m.type === "code");
      const text = isCode ? child.text : escapeText(child.text ?? "", lineStart, table);
      lineStart = false;
      return { ...child, text };
    }
    lineStart = child.type === "hardBreak";
    return escapeNode(child, table);
  });
  return { ...node, content };
}

export function createOutlineDialect(resolveImageSrc: (src: string) => string): EditorDialect {
  // A separate instance keeps Outline syntax out of other editors. It has the
  // same members that @tiptap/markdown uses from the `marked` function.
  const marked = new Marked() as unknown as typeof MarkedFunction;
  return {
    marked,
    inlineImages: true,
    starterKit: { underline: false, hardBreak: false },
    extensions: [
      OutlineUnderline,
      OutlineHardBreak,
      OutlineHardBreakSyntax,
      OutlineHighlight,
      OutlineInlineMath,
      EscapedCharacter,
      createNotice(marked),
      createToggle(marked),
    ],
    prepareForSerialize: (json) => escapeNode(json, false),
    resolveImageSrc,
  };
}

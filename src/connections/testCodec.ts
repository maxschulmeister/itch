import { MarkdownManager } from "@tiptap/markdown";
import StarterKit from "@tiptap/starter-kit";
import Image from "@tiptap/extension-image";
import CodeBlock from "@tiptap/extension-code-block";
import TaskList from "@tiptap/extension-task-list";
import TaskItem from "@tiptap/extension-task-item";
import { TableKit } from "@tiptap/extension-table";
import { ScratchBlockMath } from "../components/editor/MathExtensions";
import type { EditorDialect, MarkdownCodec } from "./types";

/**
 * Markdown codec without an editor, for tests. It uses the markdown extensions
 * of the itch editor plus the dialect.
 */
export function createTestCodec(dialect: EditorDialect): MarkdownCodec {
  const manager = new MarkdownManager({
    marked: dialect.marked,
    extensions: [
      StarterKit.configure({ codeBlock: false, ...dialect.starterKit }),
      Image.configure({ inline: dialect.inlineImages }),
      TaskList,
      TaskItem.configure({ nested: true }),
      TableKit,
      CodeBlock,
      ScratchBlockMath,
      ...dialect.extensions,
    ],
  });
  return {
    parse: (markdown) => manager.parse(markdown),
    serialize: (json) => manager.serialize(dialect.prepareForSerialize(json)),
    lex: (markdown) => manager.instance.lexer(markdown),
  };
}

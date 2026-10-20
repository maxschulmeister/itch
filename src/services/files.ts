import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";

export interface FileContent {
  path: string;
  content: string;
  title: string;
  modified: number;
}

export async function readFileDirect(path: string): Promise<FileContent> {
  return invoke("read_file_direct", { path });
}

export async function saveFileDirect(
  path: string,
  content: string,
): Promise<FileContent> {
  return invoke("save_file_direct", { path, content });
}

export async function openFilePreview(path: string): Promise<void> {
  return invoke("open_file_preview", { path });
}

// Ask for a markdown file and open it in its own window
export async function openFileWithDialog(): Promise<void> {
  const selected = await open({
    multiple: false,
    title: "Open File",
    filters: [{ name: "Markdown", extensions: ["md", "markdown"] }],
  });
  if (typeof selected === "string") {
    await openFilePreview(selected);
  }
}

export async function getRecentFiles(): Promise<string[]> {
  return invoke("get_recent_files");
}

export interface ImportedNote {
  id: string;
  title: string;
  preview: string;
  modified: number;
}

export async function importFileToFolder(
  path: string,
): Promise<ImportedNote> {
  return invoke("import_file_to_folder", { path });
}

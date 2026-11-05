import { invoke } from "@tauri-apps/api/core";
import { open, save } from "@tauri-apps/plugin-dialog";

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

// Create an empty untitled file in the app data and open it in its own window
export async function createUntitledFile(): Promise<void> {
  return invoke("create_untitled_file");
}

// Ask where to save an untitled file, then move it there. Returns null when
// the user cancels the save dialog.
export async function saveUntitledFileWithDialog(
  source: string,
  name: string,
): Promise<FileContent | null> {
  const path = await save({
    title: "Save",
    defaultPath: `${name.replace(/[\\/:*?"<>|]/g, "-")}.md`,
    filters: [{ name: "Markdown", extensions: ["md", "markdown"] }],
  });
  if (!path) return null;
  return invoke("save_untitled_file", { source, path });
}

export async function discardUntitledFile(path: string): Promise<void> {
  return invoke("discard_untitled_file", { path });
}

// The user kept a window open, so the app must not quit.
export async function cancelQuit(): Promise<void> {
  return invoke("cancel_quit");
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

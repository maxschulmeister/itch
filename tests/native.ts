import { readFileSync } from "node:fs";
import { expect, type Page } from "@playwright/test";

export const FILE_PATH = "/tmp/note.md";
export const FILE_TEXT = "Text from the standalone file.";

// The official Tauri IPC mock (mockIPC/mockWindows), as a classic script so it
// runs before the app.
const tauriMocks = readFileSync(
  new URL("../node_modules/@tauri-apps/api/mocks.js", import.meta.url),
  "utf8",
).replace(/^export \{.*\};?$/m, "");

type Args = Record<string, any>;

/**
 * The native side of the app, shared by all windows of one test. It keeps the
 * file and the settings in memory, records the commands, and broadcasts
 * `settings-changed` with the full settings after `update_settings`, like the
 * backend.
 */
export class Native {
  file = {
    path: FILE_PATH,
    content: `# Note\n\n${FILE_TEXT}\n`,
    title: "Note",
    modified: 1,
  };
  settings: Args = { theme: { mode: "light" } };
  notesFolder: string | null = null;
  notes: Record<string, string> = {};
  /** The path that the user selects in the native save dialog. */
  pathToSave: string | null = null;
  /** The paths of the `create_file` calls. */
  createdFiles: string[] = [];
  /** The folder that the user selects in the native folder dialog. */
  folderToChoose: string | null = null;
  /** When set, the native Settings window does not open, with this error. */
  settingsWindowError: string | null = null;
  calls: string[] = [];
  unexpected: string[] = [];
  /**
   * The `show_settings_window` calls. `pixel` is a PNG of the viewport center
   * when the backend shows the window: the first frame that the user sees.
   */
  shows: { background: number[]; pixel: Buffer }[] = [];
  private settingsLoaded: Promise<void> = Promise.resolve();

  /** Holds `get_settings` replies until the returned function is called. */
  holdSettings() {
    let release!: () => void;
    this.settingsLoaded = new Promise((resolve) => (release = resolve));
    return release;
  }
  private windows: Page[] = [];
  private labels = new Map<Page, string>();

  count(cmd: string) {
    return this.calls.filter((c) => c === cmd).length;
  }

  /** Opens `url` in `page` as the native window `label`. */
  async open(page: Page, url: string, label: string) {
    this.windows.push(page);
    this.labels.set(page, label);
    await page.exposeFunction("__native", (cmd: string, args: Args) =>
      this.invoke(cmd, args, page),
    );
    await page.addInitScript(`${tauriMocks}
      mockWindows(${JSON.stringify(label)});
      mockConvertFileSrc("macos");
      mockIPC((cmd, args) => window.__native(cmd, args), { shouldMockEvents: true });
      // The mock drops emitTo; send it to the native side like Tauri does.
      const invokeInWindow = window.__TAURI_INTERNALS__.invoke;
      window.__TAURI_INTERNALS__.invoke = (cmd, args, options) =>
        cmd === "plugin:event|emit_to"
          ? window.__native(cmd, args)
          : invokeInWindow(cmd, args, options);`);
    await page.goto(url);
  }

  async openFileWindow(page: Page) {
    await this.open(page, `/?mode=preview&file=${encodeURIComponent(FILE_PATH)}`, "preview");
    await expect(page.locator(".ProseMirror")).toContainText(FILE_TEXT);
  }

  /** Waits until no window writes the settings for 500 ms. */
  async settled() {
    let before: number;
    do {
      before = this.count("update_settings");
      await new Promise((resolve) => setTimeout(resolve, 500));
    } while (this.count("update_settings") !== before);
  }

  private async show(page: Page, args: Args) {
    const { width, height } = page.viewportSize()!;
    const pixel = await page.screenshot({
      clip: { x: Math.floor(width / 2), y: Math.floor(height / 2), width: 1, height: 1 },
    });
    this.shows.push({ background: args.background, pixel });
    return null;
  }

  private broadcast(event: string, payload: unknown, label?: string) {
    for (const page of this.windows) {
      if (label && this.labels.get(page) !== label) continue;
      page
        .evaluate(
          ([event, payload]) =>
            (window as any).__TAURI_INTERNALS__.invoke("plugin:event|emit", { event, payload }),
          [event, payload] as const,
        )
        .catch(() => {});
    }
  }

  private invoke(cmd: string, args: Args, page: Page) {
    this.calls.push(cmd);
    switch (cmd) {
      case "read_file_direct":
        return structuredClone(this.file);
      case "save_file_direct":
        this.file = { ...this.file, content: args.content, modified: this.file.modified + 1 };
        return structuredClone(this.file);
      case "get_settings":
        return this.settingsLoaded.then(() => structuredClone(this.settings));
      case "update_settings":
        this.settings = structuredClone(args.newSettings);
        // Sent after the command returns, like the backend event.
        setTimeout(this.broadcast.bind(this), 0, "settings-changed", this.settings);
        return null;
      case "get_notes_folder":
        return this.notesFolder;
      case "set_notes_folder":
      case "close_notes_folder":
        this.notesFolder = args.path ?? null;
        setTimeout(() => {
          this.broadcast("settings-changed", this.settings);
          this.broadcast("set-notes-folder", this.notesFolder);
        });
        return this.notesFolder;
      case "get_recent_files":
        // No recent files: the start screen shows its empty state.
        return [];
      case "get_default_ignored_patterns":
        return ["node_modules", ".git"];
      case "plugin:dialog|save":
        return this.pathToSave;
      case "create_file":
        this.createdFiles.push(args.path);
        return null;
      case "open_folder_dialog":
        return this.folderToChoose;
      case "list_notes":
        return Object.entries(this.notes).map(([id, content]) => ({
          id,
          title: id,
          preview: content,
          modified: 1,
        }));
      case "read_note":
        return { id: args.id, title: args.id, content: this.notes[args.id], path: `${this.notesFolder}/${args.id}.md`, modified: 1 };
      case "save_note":
        this.notes[args.id] = args.content;
        return { id: args.id, title: args.id, content: args.content, path: `${this.notesFolder}/${args.id}.md`, modified: 2 };
      case "start_file_watcher":
        return null;
      case "plugin:event|emit_to":
        // Tauri sends the event only to the windows with the target label.
        setTimeout(this.broadcast.bind(this), 0, args.event, args.payload, args.target.label);
        return null;
      case "git_is_available":
        return true;
      case "git_get_status":
        // The library is not a git repository.
        return {
          isRepo: false,
          hasRemote: false,
          hasUpstream: false,
          remoteUrl: null,
          changedCount: 0,
          aheadCount: 0,
          behindCount: 0,
          currentBranch: null,
          error: null,
        };
      case "preview_note_name":
        // The default "Untitled" template has no date tokens to expand.
        return args.template;
      case "get_cli_status":
        return { supported: false, installed: false, path: null };
      case "ai_providers":
        return ["pi", "claude", "codex", "opencode", "ollama"];
      case "ai_list_models":
        return {
          models: [{ id: "model-a", name: "Model A", group: "Test", thinkingLevels: [] }],
          defaultModel: "model-a",
          defaultThinking: null,
        };
      case "open_settings_window":
        if (this.settingsWindowError) throw new Error(this.settingsWindowError);
        return null;
      case "show_settings_window":
        return this.show(page, args);
      case "set_title_bar_theme":
        return null;
    }
    // The labels of the open windows, for Window.getByLabel and getAllWindows.
    if (cmd === "plugin:window|get_all_windows") {
      return this.windows.filter((page) => !page.isClosed()).map((page) => this.labels.get(page));
    }
    // Other native window calls (theme, close) have no app data.
    if (cmd.startsWith("plugin:window|")) return null;
    this.unexpected.push(cmd);
    throw new Error(`Unexpected IPC command: ${cmd}`);
  }
}

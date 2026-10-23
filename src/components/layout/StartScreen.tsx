import { useCallback, useEffect, useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import { toast } from "sonner";
import { useNotes } from "../../context/NotesContext";
import { useTheme } from "../../context/ThemeContext";
import { Button } from "../ui";
import { NoteIcon } from "../icons";
import { isWindows, mod, shortcut } from "../../lib/platform";
import * as filesService from "../../services/files";

interface StartScreenProps {
  onOpenSettings: () => void;
}

// Split a path into file name and parent folder (handles / and \ separators)
function splitPath(path: string): { name: string; parent: string } {
  const index = Math.max(path.lastIndexOf("/"), path.lastIndexOf("\\"));
  return { name: path.slice(index + 1), parent: path.slice(0, index) };
}

export function StartScreen({ onOpenSettings }: StartScreenProps) {
  const { setNotesFolder } = useNotes();
  const { reloadSettings } = useTheme();
  const [recentFiles, setRecentFiles] = useState<string[]>([]);

  const loadRecentFiles = useCallback(() => {
    filesService
      .getRecentFiles()
      .then(setRecentFiles)
      .catch((err) => console.error("Failed to load recent files:", err));
  }, []);

  // Reload when the window shows again, because files open in other windows
  useEffect(() => {
    loadRecentFiles();
    window.addEventListener("focus", loadRecentFiles);
    return () => window.removeEventListener("focus", loadRecentFiles);
  }, [loadRecentFiles]);

  const handleOpenFile = async () => {
    try {
      await filesService.openFileWithDialog();
    } catch (err) {
      console.error("Failed to open file:", err);
      toast.error("Failed to open file");
    }
  };

  const handleOpenRecent = async (path: string) => {
    try {
      await filesService.openFilePreview(path);
    } catch (err) {
      console.error("Failed to open file:", err);
      toast.error("Failed to open file");
      loadRecentFiles();
    }
  };

  const handleOpenFolder = async () => {
    try {
      const selected = await open({
        directory: true,
        multiple: false,
        title: "Open Folder",
      });

      if (selected && typeof selected === "string") {
        await setNotesFolder(selected);
        // Reload settings, because the folder-specific part changed
        await reloadSettings();
      }
    } catch (err) {
      console.error("Failed to select folder:", err);
      toast.error("Failed to select folder");
    }
  };

  return (
    <div className="h-full flex flex-col bg-bg-secondary">
      {/* Draggable title bar area */}
      {!isWindows && <div className="h-10 shrink-0" data-tauri-drag-region />}

      <div className="flex-1 min-h-0 overflow-auto flex">
        <div className="m-auto w-full max-w-md p-8 select-none">
          <div className="text-center animate-fade-in-up">
            <h1 className="text-3xl text-text font-serif mb-2 tracking-[-0.01em]">
              Welcome to Itch
            </h1>
            <p className="text-text-muted mb-6">
              Open a markdown file to edit it, or open a folder to keep your
              notes together in a library.
            </p>
          </div>

          <div
            className="flex flex-wrap justify-center gap-3 animate-fade-in-up"
            style={{ animationDelay: "100ms" }}
          >
            <Button
              onClick={handleOpenFile}
              size="xl"
              title={`Open File (${shortcut(mod, "O")})`}
            >
              Open File…
            </Button>
            <Button onClick={handleOpenFolder} variant="outline" size="xl">
              Open Folder…
            </Button>
          </div>

          {recentFiles.length > 0 && (
            <section
              className="mt-10 animate-fade-in-up"
              style={{ animationDelay: "200ms" }}
            >
              <h2 className="px-2.5 mb-1 text-xs font-medium text-text-muted">
                Recent
              </h2>
              <ul>
                {recentFiles.map((path) => {
                  const { name, parent } = splitPath(path);
                  return (
                    <li key={path}>
                      <button
                        onClick={() => handleOpenRecent(path)}
                        title={path}
                        className="w-full flex items-center gap-2.5 px-2.5 py-1.5 rounded-md text-start hover:bg-bg-muted transition-colors cursor-pointer focus-visible:outline focus-visible:outline-accent"
                      >
                        <NoteIcon className="w-4.5 h-4.5 stroke-[1.5] text-text-muted shrink-0" />
                        <span className="min-w-0 flex-1">
                          <span className="block text-sm text-text truncate">
                            {name}
                          </span>
                          <span className="block text-xs text-text-muted truncate">
                            {parent}
                          </span>
                        </span>
                      </button>
                    </li>
                  );
                })}
              </ul>
            </section>
          )}

          <div
            className="mt-8 text-center animate-fade-in-up"
            style={{ animationDelay: "300ms" }}
          >
            <Button onClick={onOpenSettings} variant="link" size="sm">
              Settings
            </Button>
          </div>
        </div>
      </div>
    </div>
  );
}

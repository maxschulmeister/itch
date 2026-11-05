import { useState, useCallback, useRef, useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { message } from "@tauri-apps/plugin-dialog";
import { toast } from "sonner";
import { Editor, type PreviewModeData } from "../editor/Editor";
import { openSettingsWindow } from "../../services/settings";
import * as filesService from "../../services/files";
import { getNotesFolder } from "../../services/notes";

interface PreviewAppProps {
  filePath: string;
  /** The file is a new file in the app data that the user did not save yet. */
  untitled: boolean;
}

const SAVE = "Save…";
const DONT_SAVE = "Don’t Save";

export function PreviewApp({
  filePath: initialPath,
  untitled,
}: PreviewAppProps) {
  // An untitled file moves to a new path when the user saves it
  const [filePath, setFilePath] = useState(initialPath);
  const filePathRef = useRef(filePath);
  filePathRef.current = filePath;
  const untitledRef = useRef(untitled);
  // The file as the last load or save left it, for the close question
  const savedRef = useRef({ content: "", title: "" });
  // Saves the edits that wait for the auto-save delay
  const flushRef = useRef(async () => {});
  const registerSaveFlush = useCallback((flush: () => Promise<void>) => {
    flushRef.current = flush;
    return () => {
      if (flushRef.current === flush) flushRef.current = async () => {};
    };
  }, []);
  const [content, setContent] = useState<string | null>(null);
  const [title, setTitle] = useState("");
  const [modified, setModified] = useState(0);
  const [hasExternalChanges, setHasExternalChanges] = useState(false);
  const [reloadVersion, setReloadVersion] = useState(0);
  const [focusMode, setFocusMode] = useState(false);
  const recentlySavedRef = useRef(false);

  // Load file on mount
  useEffect(() => {
    filesService
      .readFileDirect(filePath)
      .then((result) => {
        savedRef.current = result;
        setContent(result.content);
        setTitle(result.title);
        setModified(result.modified);
      })
      .catch((error) => {
        console.error("Failed to load file:", error);
        toast.error(`Failed to load file: ${error}`);
      });
  }, [filePath]);

  // Listen for window focus to detect external changes
  useEffect(() => {
    const handleFocus = async () => {
      if (recentlySavedRef.current) {
        recentlySavedRef.current = false;
        return;
      }
      try {
        const result = await filesService.readFileDirect(filePath);
        if (result.modified !== modified && content !== null) {
          setHasExternalChanges(true);
        }
      } catch {
        // File may have been deleted
      }
    };

    window.addEventListener("focus", handleFocus);
    return () => window.removeEventListener("focus", handleFocus);
  }, [filePath, modified, content]);

  const save = useCallback(
    async (newContent: string) => {
      try {
        const result = await filesService.saveFileDirect(filePath, newContent);
        savedRef.current = result;
        recentlySavedRef.current = true;
        setModified(result.modified);
        setTitle(result.title);
        setHasExternalChanges(false);
      } catch (error) {
        console.error("Failed to save file:", error);
        toast.error(`Failed to save: ${error}`);
      }
    },
    [filePath],
  );

  const reload = useCallback(async () => {
    try {
      const result = await filesService.readFileDirect(filePath);
      savedRef.current = result;
      setContent(result.content);
      setTitle(result.title);
      setModified(result.modified);
      setHasExternalChanges(false);
      setReloadVersion((v) => v + 1);
    } catch (error) {
      console.error("Failed to reload file:", error);
      toast.error(`Failed to reload: ${error}`);
    }
  }, [filePath]);

  // Ask where to save an untitled file and move it there. Returns false when
  // the user cancels.
  const saveUntitled = useCallback(async () => {
    await flushRef.current();
    const result = await filesService.saveUntitledFileWithDialog(
      filePathRef.current,
      savedRef.current.title,
    );
    if (!result) return false;
    untitledRef.current = false;
    savedRef.current = result;
    setFilePath(result.path);
    setContent(result.content);
    setTitle(result.title);
    setModified(result.modified);
    return true;
  }, []);

  // Cmd+S and File > Save: the edits save automatically, so only an untitled
  // file needs a place
  const saveFile = useCallback(() => {
    const done = untitledRef.current ? saveUntitled() : flushRef.current();
    done.catch((error) => {
      console.error("Failed to save file:", error);
      toast.error(`Failed to save: ${error}`);
    });
  }, [saveUntitled]);

  useEffect(() => {
    const unlisten = listen("save-file", saveFile);
    return () => {
      unlisten.then((fn) => fn());
    };
  }, [saveFile]);

  // Save the edits before the window closes. Ask what to do with an untitled
  // file that has text; an empty one is deleted.
  useEffect(() => {
    const unlisten = getCurrentWindow().onCloseRequested(async (event) => {
      try {
        await flushRef.current();
        if (!untitledRef.current) return;
        const path = filePathRef.current;
        if (savedRef.current.content.trim() === "") {
          await filesService.discardUntitledFile(path);
          return;
        }
        const choice = await message(
          "Your changes will be lost if you don’t save them.",
          {
            title: `Do you want to save “${savedRef.current.title}”?`,
            kind: "warning",
            buttons: { yes: SAVE, no: DONT_SAVE, cancel: "Cancel" },
          },
        );
        if (choice === DONT_SAVE) {
          await filesService.discardUntitledFile(path);
          return;
        }
        if (choice === SAVE && (await saveUntitled())) return;
      } catch (error) {
        console.error("Failed to save file:", error);
        toast.error(`Failed to save: ${error}`);
      }
      event.preventDefault();
      await filesService.cancelQuit();
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, [saveUntitled]);

  // Listen for preview-file-change events
  useEffect(() => {
    const unlisten = listen<string>("preview-file-change", () => {
      setHasExternalChanges(true);
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);

  // Keyboard shortcuts for preview mode
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      const modKey = e.metaKey || e.ctrlKey;

      // Cmd+Shift+Enter: Toggle focus mode
      if (modKey && e.shiftKey && e.key === "Enter") {
        e.preventDefault();
        setFocusMode((prev) => !prev);
        return;
      }

      // Cmd+Shift+M: Toggle markdown source mode
      if (modKey && e.shiftKey && e.key.toLowerCase() === "m") {
        e.preventDefault();
        window.dispatchEvent(new CustomEvent("toggle-source-mode"));
        return;
      }

      // Cmd+Shift+P: Print
      if (modKey && e.shiftKey && e.key.toLowerCase() === "p") {
        e.preventDefault();
        window.dispatchEvent(new CustomEvent("print-note"));
        return;
      }

      // Cmd+P: Block browser print dialog
      if (modKey && !e.shiftKey && e.key === "p") {
        e.preventDefault();
        return;
      }

      // Cmd+N: New file
      if (modKey && !e.shiftKey && e.key.toLowerCase() === "n") {
        e.preventDefault();
        filesService.createUntitledFile().catch((err) => {
          console.error("Failed to create file:", err);
          toast.error("Failed to create file");
        });
        return;
      }

      // Cmd+S: Save
      if (modKey && !e.shiftKey && e.key.toLowerCase() === "s") {
        e.preventDefault();
        saveFile();
        return;
      }

      // Cmd+R: Reload file from disk
      if (modKey && e.key === "r") {
        e.preventDefault();
        reload();
        return;
      }

      // Escape: Exit focus mode
      if (e.key === "Escape" && focusMode) {
        e.preventDefault();
        setFocusMode(false);
        return;
      }

      // Trap Tab to prevent focus leaving editor (only when editor is focused)
      if (e.key === "Tab") {
        const active = document.activeElement;
        const editorEl = document.querySelector(".ProseMirror");
        if (editorEl && editorEl.contains(active)) {
          e.preventDefault();
        }
      }
    };

    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [focusMode, reload, saveFile]);

  // "Save to folder" needs a library; check again on focus, because it can change
  const [hasLibrary, setHasLibrary] = useState(false);
  useEffect(() => {
    const checkLibrary = () => {
      getNotesFolder()
        .then((folder) => setHasLibrary(folder !== null))
        .catch((error) => console.error("Failed to get notes folder:", error));
    };
    checkLibrary();
    window.addEventListener("focus", checkLibrary);
    return () => window.removeEventListener("focus", checkLibrary);
  }, []);

  const [isSaving, setIsSaving] = useState(false);
  const savingRef = useRef(false);

  const handleSaveToFolder = useCallback(async () => {
    if (savingRef.current) return;
    savingRef.current = true;
    setIsSaving(true);
    try {
      await flushRef.current();
      await filesService.importFileToFolder(filePath);
      // The library has a copy now, so the untitled file is not needed
      if (untitledRef.current) {
        await filesService.discardUntitledFile(filePath);
        untitledRef.current = false;
      }
      // Backend emits select-note + focuses main window; close this preview
      await getCurrentWindow().close();
    } catch (error) {
      console.error("Failed to save to folder:", error);
      toast.error(`Failed to save to folder: ${error}`);
    } finally {
      savingRef.current = false;
      setIsSaving(false);
    }
  }, [filePath]);

  const previewData: PreviewModeData = {
    content,
    title,
    filePath,
    modified,
    hasExternalChanges,
    reloadVersion,
    save,
    reload,
    registerSaveFlush,
  };

  return (
    <div className="h-full min-h-0 flex flex-col bg-bg text-text">
      <Editor
        focusMode={focusMode}
        previewMode={previewData}
        onSaveToFolder={hasLibrary ? handleSaveToFolder : undefined}
        saveToFolderDisabled={isSaving}
        onOpenSettings={openSettingsWindow}
      />
    </div>
  );
}

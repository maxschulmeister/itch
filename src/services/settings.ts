import { invoke } from "@tauri-apps/api/core";
import { toast } from "sonner";

export async function openSettingsWindow(): Promise<void> {
  try {
    await invoke("open_settings_window");
  } catch (error) {
    console.error("Failed to open Settings:", error);
    toast.error("Unable to open Settings");
  }
}

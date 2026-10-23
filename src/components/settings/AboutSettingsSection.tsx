import { useState, useEffect } from "react";
import { getVersion } from "@tauri-apps/api/app";

export function AboutSettingsSection() {
  const [appVersion, setAppVersion] = useState<string>("");

  useEffect(() => {
    getVersion()
      .then(setAppVersion)
      .catch(() => {});
  }, []);

  return (
    <div className="space-y-8 py-8">
      {/* Version */}
      <section className="pb-2">
        <h2 className="text-xl font-medium mb-0.5">Version</h2>
        <p className="text-sm text-text-muted mb-4">
          You are currently using Itch v{appVersion || "..."}
        </p>
      </section>

      {/* Divider */}
      <div className="border-t border-border border-dashed" />

      {/* About Section */}
      <section className="pb-2">
        <h2 className="text-xl font-medium mb-1">About Itch</h2>
        <p className="text-sm text-text-muted mb-4">
          Itch is a minimalist markdown notepad for capturing quick thoughts,
          todos, and ideas. It is offline-first, keyboard-optimized, and
          AI-compatible, with no cloud and no accounts. Itch is a private fork
          of Scratch by Eric Li.
        </p>
      </section>
    </div>
  );
}

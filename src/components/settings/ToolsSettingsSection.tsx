import { useState, useEffect, useReducer, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { toast } from "sonner";
import { Button, Input, Select } from "../ui";
import { SpinnerIcon } from "../icons";
import { mod } from "../../lib/platform";
import * as cliService from "../../services/cli";
import type { CliStatus } from "../../services/cli";
import {
  AiModelOptions,
  AiThinkingOptions,
  modelThinkingLevels,
  resolveAiDefaults,
  useAiModels,
} from "../ai/AiModelOptions";
import {
  AI_PROVIDERS,
  listAiProviders,
  providerName,
  resolveProvider,
  type AiProvider,
} from "../../services/inlineAi";
import type { Settings } from "../../types/note";

type CliState = {
  status: CliStatus | null;
  loaded: boolean;
  error: boolean;
  operating: boolean;
};

type CliAction =
  | { type: "loaded"; status: CliStatus }
  | { type: "error" }
  | { type: "operating" }
  | { type: "operated"; status: CliStatus }
  | { type: "operate_failed" };

const cliInitialState: CliState = {
  status: null,
  loaded: false,
  error: false,
  operating: false,
};

function cliReducer(state: CliState, action: CliAction): CliState {
  switch (action.type) {
    case "loaded":
      return { ...state, status: action.status, loaded: true, error: false };
    case "error":
      return { ...state, error: true };
    case "operating":
      return { ...state, operating: true };
    case "operated":
      return { ...state, status: action.status, operating: false };
    case "operate_failed":
      return { ...state, operating: false };
  }
}

function CliUsageHint() {
  return (
    <p className="text-sm text-text-muted font-mono">
      itch file.md # open note
      <br />
      itch . # open folder
      <br />
      itch # launch app
    </p>
  );
}

/** Sets or removes the entry of `provider` in a per-tool settings map. */
function withEntry(
  map: Record<string, string> | undefined,
  provider: AiProvider,
  value: string,
): Record<string, string> | undefined {
  const next = { ...map };
  if (value) next[provider] = value;
  else delete next[provider];
  return Object.keys(next).length > 0 ? next : undefined;
}

function AiSettings() {
  const [settings, setSettings] = useState<Settings | null>(null);
  const [installed, setInstalled] = useState<AiProvider[] | null>(null);
  const [piPath, setPiPath] = useState("");
  const provider =
    settings && installed ? resolveProvider(settings.aiProvider, installed) : null;
  const [models, retryModels] = useAiModels(provider);
  const defaults = resolveAiDefaults(models, {
    model: provider ? settings?.aiModels?.[provider] : undefined,
    thinking: provider ? settings?.aiThinking?.[provider] : undefined,
  });
  const levels = modelThinkingLevels(models, defaults.model);

  const findProviders = useCallback(() => {
    listAiProviders()
      .then(setInstalled)
      .catch((error) => {
        console.error("Failed to find the AI tools:", error);
        setInstalled([]);
      });
  }, []);

  useEffect(() => {
    invoke<Settings>("get_settings")
      .then((loaded) => {
        setSettings(loaded);
        setPiPath(loaded.piPath ?? "");
      })
      .catch((error) => console.error("Failed to load AI settings:", error));
    findProviders();
  }, [findProviders]);

  const save = useCallback(
    async (change: (current: Settings) => Partial<Settings>) => {
      try {
        const current = await invoke<Settings>("get_settings");
        const next = { ...current, ...change(current) };
        await invoke("update_settings", { newSettings: next });
        setSettings(next);
        toast.success("AI settings saved");
        return true;
      } catch (error) {
        console.error("Failed to save AI settings:", error);
        toast.error("Unable to save AI settings");
        return false;
      }
    },
    [],
  );

  const savePiPath = useCallback(async () => {
    const value = piPath.trim();
    if (value === (settings?.piPath ?? "")) return;
    if (await save(() => ({ piPath: value || undefined }))) {
      // Another pi can have other models, or pi can now be found.
      findProviders();
      if (provider === "pi") retryModels();
    }
  }, [piPath, settings, save, findProviders, provider, retryModels]);

  return (
    <div className="rounded-[10px] border border-border pl-4 py-3 pr-3 space-y-3">
      <div className="flex items-center justify-between gap-4">
        <label htmlFor="ai-provider" className="text-sm text-text font-medium">
          AI tool
        </label>
        <Select
          id="ai-provider"
          value={provider ?? ""}
          disabled={!provider}
          onChange={(e) => {
            // Read the value now: React resets the select before `save` reads it.
            const value = e.target.value;
            save(() => ({ aiProvider: value }));
          }}
          className="w-64"
        >
          {installed === null ? (
            <option value="">Loading…</option>
          ) : installed.length === 0 ? (
            <option value="">No AI tool found</option>
          ) : (
            AI_PROVIDERS.filter((p) => installed.includes(p.id)).map((p) => (
              <option key={p.id} value={p.id}>
                {p.name}
              </option>
            ))
          )}
        </Select>
      </div>
      {installed?.length === 0 && (
        <p className="text-xs text-text-muted">
          Install pi, Claude Code, Codex, OpenCode or Ollama to use AI in the
          editor.
        </p>
      )}
      {provider && (
        <>
          <div className="flex items-center justify-between gap-4">
            <label htmlFor="ai-model" className="text-sm text-text font-medium">
              Default model
            </label>
            <Select
              id="ai-model"
              value={defaults.model}
              onChange={(e) => {
                const value = e.target.value;
                save((current) => ({
                  aiModels: withEntry(current.aiModels, provider, value),
                }));
              }}
              aria-describedby={
                models.status === "error" ? "ai-model-error" : undefined
              }
              className="w-64"
            >
              {defaults.model && models.status !== "ready" && (
                <option value={defaults.model}>{defaults.model}</option>
              )}
              <AiModelOptions state={models} />
            </Select>
          </div>
          {models.status === "loading" && (
            <p className="flex items-center justify-end gap-2 text-xs text-text-muted">
              <SpinnerIcon className="w-3 h-3 animate-spin motion-reduce:animate-none" />
              Loading models from {providerName(provider)}
            </p>
          )}
          {models.status === "error" && (
            <div className="flex items-start justify-between gap-4 bg-red-500/10 rounded-md px-3 py-2">
              <p id="ai-model-error" className="text-xs text-red-500 break-words">
                {models.message}
              </p>
              <Button
                type="button"
                variant="outline"
                size="xs"
                onClick={retryModels}
              >
                Try again
              </Button>
            </div>
          )}
          {levels.length > 0 && (
            <div className="flex items-center justify-between gap-4">
              <label
                htmlFor="ai-thinking"
                className="text-sm text-text font-medium"
              >
                Default thinking
              </label>
              <Select
                id="ai-thinking"
                value={defaults.thinking}
                onChange={(e) => {
                  const value = e.target.value;
                  save((current) => ({
                    aiThinking: withEntry(current.aiThinking, provider, value),
                  }));
                }}
                className="w-64"
              >
                <AiThinkingOptions levels={levels} />
              </Select>
            </div>
          )}
        </>
      )}
      <div className="flex items-center justify-between gap-4">
        <label htmlFor="pi-path" className="text-sm text-text font-medium">
          pi path
        </label>
        <Input
          id="pi-path"
          value={piPath}
          onChange={(e) => setPiPath(e.target.value)}
          onBlur={savePiPath}
          placeholder="/usr/local/bin/pi"
          spellCheck={false}
          autoCapitalize="off"
          autoCorrect="off"
          className="w-64 h-9 font-mono text-xs"
        />
      </div>
      <p className="text-xs text-text-muted">
        The list shows only the AI tools that are installed. Each new AI
        conversation starts with the default model and thinking level. A change
        in the AI menu is only for that conversation. Default uses the setting
        of the AI tool. Lower thinking levels give faster answers. Leave the pi
        path empty to find pi on your PATH.
      </p>
    </div>
  );
}

export function ToolsSettingsSection() {
  const [cli, dispatchCli] = useReducer(cliReducer, cliInitialState);

  useEffect(() => {
    cliService
      .getCliStatus()
      .then((status) => dispatchCli({ type: "loaded", status }))
      .catch((err) => {
        console.error("Failed to get CLI status:", err);
        dispatchCli({ type: "error" });
      });
  }, []);

  const handleInstallCli = async () => {
    dispatchCli({ type: "operating" });
    try {
      await cliService.installCli();
      const status = await cliService.getCliStatus();
      dispatchCli({ type: "operated", status });
      toast.success(
        "CLI tool installed. Open a new terminal to use `itch`.",
      );
    } catch (err) {
      dispatchCli({ type: "operate_failed" });
      toast.error(
        err instanceof Error ? err.message : "Failed to install CLI tool",
      );
    }
  };

  const handleUninstallCli = async () => {
    dispatchCli({ type: "operating" });
    try {
      await cliService.uninstallCli();
      const status = await cliService.getCliStatus();
      dispatchCli({ type: "operated", status });
      toast.success("CLI tool uninstalled.");
    } catch (err) {
      dispatchCli({ type: "operate_failed" });
      toast.error(
        err instanceof Error ? err.message : "Failed to uninstall CLI tool",
      );
    }
  };

  return (
    <div className="space-y-8 py-8">
      {/* Inline AI */}
      <section className="pb-2">
        <h2 className="text-xl font-medium mb-0.5">Ask AI</h2>
        <p className="text-sm text-text-muted mb-4">
          Write and edit text with AI in the editor ({mod}J or the menu on a
          selection)
        </p>
        <AiSettings />
      </section>

      {/* CLI Tool (macOS only) */}
      {(cli.loaded && cli.status?.supported) || cli.error ? (
        <>
          <div className="border-t border-border border-dashed" />

          <section className="pb-2">
            <h2 className="text-xl font-medium mb-0.5">CLI Tool</h2>
            <p className="text-sm text-text-muted mb-4">
              Open notes from the terminal with the{" "}
              <code className="font-mono text-xs bg-bg-muted px-1.5 py-0.5 rounded">
                itch
              </code>{" "}
              command
            </p>

            {cli.error ? (
              <div className="bg-red-500/10 border border-red-500/20 rounded-md p-3">
                <p className="text-sm text-red-500">
                  Failed to check CLI status. Please restart the app.
                </p>
              </div>
            ) : cli.status === null ? (
              <div className="rounded-[10px] border border-border p-4 flex items-center justify-center">
                <SpinnerIcon className="w-4.5 h-4.5 stroke-[1.5] animate-spin text-text-muted" />
              </div>
            ) : cli.status.installed ? (
              <>
                <div className="rounded-[10px] border border-border p-4 space-y-3 mb-2.5">
                  <div className="flex items-center justify-between">
                    <span className="text-sm text-text font-medium">
                      Status
                    </span>
                    <span className="text-sm text-text-muted">Installed</span>
                  </div>
                  {cli.status.path && (
                    <div className="flex items-center justify-between">
                      <span className="text-sm text-text font-medium">
                        Path
                      </span>
                      <button
                        type="button"
                        className="text-xs font-mono text-text-muted bg-bg-muted px-2 py-0.5 rounded max-w-48 truncate cursor-pointer hover:bg-bg-hover transition-colors"
                        title="Click to copy path"
                        onClick={async () => {
                          try {
                            await invoke("copy_to_clipboard", { text: cli.status!.path! });
                            toast.success("Path copied to clipboard");
                          } catch {
                            toast.error("Failed to copy path");
                          }
                        }}
                      >
                        {cli.status.path}
                      </button>
                    </div>
                  )}
                  <div className="pt-3 border-t border-border border-dashed">
                    <CliUsageHint />
                  </div>
                </div>
                <Button
                  onClick={handleUninstallCli}
                  disabled={cli.operating}
                  variant="outline"
                  size="md"
                >
                  {cli.operating ? (
                    <>
                      <SpinnerIcon className="w-3.25 h-3.25 mr-2 animate-spin" />
                      Uninstalling...
                    </>
                  ) : (
                    "Uninstall CLI Tool"
                  )}
                </Button>
              </>
            ) : (
              <>
                <div className="flex items-center gap-2.5 p-2.5 rounded-[10px] border border-border bg-bg-secondary mb-2.5">
                  <CliUsageHint />
                </div>
                <Button
                  onClick={handleInstallCli}
                  disabled={cli.operating}
                  variant="outline"
                  size="md"
                >
                  {cli.operating ? (
                    <>
                      <SpinnerIcon className="w-3.25 h-3.25 mr-2 animate-spin" />
                      Installing...
                    </>
                  ) : (
                    "Install CLI Tool"
                  )}
                </Button>
              </>
            )}
          </section>
        </>
      ) : null}
    </div>
  );
}

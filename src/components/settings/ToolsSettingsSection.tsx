import { useState, useEffect, useReducer, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { toast } from "sonner";
import { Button, Input, Select } from "../ui";
import {
  SpinnerIcon,
  CheckIcon,
  ClaudeIcon,
  CodexIcon,
  OpenCodeIcon,
  OllamaIcon,
} from "../icons";
import { AI_PROVIDER_ORDER, type AiProvider } from "../../services/ai";
import * as aiService from "../../services/ai";
import { mod } from "../../lib/platform";
import * as cliService from "../../services/cli";
import type { CliStatus } from "../../services/cli";
import { PI_THINKING_LEVELS } from "../../services/pi";
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
      scratch file.md # open note
      <br />
      scratch . # open folder
      <br />
      scratch # launch app
    </p>
  );
}

const AI_PROVIDER_INFO: Record<
  AiProvider,
  {
    name: string;
    icon: React.ComponentType<{ className?: string }>;
    installUrl: string;
  }
> = {
  claude: {
    name: "Claude Code",
    icon: ClaudeIcon,
    installUrl: "https://code.claude.com/docs/en/quickstart",
  },
  codex: {
    name: "OpenAI Codex",
    icon: CodexIcon,
    installUrl: "https://github.com/openai/codex",
  },
  opencode: {
    name: "OpenCode",
    icon: OpenCodeIcon,
    installUrl: "https://opencode.ai",
  },
  ollama: {
    name: "Ollama",
    icon: OllamaIcon,
    installUrl: "https://ollama.com",
  },
};

type PiSettingKey = "piModel" | "piThinking" | "piPath";

function PiSettings() {
  const [values, setValues] = useState<Record<PiSettingKey, string>>({
    piModel: "",
    piThinking: "",
    piPath: "",
  });
  const [saved, setSaved] = useState(values);

  useEffect(() => {
    invoke<Settings>("get_settings")
      .then((settings) => {
        const loaded = {
          piModel: settings.piModel ?? "",
          piThinking: settings.piThinking ?? "",
          piPath: settings.piPath ?? "",
        };
        setValues(loaded);
        setSaved(loaded);
      })
      .catch((error) => console.error("Failed to load pi settings:", error));
  }, []);

  const save = useCallback(
    async (key: PiSettingKey, rawValue = values[key]) => {
      const value = rawValue.trim();
      if (value === saved[key]) return;
      try {
        const settings = await invoke<Settings>("get_settings");
        await invoke("update_settings", {
          newSettings: { ...settings, [key]: value || undefined },
        });
        setSaved((prev) => ({ ...prev, [key]: value }));
        toast.success("pi settings saved");
      } catch (error) {
        console.error("Failed to save pi settings:", error);
        toast.error("Unable to save pi settings");
      }
    },
    [values, saved],
  );

  return (
    <div className="rounded-[10px] border border-border pl-4 py-3 pr-3 space-y-3">
      <div className="flex items-center justify-between gap-4">
        <label htmlFor="pi-model" className="text-sm text-text font-medium">
          Model
        </label>
        <Input
          id="pi-model"
          value={values.piModel}
          onChange={(e) => setValues((v) => ({ ...v, piModel: e.target.value }))}
          onBlur={() => save("piModel")}
          placeholder="anthropic/claude-sonnet-4-5"
          spellCheck={false}
          autoCapitalize="off"
          autoCorrect="off"
          className="w-64 h-9 font-mono text-xs"
        />
      </div>
      <div className="flex items-center justify-between gap-4">
        <label htmlFor="pi-thinking" className="text-sm text-text font-medium">
          Thinking
        </label>
        <Select
          id="pi-thinking"
          value={values.piThinking}
          onChange={(e) => {
            const value = e.target.value;
            setValues((v) => ({ ...v, piThinking: value }));
            save("piThinking", value);
          }}
          className="w-64"
        >
          {PI_THINKING_LEVELS.map((opt) => (
            <option key={opt.value} value={opt.value}>
              {opt.label}
            </option>
          ))}
        </Select>
      </div>
      <div className="flex items-center justify-between gap-4">
        <label htmlFor="pi-path" className="text-sm text-text font-medium">
          pi path
        </label>
        <Input
          id="pi-path"
          value={values.piPath}
          onChange={(e) => setValues((v) => ({ ...v, piPath: e.target.value }))}
          onBlur={() => save("piPath")}
          placeholder="/usr/local/bin/pi"
          spellCheck={false}
          autoCapitalize="off"
          autoCorrect="off"
          className="w-64 h-9 font-mono text-xs"
        />
      </div>
      <p className="text-xs text-text-muted">
        Leave the model empty to use the default model of pi. Thinking is the
        start level for each new popover; you can also change it in the
        popover. Lower levels give faster answers. Leave the path empty to find
        pi on your PATH.
      </p>
    </div>
  );
}

export function ToolsSettingsSection() {
  const [cli, dispatchCli] = useReducer(cliReducer, cliInitialState);
  const [aiProviders, setAiProviders] = useState<AiProvider[]>([]);
  const [aiProvidersLoading, setAiProvidersLoading] = useState(true);

  useEffect(() => {
    cliService
      .getCliStatus()
      .then((status) => dispatchCli({ type: "loaded", status }))
      .catch((err) => {
        console.error("Failed to get CLI status:", err);
        dispatchCli({ type: "error" });
      });
  }, []);

  useEffect(() => {
    aiService
      .getAvailableAiProviders()
      .then(setAiProviders)
      .catch(() => setAiProviders([]))
      .finally(() => setAiProvidersLoading(false));
  }, []);

  const handleInstallCli = async () => {
    dispatchCli({ type: "operating" });
    try {
      await cliService.installCli();
      const status = await cliService.getCliStatus();
      dispatchCli({ type: "operated", status });
      toast.success(
        "CLI tool installed. Open a new terminal to use `scratch`.",
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
      {/* AI Providers */}
      <section className="pb-2">
        <h2 className="text-xl font-medium mb-0.5">AI Providers</h2>
        <p className="text-sm text-text-muted mb-4">
          Edit notes with AI from the command palette ({mod}P while editing a
          note)
        </p>

        {aiProvidersLoading ? (
          <div className="flex items-center gap-2 p-3">
            <SpinnerIcon className="w-4 h-4 animate-spin text-text-muted" />
            <span className="text-sm text-text-muted">
              Detecting installed providers...
            </span>
          </div>
        ) : (
          <div className="space-y-2">
            {AI_PROVIDER_ORDER.map((provider) => {
              const installed = aiProviders.includes(provider);
              const info = AI_PROVIDER_INFO[provider];
              return (
                <div
                  key={provider}
                  className="flex items-center justify-between p-3 rounded-[10px] border border-border"
                >
                  <div className="flex items-center gap-2.5">
                    <info.icon className="w-4.5 h-4.5 text-text-muted" />
                    <span className="text-sm font-medium">{info.name}</span>
                  </div>
                  {installed ? (
                    <span className="flex items-center gap-1.25 text-sm text-text-muted">
                      Installed
                      <span className="h-4.5 w-4.5 bg-bg-emphasis rounded-full flex items-center justify-center">
                        <CheckIcon className="w-3 h-3 stroke-[2.2]" />
                      </span>
                    </span>
                  ) : (
                    <a
                      href={info.installUrl}
                      target="_blank"
                      rel="noopener noreferrer"
                      className="text-sm text-text font-medium hover:text-text-muted transition-colors cursor-pointer"
                    >
                      Install
                    </a>
                  )}
                </div>
              );
            })}
          </div>
        )}
      </section>

      <div className="border-t border-border border-dashed" />

      {/* Inline AI (pi) */}
      <section className="pb-2">
        <h2 className="text-xl font-medium mb-0.5">Ask AI</h2>
        <p className="text-sm text-text-muted mb-4">
          Write and edit text with pi in the editor ({mod}J or the Ask AI
          button on a selection)
        </p>
        <PiSettings />
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
                scratch
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

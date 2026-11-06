import { useState } from "react";
import { toast } from "sonner";
import { useConnections } from "../../connections/ConnectionsContext";
import { connectors, getConnector } from "../../connections/registry";
import type { Connector } from "../../connections/types";
import { Button, Input } from "../ui";
import { CloudCheckIcon, SpinnerIcon } from "../icons";

export function AddConnectionForm({
  connector,
  onDone,
  className = "space-y-3 p-3 rounded-[10px] border border-border",
}: {
  connector: Connector;
  onDone: () => void;
  className?: string;
}) {
  const { addConnection } = useConnections();
  const [baseUrl, setBaseUrl] = useState(connector.defaultBaseUrl);
  const [token, setToken] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      const conn = await addConnection(connector.kind, baseUrl, token);
      toast.success(`Connected to ${conn.name}`);
      onDone();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <form onSubmit={submit} className={className}>
      <label className="block space-y-1">
        <span className="text-sm font-medium">Server URL</span>
        <Input value={baseUrl} onChange={(e) => setBaseUrl(e.target.value)} autoComplete="off" />
      </label>
      <label className="block space-y-1">
        <span className="text-sm font-medium">API key</span>
        <Input
          type="password"
          value={token}
          onChange={(e) => setToken(e.target.value)}
          autoComplete="off"
          autoFocus
        />
        <span className="block text-sm text-text-muted">{connector.tokenHelp}</span>
      </label>
      {error && (
        <p className="text-sm text-red-500" role="alert">
          {error}
        </p>
      )}
      <div className="flex gap-2">
        <Button type="submit" size="md" disabled={busy || !token.trim()} className="gap-1.25">
          {busy && <SpinnerIcon className="w-4 h-4 animate-spin" />}
          Connect
        </Button>
        <Button type="button" variant="ghost" size="md" onClick={onDone} disabled={busy}>
          Cancel
        </Button>
      </div>
    </form>
  );
}

export function ConnectionsSettingsSection() {
  const { connections, removeConnection } = useConnections();
  const [adding, setAdding] = useState<Connector | null>(null);

  return (
    <section className="pb-2">
      <h2 className="text-xl font-medium mb-0.5">Connections</h2>
      <p className="text-sm text-text-muted mb-4">
        Show documents from online workspaces in the sidebar and edit them here.
      </p>

      <div className="space-y-2">
        {connections.map((conn) => (
          <div
            key={conn.id}
            className="flex items-center justify-between p-3 rounded-[10px] border border-border"
          >
            <div className="flex items-center gap-2.5 min-w-0">
              <CloudCheckIcon className="w-4.5 h-4.5 text-text-muted shrink-0" />
              <span className="text-sm font-medium truncate">{conn.name}</span>
              <span className="text-sm text-text-muted truncate">
                {getConnector(conn.kind)?.label} · {new URL(conn.baseUrl).host}
              </span>
            </div>
            <Button
              variant="ghost"
              size="sm"
              onClick={() =>
                void removeConnection(conn.id).catch((err) =>
                  toast.error(`Could not remove the connection: ${err?.message ?? err}`),
                )
              }
            >
              Remove
            </Button>
          </div>
        ))}

        {adding ? (
          <AddConnectionForm connector={adding} onDone={() => setAdding(null)} />
        ) : (
          <div className="flex gap-2">
            {connectors.map((connector) => (
              <Button
                key={connector.kind}
                variant="outline"
                size="md"
                onClick={() => setAdding(connector)}
              >
                Add {connector.label}
              </Button>
            ))}
          </div>
        )}
      </div>
    </section>
  );
}

import { outlineConnector } from "./outline";
import type { Connector } from "./types";

/** All connectors. To add one, implement `Connector` in its own folder and list it here. */
export const connectors: Connector[] = [outlineConnector];

export function getConnector(kind: string): Connector | undefined {
  return connectors.find((connector) => connector.kind === kind);
}

import { documentKey, useConnections } from "../../connections/ConnectionsContext";
import { ListItem } from "../ui";

/** Search results from all connections, below the local results. */
export function ConnectionSearchResults() {
  const { searchResults, openDocument, active } = useConnections();
  if (searchResults.length === 0) return null;

  return (
    <div className="flex flex-col gap-0.5 p-1.5 border-t border-border first:border-t-0">
      {searchResults.map(({ conn, hit }) => (
        <ListItem
          key={`${conn.id}:${hit.docId}`}
          title={hit.title.trim() || "Untitled"}
          subtitle={hit.preview}
          meta={conn.name}
          isSelected={active?.key === documentKey(conn.id, hit.docId)}
          onClick={() => void openDocument(conn.id, hit.docId)}
        />
      ))}
    </div>
  );
}

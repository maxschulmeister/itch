# Connections

Connections show documents from online markdown platforms in the itch sidebar, next to local notes. You can open, edit, create, rename, move, and delete those documents. The first connector is [Outline](https://www.getoutline.com).

## Architecture

```
src-tauri/src/connections/   Generic transport. No connector-specific code.
  mod.rs                     Config store, Tauri commands, asset protocol.
  secrets.rs                 API tokens: OS keychain (release), file (debug).
src/connections/             Generic frontend layer.
  types.ts                   Connector interface and shared types.
  transport.ts               invoke() wrappers, 429 retry.
  registry.ts                List of connectors.
  merge.ts                   3-way text merge.
  ConnectionsContext.tsx     Trees, open document, save queue, polling.
src/connections/outline/     Everything Outline-specific.
src/components/connections/  Sidebar tree and settings UI.
```

To add a connector, add a folder `src/connections/<kind>/` that implements `Connector`, and add it to `registry.ts`. The Rust side does not change.

### Why connectors live in TypeScript

- The token never enters the webview. Rust adds it to each request (`connection_request`), and only sends requests to the base URL of the connection.
- Connector logic (tree mapping, patches, merges) is plain TypeScript. Vitest tests it without a Rust rebuild.
- Rust keeps the parts that need it: secrets, HTTP without CORS, multipart upload, and the authenticated image protocol.

### Storage

- `{APP_DATA}/connections.json`: id, kind, name, base URL for each connection.
- Tokens: OS keychain (`keyring`), service `itch-connection`, account = connection id. Debug builds use `{APP_DATA}/dev-secrets.json`, because macOS asks for keychain access after each rebuild.
- Images: `itch-conn://` protocol. Rust downloads the image with the token and caches it in `{APP_CACHE}/connections/<id>/`.

## Outline

### API use

| Need | Endpoint |
| --- | --- |
| Verify token | `auth.info` |
| Collections | `collections.list` |
| Document tree | `collections.documents` (titles only, one call for each collection) |
| Read | `documents.info` |
| Write body | `documents.update` with `editMode: "patch"`, `findText`, `lastRevision` |
| Write title | `documents.update` with `title`, `lastRevision` |
| Create | `documents.create` with `publish: true` (same as the Outline web app in a collection) |
| Move | `documents.move` |
| Delete | `documents.delete` (to trash) |
| Search | `documents.search` |
| Images | `attachments.create`, then upload to the returned URL |

### Writes

1. itch shows a document as `# Title` + body. On save, the first H1 becomes the title.
2. itch never sends the full body. It compares the base text from Outline with the new text, block by block. For each changed block, it sends one `patch` with the original Outline text of that block as `findText`. Unchanged blocks are not touched, so data that markdown cannot carry (comment anchors) stays.
3. Each write sends `lastRevision`. On `409`, itch reads the latest version, merges (base, mine, theirs), and tries again.

### Remote changes

- The open document: `documents.info` every 4 s. If the revision changed, the editor merges the remote text into its current text and keeps the cursor.
- Tree: `collections.documents` every 45 s for expanded collections.
- Outline sends API edits to open web editors at once (`APIUpdateExtension`), so itch edits show live in the web app.

### Markdown dialect

Outline syntax that itch must keep: `:::info|warning|tip|success … :::` notices, `+++` toggle blocks, `==highlight==`, `__underline__`, `\n` hard breaks, `@[Name](mention://…)` mentions, `/api/attachments.redirect?id=…` images, `/doc/<slug>-<urlId>` links.

### Not possible with the official API

Live co-editing (Y.js) needs a session token. Outline does not give it to API keys or OAuth apps (`server/routes/api/auth/auth.ts`). itch uses the API with patches and checks every few seconds instead.

## Decisions

- Connection folders show at the top of the sidebar list, in a separate tree component. Arrow keys work inside each tree. The local tree is not changed.
- The editor remounts when the source changes (local ↔ connection), because the extension set changes.
- Out of scope for v1: drag between local and a connection, live cursors, comments UI, OAuth, offline editing beyond "no lost edits".

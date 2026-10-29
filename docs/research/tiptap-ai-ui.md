# Tiptap AI and UI decision

## Findings

### AI Toolkit

**Confirmed.** There are two relevant paths. `@tiptap/ai-toolkit` is MIT and public on npm. It is the SDK for Tiptap’s Server AI Toolkit service. The service needs Tiptap access control JWTs and a Tiptap Cloud environment. The client package is `@tiptap-pro/client-ai-toolkit`; it is a paid add-on, distributed through Tiptap’s private npm registry. It needs a Tiptap account, paid access, and registry token. [Package source](https://github.com/ueberdosis/tiptap/tree/main/packages/ai-toolkit) · [npm](https://www.npmjs.com/package/@tiptap/ai-toolkit) · [Server install](https://tiptap.dev/docs/ai/ai-toolkit/install) · [Client install](https://tiptap.dev/docs/ai/ai-toolkit/client/install) · [Pro registry setup](https://tiptap.dev/docs/guides/pro-extensions)

Tiptap says that you can bring your own model and AI stack. The client toolkit runs document tools in the editor and supports custom integrations. Agent mode needs model function/tool calling. Fixed workflows, such as proofreading or rewriting, do not ask the model to choose tools. The client API reads JSON, HTML, or text; edit operations support text, HTML, or JSON and can stream. The documented formats do not list Markdown. The server API also supports Tiptap Shorthand. [Overview](https://tiptap.dev/docs/ai/ai-toolkit/overview) · [Agents](https://tiptap.dev/docs/ai/ai-toolkit/client/agents) · [Workflows](https://tiptap.dev/docs/ai/ai-toolkit/client/workflows) · [Client API](https://tiptap.dev/docs/ai/ai-toolkit/client/api-reference) · [Shorthand](https://tiptap.dev/docs/ai/ai-toolkit/advanced-guides/tiptap-shorthand)

**Inferred.** The client toolkit can likely call a local `pi` process through a custom adapter. Its document operations run in the editor, so they need not use Tiptap servers. Tiptap does not promise offline operation in its docs. Offline use depends on a licensed local package, local model, and local bridge. The Server AI Toolkit is not offline: it calls Tiptap’s service. The AI Toolkit price is not public; Tiptap says to contact sales. The pricing page does not show a usable Start-plan price. Model costs are separate. [AI Toolkit pricing/access](https://tiptap.dev/product/ai-toolkit) · [Pricing](https://tiptap.dev/pricing)

The older `@tiptap-pro/extension-ai` still appears in current AI Menu docs. Its examples need a Tiptap AI token and use AI Generation commands. It is a Pro/private-registry extension. I found no official notice that calls it deprecated. Do not treat it as the public, local toolkit. [AI Menu](https://tiptap.dev/docs/ui-components/components/ai-menu) · [Pro extensions](https://tiptap.dev/docs/guides/pro-extensions)

### Notion-like UI and licenses

**Confirmed.** The Notion-like template needs a Start plan for production. A Tiptap account with an active plan or trial is required to install it. Use `npx @tiptap/cli@latest add notion-like-editor`. It includes collaboration and AI integrations, server-issued JWTs, and Tiptap Cloud IDs/tokens. Its AI Menu uses the Pro AI extension. Image uploads need a separate server. The template is under Tiptap’s Terms and Pro License. [Template](https://tiptap.dev/docs/ui-components/templates/notion-like-editor) · [Pro License](https://tiptap.dev/pro-license)

The template does not require Tailwind. Tiptap UI styles use SCSS and the CLI adds a Sass compiler. You can integrate these styles with Tailwind. Current UI docs say components work best with React 18; React 19 compatibility is not confirmed. [Styles](https://tiptap.dev/docs/ui-components/getting-started/style) · [UI overview](https://tiptap.dev/docs/ui-components/getting-started/overview)

The Simple Editor template is all MIT. The docs mark MarkButton, HeadingDropdownMenu, Color Highlight, Link Popover, and Toolbar as open source. The AI Menu and AI Ask Button are not marked open source; their examples use `@tiptap-pro/extension-ai` and an AI token. Treat that built-in integration as paid. The Text Color Button is also not marked open source. [Simple Editor](https://tiptap.dev/docs/ui-components/templates/simple-editor) · [Component list](https://tiptap.dev/docs/ui-components/components/overview)

**Use without Cloud.** You can use the standalone MIT formatting components without Tiptap Cloud. You can also build an AI menu UI, but replace its Pro AI integration with your own bridge. Removing Cloud calls does not change the template’s Pro license.

**Legal limit.** Tiptap’s Pro License says it is for business users and does not offer use to consumers. A personal user should not use the Pro template or client toolkit under those published terms without written approval from Tiptap. [Pro License](https://tiptap.dev/pro-license) · [Terms](https://tiptap.dev/terms-of-service)

### BubbleMenu and recommendation

**Confirmed.** Tiptap v3 has an MIT BubbleMenu. In React, import `BubbleMenu` from `@tiptap/react/menus`; it uses the BubbleMenu functionality and does not need the separate extension added to the editor. The positioning is Floating UI-based. The v3.20 npm packages are MIT. [BubbleMenu docs](https://tiptap.dev/docs/editor/extensions/functionality/bubble-menu) · [npm extension](https://www.npmjs.com/package/@tiptap/extension-bubble-menu) · [npm React](https://www.npmjs.com/package/@tiptap/react)

**Decision.** For this personal app, use BubbleMenu and MIT UI components or write the selection menu. Call `pi` from Rust and apply its streamed result with editor commands. This avoids Tiptap Cloud, Pro access, and Pro-license limits. Costs are the app’s development and any model/provider cost; a local model may have no per-call provider fee.

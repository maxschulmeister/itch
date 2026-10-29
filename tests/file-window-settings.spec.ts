import { test as base, expect, type Browser } from "@playwright/test";
import { FILE_TEXT, Native } from "./native";

const test = base.extend<{ native: Native }>({
  native: async ({}, use) => {
    const native = new Native();
    await use(native);
    expect(native.unexpected).toEqual([]);
  },
});

test("file window shortcut opens the Settings window and keeps the file", async ({ page, native }) => {
  await native.openFileWindow(page);
  const editor = page.locator(".ProseMirror");
  await editor.click();

  await page.keyboard.press("ControlOrMeta+Comma");

  await expect.poll(() => native.count("open_settings_window")).toBe(1);
  await expect(editor).toBeVisible();
  await expect(editor).toBeFocused();
  await expect(editor).toContainText(FILE_TEXT);
});

test("file window toolbar button opens the Settings window and keeps the file", async ({ page, native }) => {
  await native.openFileWindow(page);
  const editor = page.locator(".ProseMirror");

  await page.getByRole("button", { name: "Settings" }).click();

  await expect.poll(() => native.count("open_settings_window")).toBe(1);
  await expect(editor).toBeVisible();
  await expect(editor).toContainText(FILE_TEXT);
});

test("file window keeps and saves a pending edit when Settings opens", async ({ page, native }) => {
  await native.openFileWindow(page);
  const editor = page.locator(".ProseMirror");
  await editor.getByText(FILE_TEXT).click();
  await page.keyboard.press("End");

  await page.keyboard.type(" Pending edit.");
  await page.keyboard.press("ControlOrMeta+Comma");

  await expect.poll(() => native.count("open_settings_window")).toBe(1);
  await expect(editor).toContainText(`${FILE_TEXT} Pending edit.`);
  await expect.poll(() => native.file.content).toContain(`${FILE_TEXT} Pending edit.`);
});

test("file window keeps and saves a pending source edit when Settings opens", async ({ page, native }) => {
  await native.openFileWindow(page);
  await page.locator(".ProseMirror").click();
  await page.keyboard.press("ControlOrMeta+Shift+M");
  const source = page.locator("textarea");
  await expect(source).toHaveValue(new RegExp(FILE_TEXT));

  await page.keyboard.type("Source edit. ");
  await page.keyboard.press("ControlOrMeta+Comma");

  await expect.poll(() => native.count("open_settings_window")).toBe(1);
  await expect(source).toBeFocused();
  await expect(source).toHaveValue(/Source edit\. /);
  await expect.poll(() => native.file.content).toContain("Source edit. ");
});

test("file window keeps the file and tells the user when Settings cannot open", async ({ page, native }) => {
  native.settingsWindowError = "Window creation failed";
  await native.openFileWindow(page);
  const editor = page.locator(".ProseMirror");

  await page.getByRole("button", { name: "Settings" }).click();

  await expect(page.getByText("Unable to open Settings")).toBeVisible();
  await expect(editor).toBeVisible();
  await expect(editor).toContainText(FILE_TEXT);
});

test("Settings window shows all tabs without Back and lists the AI tools", async ({ page, native }) => {
  await native.open(page, "/?mode=settings", "settings");
  const nav = page.getByRole("navigation");
  for (const tab of ["Folder", "Integrations", "Appearance", "Shortcuts", "About"]) {
    await expect(nav.getByRole("button", { name: new RegExp(tab) })).toBeVisible();
  }
  await expect(page.getByRole("button", { name: /^Back/ })).toHaveCount(0);

  await nav.getByRole("button", { name: /Integrations/ }).click();

  await expect(page.getByLabel("AI tool").locator("option")).toHaveText([
    "pi",
    "Claude Code",
    "Codex",
    "OpenCode",
    "Ollama",
  ]);
});

test("Settings window keeps the selected AI tool in a new Settings window", async ({ browser, native }) => {
  const openIntegrations = async () => {
    const page = await (await browser.newContext()).newPage();
    await native.open(page, "/?mode=settings", "settings");
    await page.getByRole("navigation").getByRole("button", { name: /Integrations/ }).click();
    return page.getByLabel("AI tool");
  };
  const aiTool = await openIntegrations();
  await expect(aiTool.locator("option:checked")).toHaveText("pi");

  await aiTool.selectOption({ label: "Codex" });
  await expect(aiTool.locator("option:checked")).toHaveText("Codex");
  await aiTool.page().close();

  await expect((await openIntegrations()).locator("option:checked")).toHaveText("Codex");
});

test.describe("Settings window changes", () => {
  test("apply to the open file window without a write back", async ({ context, native }) => {
    const fileWindow = await context.newPage();
    await native.openFileWindow(fileWindow);
    const settingsWindow = await context.newPage();
    await native.open(settingsWindow, "/?mode=settings", "settings");
    await settingsWindow.getByRole("navigation").getByRole("button", { name: /Appearance/ }).click();
    await native.settled();
    const writes = native.count("update_settings");

    await settingsWindow.getByRole("button", { name: "Dark" }).click();
    await settingsWindow
      .locator("label", { hasText: /^Font$/ })
      .locator("..")
      .getByRole("combobox")
      .selectOption({ label: "Serif" });
    await expect(settingsWindow.locator("html")).toHaveClass(/\bdark\b/);

    await expect(fileWindow.locator("html")).toHaveClass(/\bdark\b/);
    await expect(fileWindow.locator(".ProseMirror")).toHaveCSS("font-family", /Georgia/);
    await native.settled();
    expect(native.count("update_settings")).toBe(writes + 2);
  });

  test("zoom applies to the open file window without a write back", async ({ context, native }) => {
    const fileWindow = await context.newPage();
    await native.openFileWindow(fileWindow);
    const settingsWindow = await context.newPage();
    await native.open(settingsWindow, "/?mode=settings", "settings");
    await native.settled();
    const writes = native.count("update_settings");

    await settingsWindow.keyboard.press("ControlOrMeta+Equal");
    await expect
      .poll(() => settingsWindow.evaluate(() => document.documentElement.style.zoom))
      .toBe("1.05");

    await expect
      .poll(() => fileWindow.evaluate(() => document.documentElement.style.zoom))
      .toBe("1.05");
    await native.settled();
    expect(native.count("update_settings")).toBe(writes + 1);
  });
});

test.describe("Settings window library change", () => {
  // The library window has an edit that waits for the 300 ms save delay. Its
  // clock does not run, so only the library change can save the edit.
  async function openLibraryWithPendingEdit(browser: Browser, native: Native) {
    native.notesFolder = "/tmp/notes";
    native.notes = { draft: "# Draft\n\nLibrary text." };
    const mainWindow = await (await browser.newContext()).newPage();
    await native.open(mainWindow, "/", "main");
    await mainWindow.getByText("Library text.").click();
    const editor = mainWindow.locator(".ProseMirror");
    await expect(editor).toContainText("Library text.");
    const settingsWindow = await (await browser.newContext()).newPage();
    await native.open(settingsWindow, "/?mode=settings", "settings");
    await expect(settingsWindow.getByRole("button", { name: "Close Folder" })).toBeVisible();
    await native.settled();

    await mainWindow.clock.install();
    await editor.getByText("Library text.").click();
    await mainWindow.keyboard.press("End");
    await mainWindow.keyboard.type(" Pending edit.");
    await expect(editor).toContainText("Library text. Pending edit.");
    return { mainWindow, settingsWindow };
  }

  test("Close Folder saves the pending edit of the library window first", async ({ browser, native }) => {
    const { mainWindow, settingsWindow } = await openLibraryWithPendingEdit(browser, native);

    await settingsWindow.getByRole("button", { name: "Close Folder" }).click();

    await expect.poll(() => native.count("close_notes_folder")).toBe(1);
    expect(native.notes.draft).toContain("Pending edit.");
    expect(native.calls.lastIndexOf("save_note")).toBeLessThan(
      native.calls.indexOf("close_notes_folder"),
    );
    await expect(settingsWindow.getByRole("button", { name: "Choose Folder" })).toBeVisible();
    await expect(mainWindow.locator(".ProseMirror")).toHaveCount(0);
  });

  test("Change Folder saves the pending edit of the library window first", async ({ browser, native }) => {
    const { settingsWindow } = await openLibraryWithPendingEdit(browser, native);
    native.folderToChoose = "/tmp/other";

    await settingsWindow.getByRole("button", { name: "Change Folder" }).click();

    await expect.poll(() => native.count("set_notes_folder")).toBe(1);
    expect(native.notes.draft).toContain("Pending edit.");
    expect(native.calls.lastIndexOf("save_note")).toBeLessThan(
      native.calls.indexOf("set_notes_folder"),
    );
    await expect(settingsWindow.getByText("/tmp/other")).toBeVisible();
  });
});

test("Settings window leaves the note list and watcher to the library window", async ({ browser, native }) => {
  native.notesFolder = "/tmp/notes";
  native.notes = { draft: "# Draft\n\nLibrary text." };
  const mainWindow = await (await browser.newContext()).newPage();
  await native.open(mainWindow, "/", "main");
  await expect(mainWindow.getByText("Library text.")).toBeVisible();
  await native.settled();
  const notesLists = native.count("list_notes");
  const watcherStarts = native.count("start_file_watcher");

  const settingsWindow = await (await browser.newContext()).newPage();
  await native.open(settingsWindow, "/?mode=settings", "settings");
  await expect(settingsWindow.getByText("/tmp/notes")).toBeVisible();
  await native.settled();

  expect(native.count("list_notes")).toBe(notesLists);
  expect(native.count("start_file_watcher")).toBe(watcherStarts);
});

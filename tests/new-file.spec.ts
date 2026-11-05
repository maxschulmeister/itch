import { test as base, expect, type Page } from "@playwright/test";
import { FILE_TEXT, Native, UNTITLED_PATH } from "./native";

const test = base.extend<{ native: Native }>({
  native: async ({}, use) => {
    const native = new Native();
    await use(native);
    expect(native.unexpected).toEqual([]);
  },
});

const SAVE_PATH = "/tmp/Ideas.md";

async function typeInEditor(page: Page, text: string) {
  await page.locator(".ProseMirror").click();
  await page.keyboard.type(text);
}

async function closeWindow(page: Page) {
  await page.keyboard.press("ControlOrMeta+w");
}

test("start screen button opens an untitled file without a dialog", async ({ page, native }) => {
  await native.open(page, "/", "main");

  await page.getByRole("button", { name: "New File" }).click();

  await expect.poll(() => native.count("create_untitled_file")).toBe(1);
  expect(native.count("plugin:dialog|save")).toBe(0);
});

test("start screen shortcut opens an untitled file", async ({ page, native }) => {
  await native.open(page, "/", "main");
  await expect(page.getByRole("button", { name: "New File" })).toBeVisible();

  await page.keyboard.press("ControlOrMeta+N");

  await expect.poll(() => native.count("create_untitled_file")).toBe(1);
});

test("file window shortcut opens an untitled file", async ({ page, native }) => {
  await native.openFileWindow(page);

  await page.keyboard.press("ControlOrMeta+N");

  await expect.poll(() => native.count("create_untitled_file")).toBe(1);
});

test("save shortcut saves an untitled file at the path from the save dialog", async ({ page, native }) => {
  native.pathToSave = SAVE_PATH;
  await native.openUntitledWindow(page);
  await typeInEditor(page, "# Ideas");

  await page.keyboard.press("ControlOrMeta+S");

  await expect.poll(() => native.savedUntitled).toEqual([
    { source: UNTITLED_PATH, path: SAVE_PATH },
  ]);
  expect(native.saveDialogs[0].defaultPath).toBe("Ideas.md");
  // The edits are in the file before it moves.
  expect(native.file.content).toContain("# Ideas");

  await closeWindow(page);

  await expect.poll(() => native.count("plugin:window|destroy")).toBe(1);
  expect(native.messages).toEqual([]);
});

test("closing an untitled file with text asks to save, and Cancel keeps it open", async ({ page, native }) => {
  await native.openUntitledWindow(page);
  await typeInEditor(page, "Draft");

  await closeWindow(page);

  await expect.poll(() => native.count("cancel_quit")).toBe(1);
  expect(native.messages).toHaveLength(1);
  expect(native.messages[0].title).toBe("Do you want to save “Draft”?");
  expect(native.count("plugin:window|destroy")).toBe(0);
  expect(native.discarded).toEqual([]);
});

test("Don't Save discards the untitled file and closes the window", async ({ page, native }) => {
  native.messageReply = "Don’t Save";
  await native.openUntitledWindow(page);
  await typeInEditor(page, "Draft");

  await closeWindow(page);

  await expect.poll(() => native.count("plugin:window|destroy")).toBe(1);
  expect(native.discarded).toEqual([UNTITLED_PATH]);
  expect(native.savedUntitled).toEqual([]);
});

test("Save in the close question saves the untitled file and closes the window", async ({ page, native }) => {
  native.messageReply = "Save…";
  native.pathToSave = SAVE_PATH;
  await native.openUntitledWindow(page);
  await typeInEditor(page, "Draft");

  await closeWindow(page);

  await expect.poll(() => native.count("plugin:window|destroy")).toBe(1);
  expect(native.savedUntitled).toEqual([{ source: UNTITLED_PATH, path: SAVE_PATH }]);
  expect(native.file.content).toContain("Draft");
});

test("cancelled save dialog in the close question keeps the window open", async ({ page, native }) => {
  native.messageReply = "Save…";
  await native.openUntitledWindow(page);
  await typeInEditor(page, "Draft");

  await closeWindow(page);

  await expect.poll(() => native.count("cancel_quit")).toBe(1);
  expect(native.count("plugin:window|destroy")).toBe(0);
  expect(native.savedUntitled).toEqual([]);
});

test("closing an empty untitled file discards it without a question", async ({ page, native }) => {
  await native.openUntitledWindow(page);

  await closeWindow(page);

  await expect.poll(() => native.count("plugin:window|destroy")).toBe(1);
  expect(native.discarded).toEqual([UNTITLED_PATH]);
  expect(native.messages).toEqual([]);
});

test("closing a file window saves the edits without a question", async ({ page, native }) => {
  await native.openFileWindow(page);
  await page.locator(".ProseMirror").getByText(FILE_TEXT).click();
  await page.keyboard.press("End");
  await page.keyboard.type(" Edit.");

  await closeWindow(page);

  await expect.poll(() => native.count("plugin:window|destroy")).toBe(1);
  expect(native.file.content).toContain(`${FILE_TEXT} Edit.`);
  expect(native.messages).toEqual([]);
});

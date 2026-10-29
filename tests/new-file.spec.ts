import { test as base, expect } from "@playwright/test";
import { Native } from "./native";

const test = base.extend<{ native: Native }>({
  native: async ({}, use) => {
    const native = new Native();
    await use(native);
    expect(native.unexpected).toEqual([]);
  },
});

const NEW_PATH = "/tmp/Ideas.md";

test("start screen button creates a file at the path from the save dialog", async ({ page, native }) => {
  native.pathToSave = NEW_PATH;
  await native.open(page, "/", "main");

  await page.getByRole("button", { name: "New File…" }).click();

  await expect.poll(() => native.createdFiles).toEqual([NEW_PATH]);
});

test("start screen shortcut creates a file", async ({ page, native }) => {
  native.pathToSave = NEW_PATH;
  await native.open(page, "/", "main");
  await expect(page.getByRole("button", { name: "New File…" })).toBeVisible();

  await page.keyboard.press("ControlOrMeta+N");

  await expect.poll(() => native.createdFiles).toEqual([NEW_PATH]);
});

test("file window shortcut creates a file", async ({ page, native }) => {
  native.pathToSave = NEW_PATH;
  await native.openFileWindow(page);

  await page.keyboard.press("ControlOrMeta+N");

  await expect.poll(() => native.createdFiles).toEqual([NEW_PATH]);
});

test("cancelled save dialog creates no file", async ({ page, native }) => {
  await native.open(page, "/", "main");

  await page.getByRole("button", { name: "New File…" }).click();

  await expect.poll(() => native.count("plugin:dialog|save")).toBe(1);
  expect(native.createdFiles).toEqual([]);
});

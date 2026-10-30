import { test as base, expect } from "@playwright/test";
import { FILE_TEXT, Native } from "./native";

const test = base.extend<{ native: Native }>({
  native: async ({}, use) => {
    const native = new Native();
    await use(native);
    expect(native.unexpected).toEqual([]);
  },
});

const asset = (path: string) => `asset://localhost/${encodeURIComponent(path)}`;

test("file window shows images next to the file and saves their paths unchanged", async ({ page, native }) => {
  native.file.content = [
    "# Note",
    "",
    FILE_TEXT,
    "",
    "![Overview](00-overview.png)",
    "",
    "![Up](../shared/logo.png)",
    "",
    "![Web](https://example.com/a.png)",
    "",
  ].join("\n");
  await native.openFileWindow(page);
  const editor = page.locator(".ProseMirror");

  await expect(editor.getByRole("img", { name: "Overview" })).toHaveAttribute(
    "src",
    asset("/tmp/00-overview.png"),
  );
  await expect(editor.getByRole("img", { name: "Up" })).toHaveAttribute("src", asset("/shared/logo.png"));
  await expect(editor.getByRole("img", { name: "Web" })).toHaveAttribute(
    "src",
    "https://example.com/a.png",
  );

  await editor.getByText(FILE_TEXT).click();
  await page.keyboard.press("End");
  await page.keyboard.type(" Edit.");

  await expect.poll(() => native.file.content).toContain(`${FILE_TEXT} Edit.`);
  expect(native.file.content).toContain("![Overview](00-overview.png)");
  expect(native.file.content).toContain("![Up](../shared/logo.png)");
  expect(native.file.content).toContain("![Web](https://example.com/a.png)");
});

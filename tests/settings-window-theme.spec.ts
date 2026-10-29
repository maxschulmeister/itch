import { test as base, expect, type Page } from "@playwright/test";
import { Native } from "./native";

const test = base.extend<{ native: Native }>({
  native: async ({}, use) => {
    const native = new Native();
    await use(native);
    expect(native.unexpected).toEqual([]);
  },
});

/** The RGB color of a 1×1 PNG screenshot. */
function rgb(page: Page, png: Buffer) {
  return page.evaluate(async (base64) => {
    const blob = await (await fetch(`data:image/png;base64,${base64}`)).blob();
    const canvas = new OffscreenCanvas(1, 1);
    const context = canvas.getContext("2d")!;
    context.drawImage(await createImageBitmap(blob), 0, 0);
    return Array.from(context.getImageData(0, 0, 1, 1).data.slice(0, 3));
  }, png.toString("base64"));
}

// The backend creates the Settings window hidden and shows it only on
// show_settings_window. These tests check when the page asks for that and what
// the page shows at that moment. They cannot check that the backend creates
// the window hidden.
test("Settings window with the dark theme is shown only when it shows the dark theme", async ({
  page,
  native,
}) => {
  await page.emulateMedia({ colorScheme: "light" });
  native.settings = { theme: { mode: "dark" } };
  const releaseSettings = native.holdSettings();

  await native.open(page, "/?mode=settings", "settings");
  await expect.poll(() => native.count("get_settings")).toBeGreaterThan(0);
  await page.waitForLoadState("networkidle");
  expect(native.shows).toEqual([]);

  releaseSettings();

  await expect.poll(() => native.shows.length).toBe(1);
  const [shown] = native.shows;
  expect(shown.background).toEqual([22, 20, 19]);
  expect((await rgb(page, shown.pixel)).every((channel) => channel < 64)).toBe(true);
});

/** Opens the Settings window and returns the first `show_settings_window` call. */
async function openSettings(page: Page, native: Native) {
  await native.open(page, "/?mode=settings", "settings");
  await expect.poll(() => native.shows.length).toBe(1);
  const [shown] = native.shows;
  return { background: shown.background, pixel: await rgb(page, shown.pixel) };
}

/** Sends `focus-settings` to the window like the backend does. */
function emitFocusSettings(page: Page) {
  return page.evaluate(() =>
    (window as any).__TAURI_INTERNALS__.invoke("plugin:event|emit", {
      event: "focus-settings",
      payload: null,
    }),
  );
}

test("Settings window with the light theme is shown light when the OS is dark", async ({
  page,
  native,
}) => {
  await page.emulateMedia({ colorScheme: "dark" });
  native.settings = { theme: { mode: "light" } };

  const shown = await openSettings(page, native);

  expect(shown.background).toEqual([255, 255, 255]);
  expect(shown.pixel.every((channel) => channel > 192)).toBe(true);
});

test("Settings window with the system theme is shown dark when the OS is dark", async ({
  page,
  native,
}) => {
  await page.emulateMedia({ colorScheme: "dark" });
  native.settings = { theme: { mode: "system" } };

  const shown = await openSettings(page, native);

  expect(shown.background).toEqual([22, 20, 19]);
  expect(shown.pixel.every((channel) => channel < 64)).toBe(true);
});

test("Settings window is shown with the custom dark background", async ({ page, native }) => {
  await page.emulateMedia({ colorScheme: "light" });
  native.settings = { theme: { mode: "dark" }, customColorsDark: { bg: "#203040" } };

  const shown = await openSettings(page, native);

  expect(shown.background).toEqual([32, 48, 64]);
});

test("Settings window is shown again on focus-settings only after it loads, with the current theme", async ({
  page,
  native,
}) => {
  await page.emulateMedia({ colorScheme: "light" });
  native.settings = { theme: { mode: "dark" } };
  const releaseSettings = native.holdSettings();
  await native.open(page, "/?mode=settings", "settings");
  await expect.poll(() => native.count("get_settings")).toBeGreaterThan(0);
  await page.waitForLoadState("networkidle");

  await emitFocusSettings(page);
  expect(native.shows).toEqual([]);

  releaseSettings();
  await expect.poll(() => native.shows.length).toBe(1);
  await page.getByRole("navigation").getByRole("button", { name: /Appearance/ }).click();
  await page.getByRole("button", { name: "Light" }).click();
  await expect(page.locator("html")).not.toHaveClass(/\bdark\b/);

  await emitFocusSettings(page);

  await expect.poll(() => native.shows.length).toBe(2);
  expect(native.shows[1].background).toEqual([255, 255, 255]);
  expect((await rgb(page, native.shows[1].pixel)).every((channel) => channel > 192)).toBe(true);
});

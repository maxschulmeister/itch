import { defineConfig } from "@playwright/test";

const baseURL = "http://127.0.0.1:1422";

export default defineConfig({
  testDir: "tests",
  use: { baseURL, browserName: "chromium" },
  webServer: {
    command: "npm run dev -- --host 127.0.0.1 --port 1422",
    url: baseURL,
    reuseExistingServer: !process.env.CI,
  },
});

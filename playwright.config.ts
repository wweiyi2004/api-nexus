import { defineConfig } from "@playwright/test";

process.env.NO_PROXY = "127.0.0.1,localhost";
process.env.no_proxy = "127.0.0.1,localhost";

export default defineConfig({
  testDir: "./tests",
  timeout: 30_000,
  use: {
    baseURL: "http://127.0.0.1:1422",
    trace: "retain-on-failure",
  },
  webServer: {
    command: "npm.cmd run dev -- --host=127.0.0.1 --port=1422",
    url: "http://127.0.0.1:1422",
    reuseExistingServer: true,
    timeout: 120_000,
  },
});

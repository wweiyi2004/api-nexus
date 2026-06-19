import { expect, test } from "@playwright/test";

test.beforeEach(async ({ page }) => {
  await page.addInitScript(() => {
    const config = {
      providers: [],
      proxy_port: 11434,
      proxy_host: "127.0.0.1",
      auto_start: true,
      proxy_api_key: "sk-nexus-test",
      proxy_api_keys: [{ id: "default", name: "默认密钥", key: "sk-nexus-test", enabled: true }],
      model_aliases: [],
      model_prices: [],
      usd_to_cny_rate: 7.2,
      log_retention_days: 30,
      max_log_entries: 10000,
    };
    (window as unknown as { __TAURI_INTERNALS__: unknown }).__TAURI_INTERNALS__ = {
      invoke: async (command: string) => {
        if (command === "get_server_status") {
          return { running: false, port: 11434, host: "127.0.0.1", url: "http://127.0.0.1:11434" };
        }
        if (command === "get_config" || command === "save_config_cmd") return config;
        if (command === "get_token_stats") {
          return {
            request_count: 0,
            input_tokens: 0,
            output_tokens: 0,
            cached_tokens: 0,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
          };
        }
        if (command === "get_request_logs") return [];
        return null;
      },
    };
  });
});

test("navigates through dashboard, logs and settings", async ({ page }) => {
  await page.goto("/");
  await expect(page.getByRole("heading", { name: "本地 API 网关" })).toBeVisible();

  await page.getByRole("link", { name: "请求日志" }).click();
  await expect(page.getByRole("heading", { name: "请求日志" })).toBeVisible();
  await expect(page.getByText("暂无请求记录")).toBeVisible();

  await page.getByRole("link", { name: "设置" }).click();
  await expect(page.getByRole("heading", { name: "设置" })).toBeVisible();
  await expect(page.getByText("日志持久化")).toBeVisible();
  await expect(page.getByText("应用更新")).toBeVisible();
});

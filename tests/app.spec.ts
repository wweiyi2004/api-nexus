import { expect, test } from "@playwright/test";

test.beforeEach(async ({ page }) => {
  await page.addInitScript(() => {
    let config = {
      providers: [
        {
          id: "provider-a",
          name: "OpenAI Route",
          protocol: "openai",
          base_url: "https://a.example.com",
          api_key: "",
          models: ["shared-model", "other-model"],
          enabled: true,
          priority: 0,
        },
        {
          id: "provider-b",
          name: "Other Route",
          protocol: "openai",
          base_url: "https://b.example.com",
          api_key: "",
          models: ["other-model"],
          enabled: true,
          priority: 1,
        },
        {
          id: "provider-c",
          name: "Anthropic Route",
          protocol: "anthropic",
          base_url: "https://c.example.com",
          api_key: "",
          models: ["shared-model"],
          enabled: true,
          priority: 2,
        },
      ],
      proxy_port: 11434,
      proxy_host: "127.0.0.1",
      auto_start: true,
      proxy_api_key: "sk-nexus-test",
      proxy_api_keys: [{ id: "default", name: "默认密钥", key: "sk-nexus-test", enabled: true }],
      model_aliases: [],
      model_routes: [
        { model: "shared-model", provider_ids: ["provider-a", "provider-c"] },
        { model: "other-model", provider_ids: ["provider-b", "provider-a"] },
      ],
      model_prices: [],
      usd_to_cny_rate: 7.2,
      log_retention_days: 30,
      max_log_entries: 10000,
    };
    Object.defineProperty(window, "__TEST_CONFIG__", { get: () => config });
    (window as unknown as { __TAURI_INTERNALS__: unknown }).__TAURI_INTERNALS__ = {
      invoke: async (command: string, args?: { config?: typeof config }) => {
        if (command === "get_server_status") {
          return { running: false, port: 11434, host: "127.0.0.1", url: "http://127.0.0.1:11434" };
        }
        if (command === "get_config") return config;
        if (command === "save_config_cmd") {
          config = args?.config ?? config;
          return config;
        }
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

test("reorders only the selected model route", async ({ page }) => {
  await page.goto("/models");
  await expect(page.getByRole("heading", { name: "模型路由" })).toBeVisible();

  const sharedModel = page.locator(".data-row").filter({ hasText: "shared-model" });
  const routes = sharedModel.getByTestId("provider-route");
  await expect(routes).toHaveCount(2);
  await expect(routes.nth(0)).toContainText("OpenAI Route");
  const otherModel = page.locator(".data-row").filter({ hasText: "other-model" });
  await expect(otherModel.getByTestId("provider-route").nth(0)).toContainText("Other Route");

  await routes.nth(1).dragTo(routes.nth(0));

  await expect(sharedModel.getByTestId("provider-route").nth(0)).toContainText("Anthropic Route");
  await expect(otherModel.getByTestId("provider-route").nth(0)).toContainText("Other Route");
  await expect(sharedModel.getByText("正在保存")).toHaveCount(0);

  const savedRoutes = await page.evaluate(() =>
    (window as unknown as {
      __TEST_CONFIG__: { model_routes: Array<{ model: string; provider_ids: string[] }> };
    }).__TEST_CONFIG__.model_routes,
  );
  expect(savedRoutes).toEqual([
    { model: "shared-model", provider_ids: ["provider-c", "provider-a"] },
    { model: "other-model", provider_ids: ["provider-b", "provider-a"] },
  ]);
});

import { describe, expect, it } from "vitest";
import {
  buildTrendData,
  calculateLogCost,
  countLogTokens,
  type ModelPrice,
  type RequestLogEntry,
} from "./Logs";

const price: ModelPrice = {
  model: "test-model",
  input_usd_per_million: 10,
  output_usd_per_million: 20,
  cached_usd_per_million: 2,
  cache_read_usd_per_million: 2,
  cache_write_usd_per_million: 12,
};

function log(overrides: Partial<RequestLogEntry> = {}): RequestLogEntry {
  return {
    timestamp: Math.floor(Date.now() / 1000) - 60,
    method: "POST",
    path: "/v1/messages",
    model: "test-model",
    provider: "provider",
    api_key_name: "client-a",
    status: 200,
    input_tokens: 1_000_000,
    output_tokens: 200_000,
    cached_tokens: 400_000,
    cache_read_tokens: 400_000,
    cache_write_tokens: 0,
    duration_ms: 100,
    error: null,
    ...overrides,
  };
}

describe("request log analytics", () => {
  it("does not double charge OpenAI cached prompt tokens", () => {
    const result = calculateLogCost(log(), price, "openai", 7);
    expect(result.usd).toBeCloseTo(10.8);
    expect(result.cny).toBeCloseTo(75.6);
    expect(countLogTokens(log(), "openai")).toBe(1_200_000);
  });

  it("prices Anthropic cache reads and writes independently", () => {
    const anthropic = log({
      input_tokens: 600_000,
      cache_read_tokens: 400_000,
      cache_write_tokens: 100_000,
      cached_tokens: 500_000,
    });
    expect(calculateLogCost(anthropic, price, "anthropic").usd).toBeCloseTo(12);
    expect(countLogTokens(anthropic, "anthropic")).toBe(1_300_000);
  });

  it("builds per-key rankings using the selected metric", () => {
    const logs = [
      log({ api_key_name: "client-a" }),
      log({ api_key_name: "client-a", timestamp: Math.floor(Date.now() / 1000) - 30 }),
      log({ api_key_name: "client-b", status: 500, error: "failed" }),
    ];
    const data = buildTrendData(logs, "1h", "requests", () => null, (entry) => entry.input_tokens);
    expect(data.activeKeys).toBe(2);
    expect(data.rankings[0]).toMatchObject({ name: "client-a", value: 2, requests: 2 });
    expect(data.buckets.reduce((sum, bucket) => sum + bucket.total, 0)).toBe(3);
  });
});

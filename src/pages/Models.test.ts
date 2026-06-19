import { describe, expect, it } from "vitest";
import { reorderProvidersByRoute } from "./Models";

const providers = [
  {
    id: "a",
    name: "A",
    protocol: "openai" as const,
    base_url: "https://a.example.com",
    api_key: "",
    models: ["shared-model"],
    enabled: true,
    priority: 0,
  },
  {
    id: "b",
    name: "B",
    protocol: "openai" as const,
    base_url: "https://b.example.com",
    api_key: "",
    models: ["other-model"],
    enabled: true,
    priority: 1,
  },
  {
    id: "c",
    name: "C",
    protocol: "anthropic" as const,
    base_url: "https://c.example.com",
    api_key: "",
    models: ["shared-model"],
    enabled: true,
    priority: 2,
  },
];

describe("reorderProvidersByRoute", () => {
  it("moves a provider while preserving unrelated provider positions", () => {
    const result = reorderProvidersByRoute(providers, ["a", "c"], "c", "a");
    const orderedIds = [...result]
      .sort((a, b) => a.priority - b.priority)
      .map((provider) => provider.id);

    expect(orderedIds).toEqual(["c", "b", "a"]);
  });

  it("does not change priorities for an invalid drop", () => {
    const result = reorderProvidersByRoute(providers, ["a", "c"], "a", "missing");

    expect(result).toBe(providers);
  });
});

import { describe, expect, it } from "vitest";
import { reorderModelRoutes } from "./Models";

const routes = [
  { model: "shared-model", provider_ids: ["a", "c"] },
  { model: "other-model", provider_ids: ["b", "a"] },
];

describe("reorderModelRoutes", () => {
  it("moves a provider for only the selected model", () => {
    const result = reorderModelRoutes(
      routes,
      "shared-model",
      ["a", "c"],
      "c",
      "a",
    );

    expect(result[0].provider_ids).toEqual(["c", "a"]);
    expect(result[1]).toEqual(routes[1]);
  });

  it("does not change priorities for an invalid drop", () => {
    const result = reorderModelRoutes(
      routes,
      "shared-model",
      ["a", "c"],
      "a",
      "missing",
    );

    expect(result).toBe(routes);
  });
});

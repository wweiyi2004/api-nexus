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

  it("can insert a moved provider after the drop target", () => {
    const result = reorderModelRoutes(
      routes,
      "other-model",
      ["b", "a"],
      "b",
      "a",
      "after",
    );

    expect(result[0]).toEqual(routes[0]);
    expect(result[1].provider_ids).toEqual(["a", "b"]);
  });

  it("keeps before and after drops distinct when moving downward", () => {
    const threeRoutes = [{ model: "m", provider_ids: ["a", "b", "c"] }];

    expect(
      reorderModelRoutes(threeRoutes, "m", ["a", "b", "c"], "a", "c", "before")[0]
        .provider_ids,
    ).toEqual(["b", "a", "c"]);
    expect(
      reorderModelRoutes(threeRoutes, "m", ["a", "b", "c"], "a", "c", "after")[0]
        .provider_ids,
    ).toEqual(["b", "c", "a"]);
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

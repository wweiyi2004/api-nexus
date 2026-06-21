import { describe, expect, it } from "vitest";
import tauriConfigJson from "../../src-tauri/tauri.conf.json?raw";

describe("tauri window config", () => {
  it("disables webview drag-drop so HTML5 route reordering works", () => {
    const config = JSON.parse(tauriConfigJson);

    expect(config.app.windows[0].dragDropEnabled).toBe(false);
  });
});

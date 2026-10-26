import assert from "node:assert/strict";
import { test } from "node:test";
import { readFileSync } from "node:fs";

test("macOS declares CSV Editor file association", () => {
  const conf = JSON.parse(readFileSync(new URL("../src-tauri/tauri.conf.json", import.meta.url), "utf8"));
  assert.deepEqual(conf.bundle.fileAssociations, [{
    ext: ["csv"], name: "CSV Document", description: "CSV document",
    role: "Editor", contentTypes: ["public.comma-separated-values-text"], mimeType: "text/csv",
  }]);
});

test("default capability covers main and document windows", () => {
  const capability = JSON.parse(readFileSync(new URL("../src-tauri/capabilities/default.json", import.meta.url), "utf8"));
  assert.deepEqual(capability.windows, ["main", "doc-*"]);
});

test("window close is scoped instead of quitting the process", () => {
  const app = readFileSync(new URL("../frontend/js/app.js", import.meta.url), "utf8");
  assert.match(app, /createWindowCloseCoordinator/);
  assert.equal(app.includes('invoke("quit_app"'), false);
});

test("single-instance stays pinned to 2.3.4", () => {
  const manifest = readFileSync(new URL("../src-tauri/Cargo.toml", import.meta.url), "utf8");
  const lock = readFileSync(new URL("../src-tauri/Cargo.lock", import.meta.url), "utf8");
  assert.ok(manifest.includes("tauri-plugin-single-instance = \"=2.3.4\""));
  assert.ok(lock.includes("name = \"tauri-plugin-single-instance\"\nversion = \"2.3.4\""));
});

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

test("single-instance stays pinned to 2.3.4", () => {
  const manifest = readFileSync(new URL("../src-tauri/Cargo.toml", import.meta.url), "utf8");
  const lock = readFileSync(new URL("../src-tauri/Cargo.lock", import.meta.url), "utf8");
  assert.ok(manifest.includes("tauri-plugin-single-instance = \"=2.3.4\""));
  assert.ok(lock.includes("name = \"tauri-plugin-single-instance\"\nversion = \"2.3.4\""));
});

import assert from "node:assert/strict";
import { test } from "node:test";
import { readFileSync } from "node:fs";

test("macOS declares CSV, TXT and LOG as optional Editor associations", () => {
  const confText = readFileSync(new URL("../src-tauri/tauri.conf.json", import.meta.url), "utf8");
  const conf = JSON.parse(confText);
  assert.deepEqual(conf.bundle.fileAssociations, [
    {
      ext: ["csv"], name: "CSV Document", description: "CSV document",
      role: "Editor", contentTypes: ["public.comma-separated-values-text"], mimeType: "text/csv",
    },
    {
      ext: ["txt"], name: "Text Document", description: "Plain text document",
      role: "Editor", contentTypes: ["public.plain-text"], mimeType: "text/plain",
    },
    {
      ext: ["log"], name: "Log Document", description: "Log document",
      role: "Editor", contentTypes: ["public.plain-text"], mimeType: "text/plain",
    },
  ]);
  assert.doesNotMatch(confText, /LSSetDefault|defaultHandler|defaultApplication/i);
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
  assert.match(lock, /name = "tauri-plugin-single-instance"\r?\nversion = "2\.3\.4"/);
});

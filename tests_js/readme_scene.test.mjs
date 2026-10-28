import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { createReadmeFixtures, parseReadmeMode } from "../frontend/js/readme_scene.js";

const read = (path) => readFileSync(new URL(`../${path}`, import.meta.url), "utf8");

test("readme modes require a scene and an opaque filesystem-safe token", () => {
  assert.deepEqual(parseReadmeMode("readme-main-0123456789abcdef"),
    { scene: "main", token: "0123456789abcdef" });
  assert.equal(parseReadmeMode("readme-split-0123456789abcdef").scene, "split");
  assert.equal(parseReadmeMode("readme-text-0123456789abcdef").scene, "text");
  for (const bad of ["readme-main-short", "readme-main-../12345678", "idlebig"]) {
    assert.throws(() => parseReadmeMode(bad));
  }
});

test("fixtures use clean names and preserve literal backslashes", () => {
  const token = "0123456789abcdef";
  const dir = "C:/Temp/simpletable-readme-" + token;
  const files = createReadmeFixtures("C:/Temp", token);
  assert.deepEqual(files.map((file) => file.path), [
    dir + "/release-plan.csv",
    dir + "/release-checklist.csv",
    dir + "/server.log",
    dir + "/notes.txt",
  ]);
  const main = files[0];
  const log = files[2];
  const slash = String.fromCharCode(92);
  assert.ok(main.text.startsWith('迭代,模块,状态,发布备注\r\n'));
  assert.ok(main.text.includes('\r\n'));
  assert.ok(main.text.includes('"'));
  assert.ok(log.text.includes('\r\n\r\n'));
  assert.ok(log.text.includes("C:" + slash + "demo" + slash + "release"));
  assert.ok(log.text.includes("反斜杠" + slash + "均按字面显示"));
  for (const file of files) assert.doesNotMatch(file.path + file.text, /price_j8888/);
});

test("scenes fill the frame and leave useful content in the split editor", () => {
  const files = createReadmeFixtures("C:/Temp", "0123456789abcdef");
  const [main, checklist, log] = files;
  assert.ok((main.text.match(/^[12]\.\d/gm) ?? []).length >= 30);
  assert.ok((checklist.text.match(/\r\n/g) ?? []).length >= 31);
  assert.ok(log.text.split("\r\n").length >= 30);
  const scene = read("frontend/js/readme_scene.js");
  assert.ok(scene.includes("grid.select([0, 3]);"));
});

test("park routes readme scenes before legacy global markers", () => {
  const park = read("frontend/js/park.js");
  assert.match(park, /mode\.startsWith\("readme-"\)/);
  assert.match(park, /runReadmeScene/);
  assert.ok(park.indexOf('mode.startsWith("readme-")') < park.indexOf('const dir = await invoke("smoke_tmp_dir")'));
});

test("capture isolates fixtures and normalizes real DPI to 1440 by 900", () => {
  const script = read("scripts/shot_readme.ps1");
  assert.doesNotMatch(script, /taskkill|Get-Process\s+simpletable|Stop-Process|CopyFromScreen/);
  assert.match(script, /\$proc\.Kill\(\)/);
  assert.match(script, /NewGuid/);
  assert.match(script, /--park/);
  assert.match(script, /PrintWindow/);
  assert.match(script, /GetClientRect/);
  assert.match(script, /ClientToScreen/);
  assert.match(script, /DrawImage/);
  assert.match(script, /GetDpiForWindow/);
  assert.match(script, /\$captureWidth/);
  assert.match(script, /InterpolationMode/);
  assert.match(script, /New-Item -ItemType Directory -Force -Path \$fixtureDir/);
  assert.match(script, /Remove-Item -LiteralPath \$fixtureDir(?! -Recurse)/);
});

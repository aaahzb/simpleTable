import test from "node:test";
import assert from "node:assert/strict";
import {
  LARGE_TEXT_BYTES, PASTE_CHUNK_BYTES,
  utf8ByteLength, utf8Chunks, bytesToHex, isSingleValueText,
} from "../frontend/js/clipboard.js";

test("UTF-8 分块不拆代理对且 hex 字符串不超过 1 MiB", () => {
  const text = `a中😀b\r\n${"😀".repeat(200000)}`;
  let joined = "";
  let count = 0;
  for (const chunk of utf8Chunks(text, PASTE_CHUNK_BYTES)) {
    assert.ok(chunk.bytes.length <= PASTE_CHUNK_BYTES);
    assert.ok(bytesToHex(chunk.bytes).length <= 1024 * 1024);
    assert.equal(new TextDecoder().decode(chunk.bytes), text.slice(chunk.start, chunk.end));
    joined += text.slice(chunk.start, chunk.end);
    count += 1;
  }
  assert.equal(joined, text);
  assert.equal(utf8ByteLength(text), new TextEncoder().encode(text).length);
  assert.ok(count > 1);
});

test("大体量探测不构造完整二维数组", () => {
  const text = "中".repeat(3 * 1024 * 1024);
  assert.ok(utf8ByteLength(text, LARGE_TEXT_BYTES + 1) > LARGE_TEXT_BYTES);
  assert.equal(isSingleValueText("abc\r\n"), true);
  assert.equal(isSingleValueText("a\tb"), false);
  assert.equal(isSingleValueText("a\nb"), false);
});

/** 外部文本导入的体量阈值（字节）。 */
export const LARGE_TEXT_BYTES = 8 * 1024 * 1024;
/** 单个 IPC 解码块的最大字节数。 */
export const PASTE_CHUNK_BYTES = 512 * 1024;

/**
 * 计算 UTF-8 字节数，可在达到上限后提前停止。
 * @param {string} text - 输入文本。
 * @param {number} [stopAfter=Infinity] - 达到该字节数即可停止。
 * @returns {number} 已计算的 UTF-8 字节数。
 */
export function utf8ByteLength(text, stopAfter = Number.POSITIVE_INFINITY) {
  let bytes = 0;
  for (let i = 0; i < text.length;) {
    const cp = text.codePointAt(i);
    i += cp > 0xffff ? 2 : 1;
    bytes += cp <= 0x7f ? 1 : cp <= 0x7ff ? 2 : cp <= 0xffff ? 3 : 4;
    if (bytes >= stopAfter) return bytes;
  }
  return bytes;
}

/**
 * 按 UTF-8 字节上限产生文本块，不拆代理对，也不编码完整输入。
 * @param {string} text - 输入文本。
 * @param {number} [maxBytes=PASTE_CHUNK_BYTES] - 单块最大字节数，至少为 4。
 * @yields {{start: number, end: number, bytes: Uint8Array}} 原文范围及编码块。
 */
export function* utf8Chunks(text, maxBytes = PASTE_CHUNK_BYTES) {
  if (!Number.isInteger(maxBytes) || maxBytes < 4) throw new RangeError("maxBytes 至少为 4");
  let start = 0;
  let end = 0;
  let bytes = 0;
  const flush = function* () {
    if (end === start) return;
    const slice = text.slice(start, end);
    yield { start, end, bytes: new TextEncoder().encode(slice) };
  };
  while (end < text.length) {
    const cp = text.codePointAt(end);
    const units = cp > 0xffff ? 2 : 1;
    const n = cp <= 0x7f ? 1 : cp <= 0x7ff ? 2 : cp <= 0xffff ? 3 : 4;
    if (bytes + n > maxBytes) {
      yield* flush();
      start = end;
      bytes = 0;
    }
    end += units;
    bytes += n;
  }
  yield* flush();
}

/**
 * 将一个字节块编码为 IPC 使用的十六进制字符串。
 * @param {Uint8Array} bytes - 输入字节。
 * @returns {string} 小写十六进制文本。
 */
export function bytesToHex(bytes) {
  let out = "";
  for (const b of bytes) out += b.toString(16).padStart(2, "0");
  return out;
}

/**
 * 判断文本是否表示一个可填充的单值；只扫描，不创建 trim/replace 副本。
 * @param {string} text - 输入文本。
 * @returns {boolean} 去掉末尾换行后不含 Tab 或换行且非空。
 */
export function isSingleValueText(text) {
  let end = text.length;
  while (end > 0 && (text[end - 1] === "\r" || text[end - 1] === "\n")) end -= 1;
  if (!end) return false;
  for (let i = 0; i < end; i += 1) {
    if (text[i] === "\t" || text[i] === "\r" || text[i] === "\n") return false;
  }
  return true;
}

/**
 * CSV 读写模块（csv_io.py 的 JS 移植）。
 *
 * 遵循 RFC 4180：含换行、逗号、双引号的字段自动加双引号，
 * 字段内双引号转义为两个双引号。写出统一 \r\n 行尾（与 Python csv 默认一致），
 * UTF-8 BOM 由 Rust 侧写入。GBK/编码识别在 Rust 侧完成后传入纯文本。
 */

/**
 * 解析 CSV/TSV 文本（RFC 4180 语义，与 Python csv 模块一致）。
 *
 * @param {string} text - 纯文本（已解码）。
 * @param {string} [delimiter] - 字段分隔符，默认 ","；"\t" 用于 TSV 粘贴。
 * @returns {{headers: string[], rows: string[][], warnings: string[]}}
 *   表头、数据行、警告列表。短行补空、长行截断并产生警告；
 *   空文件返回 {headers: [], rows: [], warnings: []}。
 */
export function parseCsv(text, delimiter = ",") {
  /** @type {string[][]} */
  const records = [];
  /** @type {string[]} */
  let field = [];
  let cur = "";
  let inQuotes = false;
  let i = 0;
  const n = text.length;
  /** 当前物理行是否含任何字符（用于跳过完全空行） */
  let lineHasContent = false;

  const pushField = () => {
    field.push(cur);
    cur = "";
    lineHasContent = true;
  };
  const pushRecord = () => {
    // 先取样行内容标记（pushField 会置真），避免空行被误判为有内容
    const hadContent = lineHasContent;
    pushField();
    // 跳过完全空的物理行（如文件末尾多余空行），保留 ",," 这类空单元格行
    if (hadContent || field.length > 1) {
      records.push(field);
    }
    field = [];
    lineHasContent = false;
  };

  while (i < n) {
    const ch = text[i];
    if (inQuotes) {
      if (ch === '"') {
        if (text[i + 1] === '"') {
          cur += '"';
          i += 2;
          continue;
        }
        inQuotes = false;
        i += 1;
        continue;
      }
      cur += ch;
      i += 1;
      continue;
    }
    if (ch === '"') {
      inQuotes = true;
      lineHasContent = true;
      i += 1;
      continue;
    }
    if (ch === delimiter) {
      pushField();
      i += 1;
      continue;
    }
    if (ch === "\r") {
      if (text[i + 1] === "\n") {
        i += 1;
      }
      pushRecord();
      i += 1;
      continue;
    }
    if (ch === "\n") {
      pushRecord();
      i += 1;
      continue;
    }
    cur += ch;
    lineHasContent = true;
    i += 1;
  }
  if (cur !== "" || field.length > 0 || lineHasContent) {
    pushRecord();
  }

  if (records.length === 0) {
    return { headers: [], rows: [], warnings: [] };
  }
  const headers = records[0];
  const ncols = headers.length;
  /** @type {string[]} */
  const warnings = [];
  /** @type {string[][]} */
  const rows = [];
  for (let idx = 1; idx < records.length; idx += 1) {
    let row = records[idx];
    // 数据行号（从 1 起）：记录 0 是表头，数据行号 = 记录索引。
    if (row.length < ncols) {
      warnings.push(
        `数据行 ${idx} 只有 ${row.length} 列（表头 ${ncols} 列），已补空`);
      row = row.concat(Array(ncols - row.length).fill(""));
    } else if (row.length > ncols) {
      warnings.push(
        `数据行 ${idx} 有 ${row.length} 列（表头 ${ncols} 列），已截断`);
      row = row.slice(0, ncols);
    }
    rows.push(row);
  }
  return { headers, rows, warnings };
}

/**
 * 序列化为 CSV 文本（RFC 4180，\r\n 行尾）。
 *
 * @param {string[]} headers - 表头。
 * @param {string[][]} rows - 数据行。
 * @param {string} [delimiter] - 字段分隔符，默认 ","；"	" 用于 TSV 复制。
 * @returns {string} CSV 文本（不带 BOM，BOM 由写入方加）。
 */
export function serializeCsv(headers, rows, delimiter = ",") {
  return serializeRecords([headers, ...rows], delimiter);
}

/**
 * 序列化纯记录数组（无表头概念，TSV 复制用），行尾统一 \r\n。
 * 含分隔符/引号/换行的字段按 RFC 4180 加引号并双写内部引号，
 * 与 Python csv.writer(delimiter=...) 行为一致。
 *
 * @param {string[][]} records - 记录行。
 * @param {string} [delimiter] - 字段分隔符，默认 ","。
 * @returns {string} 序列化文本。
 */
export function serializeRecords(records, delimiter = ",") {
  const esc = (s) => {
    const t = String(s);
    if (t.includes(delimiter) || /["\r\n]/.test(t)) {
      return '"' + t.replace(/"/g, '""') + '"';
    }
    return t;
  };
  return records.map((row) => row.map(esc).join(delimiter)).join("\r\n") + "\r\n";
}

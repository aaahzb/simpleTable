/**
 * 判断打开入口是否支持该路径。
 * @param {string} path - 本地文件路径。
 * @returns {boolean} CSV/TXT/LOG 为 true。
 */
export function isSupportedDocumentPath(path) {
  return /\.(?:csv|txt|log)$/i.test(String(path));
}

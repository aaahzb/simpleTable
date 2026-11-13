import test from "node:test";
import assert from "node:assert/strict";
import {
  summarizeCounterWindows,
  uiProbeStatus,
} from "../scripts/probe-ui-physical-reads.mjs";

// 合成 typeperf 表头：C: 与 E: 分开成列，_Total 必须被丢弃。
const HEADER = String.raw`"(PDH-CSV 4.0)","\\HOST\PhysicalDisk(2 C:)\Disk Read Bytes/sec","\\HOST\PhysicalDisk(0 E:)\Disk Read Bytes/sec","\\HOST\PhysicalDisk(_Total)\Disk Read Bytes/sec"`;
const at = (value) => new Date(value).getTime();
const startAt = at("2026-11-13T20:00:01.000");
const endAt = at("2026-11-13T20:00:03.000");
const csv = `${HEADER}\n"11/13/2026 20:00:00.000","1.0","0.0","1.0"\n"11/13/2026 20:00:01.000","2.0","0.0","2.0"\n"11/13/2026 20:00:02.000","3.0","0.0","3.0"\n"11/13/2026 20:00:03.000","4.0","0.0","4.0"\n"11/13/2026 20:00:04.000","5.0","0.0","5.0"`;

test("conversion window reports source and other drives separately", () => {
  const result = summarizeCounterWindows(csv, startAt, endAt);
  assert.equal(result.covered, true);
  assert.equal(result.drives["E:"].status, "zero");
  assert.equal(result.drives["E:"].readBytes, 0);
  assert.equal(result.drives["E:"].sampleCount, 5);
  assert.equal(result.drives["C:"].status, "observed");
  assert.equal(result.drives["C:"].readBytes, 15);
  assert.equal(result.drives._Total, undefined);
  assert.equal(result.fileApi.status, "not_collected");
  assert.equal(result.processIo.status, "not_collected");

  const baseline = summarizeCounterWindows(csv);
  assert.equal(baseline.covered, true);
  assert.equal(baseline.drives["E:"].sampleCount, 5);
});

test("conversion window includes boundary buckets and rejects missing coverage", () => {
  const truncated = `${HEADER}\n"11/13/2026 20:00:01.000","0.0","0.0","0.0"\n"11/13/2026 20:00:02.000","0.0","0.0","0.0"`;
  const result = summarizeCounterWindows(truncated, startAt, endAt);
  assert.equal(result.covered, false);
  assert.equal(result.drives["E:"].status, "unverified");
  assert.equal(result.drives["E:"].readBytes, null);
});

test("physical reads remain reads_observed rather than being attributed to the app", () => {
  // E: 列在数据行中夹在两个逗号之间；_Total 列与 C: 同值，不能用相邻 "0.0","0.0" 来改。
  const nonzero = csv.replaceAll(',"0.0",', ',"8.0",');
  const result = summarizeCounterWindows(nonzero, startAt, endAt);
  assert.equal(result.drives["E:"].status, "observed");
  assert.match(result.drives["E:"].detail, /不能据此归因/);
  assert.equal(uiProbeStatus(true, true, "observed"), "reads_observed");
});

test("final status requires coverage, sufficient available memory and zero source reads", () => {
  assert.equal(uiProbeStatus(true, true, "zero"), "verified_zero");
  assert.equal(uiProbeStatus(false, true, "zero"), "unverified");
  assert.equal(uiProbeStatus(true, false, "zero"), "unverified");
  assert.equal(uiProbeStatus(true, true, "unverified"), "unverified");
});

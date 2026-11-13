import test from "node:test";
import assert from "node:assert/strict";
import {
  decodeCounterCsv,
  finalizeProbeReport,
  parseCsvLine,
  summarizeDiskReads,
} from "../scripts/probe-physical-reads.mjs";

const HEADER = String.raw`"(PDH-CSV 4.0)","\\HOST\PhysicalDisk(2 C:)\Disk Read Bytes/sec","\\HOST\PhysicalDisk(0 E:)\Disk Read Bytes/sec","\\HOST\PhysicalDisk(_Total)\Disk Read Bytes/sec"`;

test("decodeCounterCsv detects UTF-16LE and UTF-8 BOM", () => {
  assert.equal(decodeCounterCsv(Buffer.from([0xff, 0xfe, 0x41, 0x00])), "A");
  assert.equal(decodeCounterCsv(Buffer.from([0xef, 0xbb, 0xbf, 0x41])), "A");
  assert.equal(decodeCounterCsv(Buffer.from("ABC", "utf8")), "ABC");
});

test("parseCsvLine preserves quoted commas and escaped quotes", () => {
  assert.deepEqual(parseCsvLine('"a,b","x""y","3.5"'), ["a,b", 'x"y', "3.5"]);
});

test("summarizeDiskReads selects only the source drive and proves zero", () => {
  const text = `${HEADER}\n"11/12/2026 20:00:00.000","9.0","0.0","9.0"\n"11/12/2026 20:00:01.000","8.0","0.0","8.0"`;
  const result = summarizeDiskReads(text, "E:");
  assert.equal(result.status, "zero");
  assert.equal(result.readBytes, 0);
  assert.equal(result.sampleCount, 2);
  assert.equal(result.counterColumns.length, 1);
});

test("summarizeDiskReads integrates nonzero rates without claiming source attribution", () => {
  const text = `${HEADER}\n"t0","0.0","10.0","10.0"\n"t1","0.0","20.0","20.0"`;
  const result = summarizeDiskReads(text, "E:");
  assert.equal(result.status, "observed");
  assert.equal(result.readBytes, 30);
  assert.match(result.detail, /目标物理盘/);
});

test("summarizeDiskReads rejects missing drive columns and samples", () => {
  assert.equal(summarizeDiskReads(HEADER, "E:").status, "unverified");
  const otherDrive = `${HEADER}\n"t0","0.0","0.0","0.0"`;
  assert.equal(summarizeDiskReads(otherDrive, "F:").status, "unverified");
});

test("finalizeProbeReport keeps the verified status ahead of summary.status", () => {
  const zero = finalizeProbeReport(true, { status: "zero", readBytes: 0 }, { drive: "E:" });
  assert.equal(zero.status, "verified_zero");
  assert.equal(zero.readBytes, 0);
  assert.equal(zero.drive, "E:");
  assert.equal(finalizeProbeReport(true, { status: "observed" }, {}).status, "reads_observed");
  assert.equal(finalizeProbeReport(false, { status: "zero" }, {}).status, "unverified");
});

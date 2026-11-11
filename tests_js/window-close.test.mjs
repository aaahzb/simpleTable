import assert from "node:assert/strict";
import { test } from "node:test";
import { createWindowCloseCoordinator } from "../frontend/js/windowlifecycle.js";

function closeHarness(confirmed) {
  const calls = [];
  const coordinator = createWindowCloseCoordinator({
    tabs: () => [{ id: 1, dirty: true }],
    activateTab: async (id) => calls.push(["activate", id]),
    confirmDiscard: async () => confirmed,
    settleDocumentTasks: async () => calls.push(["settle"]),
    closeWindow: async () => calls.push(["close_window"]),
  });
  return { calls, coordinator };
}

test("cancel keeps the window and never quits the process", async () => {
  const { calls, coordinator } = closeHarness(false);
  assert.equal(await coordinator.requestClose(), false);
  assert.deepEqual(calls, [["activate", 1]]);
});

test("confirm settles document tasks before closing only this window", async () => {
  const { calls, coordinator } = closeHarness(true);
  assert.equal(await coordinator.requestClose(), true);
  assert.deepEqual(calls, [["activate", 1], ["settle"], ["close_window"]]);
});

test("window close settles every tab in order even when none is dirty", async () => {
  const calls = [];
  const coordinator = createWindowCloseCoordinator({
    tabs: () => [{ id: 1, dirty: false }, { id: 2, dirty: false }],
    activateTab: async (id) => calls.push(["activate", id]),
    confirmDiscard: async () => true,
    settleDocumentTasks: async (tab) => calls.push(["settle", tab.id]),
    closeWindow: async () => calls.push(["close_window"]),
  });
  assert.equal(await coordinator.requestClose(), true);
  assert.deepEqual(calls, [["settle", 1], ["settle", 2], ["close_window"]]);
});

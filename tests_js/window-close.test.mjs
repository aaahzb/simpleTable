import assert from "node:assert/strict";
import { test } from "node:test";
import { createWindowCloseCoordinator } from "../frontend/js/windowlifecycle.js";

function closeHarness(confirmed) {
  const calls = [];
  const coordinator = createWindowCloseCoordinator({
    tabs: () => [{ id: 1, dirty: true }],
    activateTab: async (id) => calls.push(["activate", id]),
    confirmDiscard: async () => confirmed,
    closeWindow: async () => calls.push(["close_window"]),
  });
  return { calls, coordinator };
}

test("cancel keeps the window and never quits the process", async () => {
  const { calls, coordinator } = closeHarness(false);
  assert.equal(await coordinator.requestClose(), false);
  assert.deepEqual(calls, [["activate", 1]]);
});

test("confirm closes only this window and never quits the process", async () => {
  const { calls, coordinator } = closeHarness(true);
  assert.equal(await coordinator.requestClose(), true);
  assert.deepEqual(calls, [["activate", 1], ["close_window"]]);
});

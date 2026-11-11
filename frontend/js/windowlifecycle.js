/** 按文档隔离内存任务；关闭时可以取消并等待对应任务落定。 */
export function createMemoryOperationRegistry(operations = new WeakMap()) {
  return {
    run(model, kind, operation, { onChange = () => {} } = {}) {
      if (!model) return Promise.reject(new Error("内存任务缺少文档模型"));
      if (operations.has(model)) {
        return Promise.reject(new Error("当前文档已有内存任务正在执行"));
      }
      const current = { kind, promise: null, previousKind: model._memoryOperationKind ?? null };
      operations.set(model, current);
      model._memoryOperationKind = kind;
      onChange();
      current.promise = (async () => {
        try {
          return await operation();
        } finally {
          if (operations.get(model) === current) {
            operations.delete(model);
            model._memoryOperationKind = current.previousKind;
          }
          onChange();
        }
      })();
      return current.promise;
    },

    kind(model) {
      return model ? operations.get(model)?.kind ?? null : null;
    },

    async settle(model) {
      const current = model ? operations.get(model) : null;
      if (!current) return;
      try { await model.cancelMemoryModeLoad(); } catch { /* 已结束或不可取消 */ }
      try { await current.promise; } catch { /* 关闭仍须继续释放引擎 */ }
    },
  };
}

/** 窗口关闭确认协调器：只关闭当前窗口，不直接退出进程。 */
export function createWindowCloseCoordinator({
  tabs, activateTab, confirmDiscard, settleDocumentTasks = async () => {}, closeWindow,
}) {
  return {
    async requestClose() {
      for (const tab of tabs()) {
        if (!tab.dirty) continue;
        await activateTab(tab.id);
        if (!(await confirmDiscard())) return false;
      }
      for (const tab of tabs()) await settleDocumentTasks(tab);
      await closeWindow();
      return true;
    },
  };
}

/**
 * 同一文档会话的跨窗口迁移协调器。
 * 源窗口先 seal，目标 accept/adopt 后才 complete；失败和超时统一 cancel。
 */

/**
 * 创建每个 WebView 各自的一份迁移协调器。
 *
 * @param {object} deps - Tauri 桥、标签适配器和错误通知。
 * @returns {{transferTabToWindow: Function, dispose: Function, ready: Promise}} 源迁移入口、清理函数和监听就绪。
 */
export function createTransferCoordinator({
  invoke,
  listen,
  createWindow,
  closeEmptyWindow,
  modelForSession,
  adoptToTab,
  removeSourceTab,
  removeAdoptedTab,
  onChanged = () => {},
  onError = () => {},
  timeoutMs = 15000,
}) {
  const sourcePending = new Map();
  const targetPending = new Map();
  const earlyAccepted = new Map();
  const earlyCancelled = new Map();
  const activeSessions = new Set();
  const disposers = [];

  function report(error) {
    try { onError(error); } catch { /* 通知失败不能掩盖迁移结果 */ }
  }

  function changed() {
    try { onChanged(); } catch (error) { report(error); }
  }

  function payloadOf(event) {
    return event?.payload ?? event;
  }

  function clearSource(record) {
    if (record.timer) clearTimeout(record.timer);
    record.timer = null;
    if (sourcePending.get(record.ticket) === record) sourcePending.delete(record.ticket);
  }

  function accepted(event) {
    const notice = payloadOf(event);
    const record = sourcePending.get(notice.ticket);
    if (!record) {
      earlyAccepted.set(notice.ticket, notice);
      return;
    }
    record.adopted = true;
    record.acceptResolve(notice);
  }

  function cancelled(event) {
    const update = payloadOf(event);
    const source = sourcePending.get(update.ticket);
    if (source) {
      source.eventCancelled = true;
      clearSource(source);
      try {
        source.model.rollbackTransfer(update.lease);
      } catch (error) {
        report(error);
      }
      source.rejectFailure(new Error("迁移已取消"));
    } else {
      earlyCancelled.set(update.ticket, update);
    }
    const target = targetPending.get(update.ticket);
    if (target) {
      targetPending.delete(update.ticket);
      if (target.session !== null) removeAdoptedTab(target.session);
      changed();
    }
  }

  function completed(event) {
    const update = payloadOf(event);
    const target = targetPending.get(update.ticket);
    if (!target) return;
    targetPending.delete(update.ticket);
    target.model?.finishTransfer(update.lease);
    changed();
  }

  async function requested(event) {
    const { ticket } = payloadOf(event);
    const record = { ticket, session: null, model: null };
    targetPending.set(ticket, record);
    try {
      const adopted = await invoke("accept_transfer", { ticket });
      record.session = adopted.session;
      record.model = await adoptToTab(adopted);
      await invoke("transfer_adopted", { ticket });
      changed();
    } catch (error) {
      targetPending.delete(ticket);
      if (record.session !== null) {
        try {
          await removeAdoptedTab(record.session);
        } catch (cleanupError) {
          report(cleanupError);
        }
      }
      try {
        await invoke("cancel_transfer", { ticket });
      } catch (cancelError) {
        if (!/ticket|结束/.test(String(cancelError))) report(cancelError);
      }
      changed();
      report(error);
    }
  }

  async function recovered(event) {
    const adopted = payloadOf(event);
    const record = { ticket: adopted.ticket, session: adopted.session, model: null };
    targetPending.set(adopted.ticket, record);
    try {
      record.model = await adoptToTab(adopted);
      record.model?.finishTransfer(adopted.lease);
      targetPending.delete(adopted.ticket);
      changed();
    } catch (error) {
      targetPending.delete(adopted.ticket);
      removeAdoptedTab(adopted.session);
      report(error);
    }
  }

  for (const [event, handler] of [
    ["transfer-accepted", accepted],
    ["transfer-cancelled", cancelled],
    ["transfer-completed", completed],
    ["transfer-request", requested],
    ["document-recovered", recovered],
  ]) {
    try {
      const off = listen(event, (eventPayload) => {
        try {
          const result = handler(eventPayload);
          if (result?.catch) result.catch(report);
        } catch (error) {
          report(error);
        }
      });
      disposers.push(off);
    } catch (error) {
      report(error);
    }
  }

  function makeSourceRecord(ticket, session, model) {
    const record = {
      ticket, session, model, timer: null, adopted: false,
      eventCancelled: false, completed: false,
    };
    record.accepted = new Promise((resolve) => { record.acceptResolve = resolve; });
    record.failure = new Promise((_, reject) => { record.rejectFailure = reject; });
    sourcePending.set(ticket, record);
    record.timer = setTimeout(() => {
      record.rejectFailure(new Error("迁移超时"));
    }, timeoutMs);
    if (earlyAccepted.has(ticket)) {
      const notice = earlyAccepted.get(ticket);
      earlyAccepted.delete(ticket);
      accepted(notice);
    }
    if (earlyCancelled.has(ticket)) {
      const update = earlyCancelled.get(ticket);
      earlyCancelled.delete(ticket);
      cancelled(update);
    }
    return record;
  }

  async function driveSource(record) {
    await record.accepted;
    const update = await invoke("complete_transfer", { ticket: record.ticket });
    record.completed = true;
    const remaining = await removeSourceTab(record.session);
    changed();
    if (remaining === 0) await closeEmptyWindow();
    return update;
  }

  /**
   * 把 session 迁往已 ready 的窗口；targetLabel=null 时先创建隐藏文档窗口。
   * complete 返回前绝不删除源标签，最后标签也只能随后关闭源窗口。
   */
  async function transferTabToWindow(session, targetLabel = null, side = "A", index = 0) {
    if (activeSessions.has(session)) throw new Error("文档正在迁移，请稍后重试");
    activeSessions.add(session);
    let model = null;
    let record = null;
    let ticket = null;
    let prepared = false;
    try {
      model = modelForSession(session);
      if (!model) throw new Error("待迁移标签的文档会话不存在");
      const snapshot = await model.prepareTransfer();
      prepared = true;
      const target = targetLabel ?? await createWindow();
      ticket = await invoke("seal_document", {
        session, lease: model.lease, target, side, index, snapshot,
      });
      record = makeSourceRecord(ticket, session, model);
      return await Promise.race([driveSource(record), record.failure]);
    } catch (error) {
      if (!prepared || record?.completed || record?.eventCancelled) {
        report(error);
        throw error;
      }
      let update = null;
      if (ticket !== null) {
        try {
          update = await invoke("cancel_transfer", { ticket });
        } catch (cancelError) {
          if (!record?.eventCancelled) report(cancelError);
        }
      }
      if (record?.eventCancelled) {
        report(error);
        throw error;
      }
      update ??= earlyCancelled.get(ticket) ?? null;
      if (update) earlyCancelled.delete(ticket);
      if (prepared) model.rollbackTransfer(update?.lease ?? model.lease);
      report(error);
      throw error;
    } finally {
      if (record) clearSource(record);
      activeSessions.delete(session);
    }
  }

  async function dispose() {
    for (const record of sourcePending.values()) {
      if (record.timer) clearTimeout(record.timer);
    }
    sourcePending.clear();
    for (const off of disposers.splice(0)) {
      try {
        const unlisten = await off;
        if (typeof unlisten === "function") unlisten();
      } catch { /* 窗口销毁时监听已随 WebView 释放 */ }
    }
  }

  const ready = Promise.all(disposers).catch((error) => { report(error); });
  return { transferTabToWindow, dispose, ready };
}

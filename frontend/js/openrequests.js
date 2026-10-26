export function createOpenRequestPump({ invoke, listen, openPath, activatePath, reportError }) {
  let chain = Promise.resolve();
  const schedule = (batch) => {
    if (!batch) return chain;
    chain = chain.then(() => consume(batch));
    return chain;
  };
  const consume = async (batch) => {
    for (const item of batch.items) {
      try {
        if (item.kind === "open") await openPath(item.path, { requestId: item.requestId });
        else await activatePath(item.path, item.session);
      } catch (error) {
        if (item.kind === "open") {
          try {
            await invoke("open_requests_fail", { requestId: item.requestId, path: item.path });
          } catch (releaseError) { reportError(releaseError, item); }
        }
        reportError(error, item);
      }
    }
    const next = await invoke("open_requests_ack", { batchId: batch.batchId });
    if (next) schedule(next);
  };
  const start = async () => {
    await listen("open-paths", (event) => schedule(event.payload));
    const first = await invoke("open_requests_ready");
    schedule(first);
  };
  const idle = async () => {
    for (;;) {
      const current = chain;
      await current;
      if (current === chain) return;
    }
  };
  return { start, consume, schedule, idle };
}

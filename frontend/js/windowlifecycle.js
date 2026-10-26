/** 窗口关闭确认协调器：只关闭当前窗口，不直接退出进程。 */
export function createWindowCloseCoordinator({ tabs, activateTab, confirmDiscard, closeWindow }) {
  return {
    async requestClose() {
      for (const tab of tabs()) {
        if (!tab.dirty) continue;
        await activateTab(tab.id);
        if (!(await confirmDiscard())) return false;
      }
      await closeWindow();
      return true;
    },
  };
}

// Request/response over a runtime port to the background page (used by the
// popup and options page).

let nextId = 1;

export function callBackground(messenger, cmd, { timeoutMs = 120000, ...args } = {}) {
  return new Promise((resolve, reject) => {
    const port = messenger.runtime.connect({ name: "eh-control" });
    const id = nextId++;
    const timer = setTimeout(() => {
      cleanup();
      reject(new Error(`background did not answer "${cmd}"`));
    }, timeoutMs);
    function cleanup() {
      clearTimeout(timer);
      try {
        port.disconnect();
      } catch {
        /* already closed */
      }
    }
    port.onMessage.addListener((msg) => {
      if (!msg || msg.id !== id) return;
      cleanup();
      if (msg.ok) resolve(msg);
      else reject(new Error(msg.error || "background error"));
    });
    port.onDisconnect.addListener(() => {
      clearTimeout(timer);
      reject(new Error("background disconnected"));
    });
    port.postMessage({ id, cmd, ...args });
  });
}

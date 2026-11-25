(() => {
  if (window !== window.top || location.origin !== __ORIGIN__ || window.__DSH_LAUNCHER_BROWSER__) {
    return;
  }
  const token = __TOKEN__;
  const send = (action, payload) => window.__TAURI_INTERNALS__.invoke('plugin:sidebar-browser|send', {
    token, action, payload,
  });
  Object.defineProperty(window, '__DSH_LAUNCHER_BROWSER__', {
    value: Object.freeze({
      version: 1,
      native: __SUPPORTED__,
      send,
      acknowledgeSession: (sessionId, turn) => send('acknowledge-session', { sessionId, turn }),
    }),
  });
  window.dispatchEvent(new Event('dsh-launcher-browser-ready'));
})();

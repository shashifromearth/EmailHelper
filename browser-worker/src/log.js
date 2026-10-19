// Structured JSON logs: one object per line on stdout: { ts, level, msg, fields }.
const LEVELS = { debug: 10, info: 20, warn: 30, error: 40, silent: 100 };

export function createLogger({ level = 'info', stream = process.stdout, base = {} } = {}) {
  const threshold = LEVELS[level] ?? LEVELS.info;
  const write = (lvl, msg, fields) => {
    if (LEVELS[lvl] < threshold) return;
    let line;
    try {
      line = JSON.stringify({ ts: new Date().toISOString(), level: lvl, msg, fields: { ...base, ...fields } });
    } catch {
      line = JSON.stringify({ ts: new Date().toISOString(), level: lvl, msg, fields: { unserializable: true } });
    }
    stream.write(line + '\n');
  };
  return {
    debug: (msg, fields = {}) => write('debug', msg, fields),
    info: (msg, fields = {}) => write('info', msg, fields),
    warn: (msg, fields = {}) => write('warn', msg, fields),
    error: (msg, fields = {}) => write('error', msg, fields),
    child: (extra) => createLogger({ level, stream, base: { ...base, ...extra } }),
  };
}

// Tiny structured logger — one JSON object per line, easy to grep/ship.
type Fields = Record<string, unknown>;

function emit(level: string, msg: string, fields?: Fields): void {
  const line = { ts: new Date().toISOString(), level, msg, ...(fields ?? {}) };
  const out = level === "error" || level === "warn" ? process.stderr : process.stdout;
  out.write(JSON.stringify(line) + "\n");
}

export const log = {
  info: (msg: string, fields?: Fields) => emit("info", msg, fields),
  warn: (msg: string, fields?: Fields) => emit("warn", msg, fields),
  error: (msg: string, fields?: Fields) => emit("error", msg, fields),
};

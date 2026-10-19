// Validation of file paths handed to file inputs: only files inside
// BROWSER_ALLOWED_UPLOAD_DIRS may be uploaded.
import fs from 'node:fs/promises';
import path from 'node:path';

export const MAX_UPLOAD_BYTES = 25 * 1024 * 1024;

export function isInside(child, parent) {
  const rel = path.relative(parent, child); // case-insensitive on win32
  return rel !== '' && !rel.startsWith('..') && !path.isAbsolute(rel);
}

/**
 * Resolve `p` (absolute, or relative to the worker cwd) and check it is a
 * regular file inside one of `allowedDirs`. Symlinks are resolved on both sides
 * so a link inside an allowed dir cannot point outside it.
 * @returns {Promise<{ok: true, path: string} | {ok: false, reason: string}>}
 */
export async function resolveUpload(p, allowedDirs) {
  if (typeof p !== 'string' || !p.trim()) return { ok: false, reason: 'file path must be a non-empty string' };
  const abs = path.resolve(p.trim());
  let real;
  try {
    real = await fs.realpath(abs);
  } catch {
    return { ok: false, reason: `file not found: ${abs}` };
  }
  let stat;
  try {
    stat = await fs.stat(real);
  } catch {
    return { ok: false, reason: `file not readable: ${abs}` };
  }
  if (!stat.isFile()) return { ok: false, reason: `not a regular file: ${abs}` };
  if (stat.size > MAX_UPLOAD_BYTES) return { ok: false, reason: `file larger than ${MAX_UPLOAD_BYTES} bytes: ${abs}` };

  for (const dir of allowedDirs) {
    let realDir;
    try {
      realDir = await fs.realpath(dir);
    } catch {
      realDir = path.resolve(dir);
    }
    if (isInside(real, realDir)) return { ok: true, path: real };
  }
  return { ok: false, reason: `path is outside BROWSER_ALLOWED_UPLOAD_DIRS: ${abs}` };
}

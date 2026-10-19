// Tiny mock ATS server for browser-worker tests.
//
//   node tests/mock-sites/server.mjs [--port 8080]
//
// - serves the files in this folder
// - records POSTs to /submit (urlencoded or multipart) in memory
// - GET /__submissions returns them as JSON; POST /__reset clears them
// - /status/<code> returns that HTTP status (blocker tests)
// - /set-cookie sets a persistent cookie (session persistence test)
// - /li/search/results/people/ serves linkedin/search.html, or redirects to an
//   authwall when the keywords contain "authwall"
import http from 'node:http';
import fs from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = path.dirname(fileURLToPath(import.meta.url));

const TYPES = {
  '.html': 'text/html; charset=utf-8',
  '.js': 'text/javascript; charset=utf-8',
  '.css': 'text/css; charset=utf-8',
  '.json': 'application/json',
  '.pdf': 'application/pdf',
  '.png': 'image/png',
};

function readBody(req, limit = 10 * 1024 * 1024) {
  return new Promise((resolve, reject) => {
    const chunks = [];
    let size = 0;
    req.on('data', (c) => {
      size += c.length;
      if (size > limit) {
        reject(new Error('too large'));
        req.destroy();
      } else chunks.push(c);
    });
    req.on('end', () => resolve(Buffer.concat(chunks)));
    req.on('error', reject);
  });
}

function addField(fields, name, value) {
  if (name in fields) fields[name] = [].concat(fields[name], value);
  else fields[name] = value;
}

/** Minimal multipart/form-data parser: records field values and uploaded file names/sizes. */
export function parseMultipart(buf, contentType) {
  const m = /boundary=(?:"([^"]+)"|([^;]+))/i.exec(contentType || '');
  const fields = {};
  const files = [];
  if (!m) return { fields, files };
  const boundary = '--' + (m[1] || m[2]).trim();
  const body = buf.toString('latin1');
  for (const part of body.split(boundary)) {
    if (!part || part.startsWith('--')) continue;
    const idx = part.indexOf('\r\n\r\n');
    if (idx < 0) continue;
    const head = part.slice(0, idx);
    let content = part.slice(idx + 4);
    if (content.endsWith('\r\n')) content = content.slice(0, -2);
    const name = /name="([^"]*)"/i.exec(head)?.[1];
    const filename = /filename="([^"]*)"/i.exec(head)?.[1];
    if (name === undefined) continue;
    if (filename !== undefined) {
      if (filename) {
        files.push({
          field: name,
          filename: Buffer.from(filename, 'latin1').toString('utf8'),
          size: content.length,
          contentType: /content-type:\s*([^\r\n]+)/i.exec(head)?.[1] || null,
        });
      }
    } else {
      addField(fields, name, Buffer.from(content, 'latin1').toString('utf8'));
    }
  }
  return { fields, files };
}

const thankYou = (ats) =>
  ats === 'lever'
    ? `<!doctype html><html><head><title>Application submitted</title></head><body><h1>Application submitted!</h1><p>Thanks! We've received your application and will be in touch.</p></body></html>`
    : `<!doctype html><html><head><title>Thank you</title></head><body><h1>Thank you for applying!</h1><p>Your application has been received.</p></body></html>`;

export function startMockServer({ port = 0, host = '127.0.0.1' } = {}) {
  const submissions = [];
  const linkedinHits = [];

  const server = http.createServer(async (req, res) => {
    const url = new URL(req.url, `http://${req.headers.host || 'localhost'}`);
    try {
      if (url.pathname === '/__submissions' && req.method === 'GET') {
        res.writeHead(200, { 'Content-Type': 'application/json' });
        return res.end(JSON.stringify({ submissions, linkedin_hits: linkedinHits }));
      }
      if (url.pathname === '/__reset' && req.method === 'POST') {
        submissions.length = 0;
        linkedinHits.length = 0;
        res.writeHead(204);
        return res.end();
      }
      if (url.pathname === '/submit' && req.method === 'POST') {
        const buf = await readBody(req);
        const ct = req.headers['content-type'] || '';
        let rec;
        if (ct.startsWith('multipart/form-data')) rec = parseMultipart(buf, ct);
        else {
          const fields = {};
          for (const [k, v] of new URLSearchParams(buf.toString('utf8'))) addField(fields, k, v);
          rec = { fields, files: [] };
        }
        const ats = url.searchParams.get('ats') || 'greenhouse';
        submissions.push({ path: url.pathname, ats, contentType: ct.split(';')[0], ...rec, at: new Date().toISOString() });
        res.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8' });
        return res.end(thankYou(ats));
      }
      const sm = /^\/status\/(\d{3})$/.exec(url.pathname);
      if (sm) {
        const code = Number(sm[1]);
        const msg = { 403: 'Access Denied', 429: 'Too Many Requests', 500: 'Internal Server Error', 503: 'Service Unavailable' }[code] || 'Status';
        res.writeHead(code, { 'Content-Type': 'text/html; charset=utf-8' });
        return res.end(`<!doctype html><html><head><title>${code} ${msg}</title></head><body><h1>${msg}</h1></body></html>`);
      }
      if (url.pathname === '/set-cookie') {
        res.writeHead(200, {
          'Content-Type': 'text/html; charset=utf-8',
          'Set-Cookie': `eh_session=${encodeURIComponent(url.searchParams.get('v') || 'logged-in')}; Max-Age=86400; Path=/; HttpOnly; SameSite=Lax`,
        });
        return res.end('<!doctype html><html><head><title>cookie set</title></head><body>ok</body></html>');
      }
      if (url.pathname === '/li/search/results/people/') {
        const kw = url.searchParams.get('keywords') || '';
        linkedinHits.push({ keywords: kw, at: Date.now() });
        if (/authwall/i.test(kw)) {
          res.writeHead(302, { Location: '/li/authwall?trk=people-search' });
          return res.end();
        }
        const html = await fs.readFile(path.join(ROOT, 'linkedin', 'search.html'));
        res.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8' });
        return res.end(html);
      }
      if (url.pathname === '/li/authwall') {
        res.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8' });
        return res.end(
          '<!doctype html><html><head><title>Sign Up | LinkedIn</title></head><body><h1>Join LinkedIn</h1>' +
            '<form><input name="session_key" type="email"><input name="session_password" type="password"><button>Sign in</button></form></body></html>',
        );
      }

      if (req.method !== 'GET' && req.method !== 'HEAD') {
        res.writeHead(405);
        return res.end();
      }
      let rel = decodeURIComponent(url.pathname);
      if (rel.endsWith('/')) rel += 'index.html';
      const file = path.resolve(ROOT, '.' + rel);
      if (!file.startsWith(ROOT + path.sep)) {
        res.writeHead(403);
        return res.end('forbidden');
      }
      const data = await fs.readFile(file);
      res.writeHead(200, { 'Content-Type': TYPES[path.extname(file).toLowerCase()] || 'application/octet-stream' });
      return res.end(req.method === 'HEAD' ? undefined : data);
    } catch (e) {
      if (e && e.code === 'ENOENT') {
        res.writeHead(404, { 'Content-Type': 'text/plain' });
        return res.end('not found');
      }
      res.writeHead(500, { 'Content-Type': 'text/plain' });
      return res.end('mock error');
    }
  });

  return new Promise((resolve, reject) => {
    server.once('error', reject);
    server.listen(port, host, () => {
      const addr = server.address();
      resolve({
        url: `http://${host}:${addr.port}`,
        port: addr.port,
        submissions,
        linkedinHits,
        close: () =>
          new Promise((r) => {
            server.closeAllConnections?.();
            server.close(() => r());
          }),
      });
    });
  });
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const i = process.argv.indexOf('--port');
  const port = i > 0 ? Number(process.argv[i + 1]) : Number(process.env.PORT || 0);
  const s = await startMockServer({ port });
  console.log(`mock sites at ${s.url}`);
}

# EmailHelper browser worker
# The base image version must match the "playwright" version pinned in package.json.
FROM mcr.microsoft.com/playwright:v1.64.1-noble

ENV NODE_ENV=production \
    PLAYWRIGHT_BROWSERS_PATH=/ms-playwright \
    BROWSER_WORKER_HOST=0.0.0.0 \
    BROWSER_ALLOW_REMOTE=true \
    BROWSER_WORKER_PORT=7801 \
    BROWSER_HEADLESS=true \
    BROWSER_PROFILE_DIR=/app/data/browser-profile \
    BROWSER_SCREENSHOT_DIR=/app/data/screenshots \
    BROWSER_ALLOWED_UPLOAD_DIRS="/app/knowledge;/app/data"

WORKDIR /app

COPY package.json package-lock.json ./
# Browsers are already in the base image; don't download them again.
RUN PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1 npm ci --omit=dev && npm cache clean --force

COPY src ./src

RUN mkdir -p /app/data /app/knowledge && chown -R pwuser:pwuser /app/data /app/knowledge

USER pwuser

EXPOSE 7801

HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
  CMD node -e "fetch('http://127.0.0.1:7801/health').then(r=>process.exit(r.ok?0:1)).catch(()=>process.exit(1))"

CMD ["node", "src/index.js"]

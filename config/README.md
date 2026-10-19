# config/

Runtime configuration is environment-based: copy `../.env.example` to `../.env`.
See [documents/CONFIGURATION.md](../documents/CONFIGURATION.md).

This folder is mounted read-only into containers (`/app/config`) and copied by
the installers. Use it for deployment-specific files you want versioned
alongside the agent, e.g. an alternate env file selected with
`EH_ENV_FILE=config/production.env`. Never commit files containing secrets.

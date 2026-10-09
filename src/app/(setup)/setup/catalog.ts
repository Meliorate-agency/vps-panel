export type CatalogEntry = {
  slug: string;
  label: string;
  description: string;
  recommended: boolean;
};

// Subset of `quick_install::RECIPES` we surface during first-run setup.
// The wizard hands the slug straight to the agent's /quick-install/{slug}
// endpoint, so each entry here must match an existing recipe slug.
export const SETUP_APP_CATALOG: readonly CatalogEntry[] = [
  {
    slug: "roundcube",
    label: "Roundcube Webmail",
    description:
      "Web UI for the IMAP/SMTP server. Access via https://webmail.<domain> after creating a Sites mapping.",
    recommended: true,
  },
  {
    slug: "phpmyadmin",
    label: "phpMyAdmin",
    description: "Browser UI for MariaDB / MySQL. Runs on 127.0.0.1:8081.",
    recommended: false,
  },
  {
    slug: "redis",
    label: "Redis",
    description: "In-memory key-value store on :6379. Useful for app caching.",
    recommended: false,
  },
  {
    slug: "postgresql",
    label: "PostgreSQL",
    description: "Relational database on :5432. Adds alternative to MariaDB.",
    recommended: false,
  },
  {
    slug: "tailscale",
    label: "Tailscale",
    description:
      "Private network between this server and your other devices (WireGuard). Install, then log in from this row.",
    recommended: false,
  },
] as const;

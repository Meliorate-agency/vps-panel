import { db } from "@/lib/db";

/**
 * Domains a new mailbox can go on: every site and DNS zone the panel knows
 * (the Domains page's list), minus subdomains of another name on it. A mail
 * domain like shop.example.com gets no MX/SPF/DKIM records (the agent only
 * publishes them into a zone of that exact name) and a mail.<domain> cert
 * order that cannot pass, so the Add mailbox picker offers only this list and
 * createMailboxAction refuses anything else.
 */
export function mailboxDomains(): string[] {
  const names = (
    db()
      .prepare(`
        SELECT lower(domain) AS name FROM sites
        UNION
        SELECT lower(name) FROM domains
        ORDER BY name ASC
      `)
      .all() as { name: string }[]
  ).map((r) => r.name);
  const known = new Set(names);
  return names.filter((name) => {
    const labels = name.split(".");
    for (let i = 1; i < labels.length; i++) {
      if (known.has(labels.slice(i).join("."))) return false;
    }
    return true;
  });
}

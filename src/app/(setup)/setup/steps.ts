// "acme" comes before "hostname": the hostname step can issue the panel's
// certificate, and Let's Encrypt registers with the SSL contact email.
export const SETUP_STEPS = [
  "welcome",
  "acme",
  "hostname",
  "services",
  "nameservers",
  "security",
  "apps",
  "password",
] as const;

export type SetupStep = (typeof SETUP_STEPS)[number];

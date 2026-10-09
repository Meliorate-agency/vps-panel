import "server-only";
import { getSettingNumber } from "./settings";

/** panel.db key: the largest file one upload may carry, in MB (MiB). Set in
 *  Settings > Panel > Uploads; read on every upload, so a change applies to
 *  the next one. */
export const UPLOAD_LIMIT_KEY = "files.max_upload_mb";
export const UPLOAD_LIMIT_DEFAULT_MB = 100;
/** The most the rest of the path takes: the agent's MAX_UPLOAD_BYTES
 *  (files.rs) and the panel vhost's client_max_body_size for
 *  /api/files/upload are both 5 GiB. */
export const UPLOAD_LIMIT_MAX_MB = 5 * 1024;

export function uploadLimitMb(): number {
  const mb = Math.floor(getSettingNumber(UPLOAD_LIMIT_KEY, UPLOAD_LIMIT_DEFAULT_MB));
  return Math.min(Math.max(mb, 1), UPLOAD_LIMIT_MAX_MB);
}

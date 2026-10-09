/**
 * Copy text to the clipboard from a click handler; resolves to whether it
 * worked.
 *
 * navigator.clipboard exists only in a secure context (HTTPS, or localhost),
 * and a fresh install serves the panel over plain HTTP until the panel host
 * has a certificate. There the older execCommand("copy") on a hidden
 * textarea still works.
 */
export async function copyText(text: string): Promise<boolean> {
  if (window.isSecureContext && navigator.clipboard) {
    try {
      await navigator.clipboard.writeText(text);
      return true;
    } catch {
      // Refused (permissions, document not focused): try the fallback.
    }
  }
  const previous = document.activeElement as HTMLElement | null;
  const area = document.createElement("textarea");
  area.value = text;
  area.setAttribute("readonly", "");
  area.style.position = "fixed";
  area.style.top = "0";
  area.style.left = "0";
  area.style.opacity = "0";
  document.body.appendChild(area);
  area.focus({ preventScroll: true });
  area.select();
  let copied = false;
  try {
    copied = document.execCommand("copy");
  } catch {
    copied = false;
  }
  area.remove();
  previous?.focus({ preventScroll: true });
  return copied;
}

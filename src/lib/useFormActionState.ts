import { useActionState, useCallback, useReducer, useRef, type RefObject } from "react";

function isOk(result: unknown): boolean {
  return typeof result === "object" && result !== null && (result as { ok?: unknown }).ok === true;
}

/**
 * useActionState for a `<form action={...}>` whose fields survive a refused
 * submit. Put the returned ref on the form.
 *
 * React 19 resets a form after its action has run, failed or not. That wiped
 * what the operator had typed under the error message, and a controlled
 * <select> or checkbox snapped back to its first option or its state at mount
 * while the component's state still held the operator's pick, so the form
 * showed one thing and sent another. With the ref, the reset only goes ahead
 * after a result with `ok: true` (clearing the uncontrolled fields, as
 * before), and the component renders once more right after it so controlled
 * fields show their state again. After any other result the form stays as
 * the operator left it.
 *
 * The form keeps its `action`, so pending, useFormStatus buttons and the
 * clicked button's name/value work as usual. Pass `formRef` when the
 * component needs the form element too; it is kept pointing at it.
 */
export function useFormActionState<S>(
  action: (prev: Awaited<S>, fd: FormData) => Promise<S>,
  initial: Awaited<S>,
  formRef?: RefObject<HTMLFormElement | null>,
) {
  const succeeded = useRef(false);
  const [, rerender] = useReducer((n: number) => n + 1, 0);
  const [state, formAction, pending] = useActionState<S, FormData>(async (prev, fd) => {
    succeeded.current = false;
    const result = await action(prev, fd);
    succeeded.current = isOk(result);
    return result;
  }, initial);

  const ref = useCallback(
    (form: HTMLFormElement | null) => {
      if (formRef) formRef.current = form;
      if (!form) return;
      const onReset = (e: Event) => {
        if (!succeeded.current) {
          e.preventDefault();
          return;
        }
        // After the reset has landed: re-render so React writes the
        // controlled values back into the fields the reset touched.
        queueMicrotask(rerender);
      };
      form.addEventListener("reset", onReset);
      return () => {
        form.removeEventListener("reset", onReset);
        if (formRef) formRef.current = null;
      };
    },
    [formRef],
  );

  return [state, formAction, pending, ref] as const;
}

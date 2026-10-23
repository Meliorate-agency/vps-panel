"use client";

import { useEffect, useRef, useState, useTransition } from "react";
import pageStyles from "@/components/page.module.css";
import styles from "./CommentCell.module.css";

type Result = { ok: true } | { ok: false; error: string };

const MAX_LEN = 200;

/**
 * The editable Comment cell on /apps. Click (or Enter on the focused cell) to
 * edit; Enter or leaving the field saves, Escape cancels. The new text shows
 * at once and goes back to the old one if the save fails (the error is the
 * cell's tooltip). An empty comment clears it.
 */
export default function CommentCell({
  appName,
  comment,
  saveAction,
}: {
  appName: string;
  comment: string;
  saveAction: (name: string, comment: string) => Promise<Result>;
}) {
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(comment);
  // What the cell shows: the saved comment, or the one being saved.
  const [shown, setShown] = useState(comment);
  const [error, setError] = useState<string | null>(null);
  const [pending, startTransition] = useTransition();
  const inputRef = useRef<HTMLInputElement>(null);

  // A refresh brings the stored comment (another tab, another user).
  const [lastServer, setLastServer] = useState(comment);
  if (comment !== lastServer && !pending) {
    setLastServer(comment);
    setShown(comment);
  }

  useEffect(() => {
    if (editing) inputRef.current?.select();
  }, [editing]);

  const startEdit = () => {
    setDraft(shown);
    setError(null);
    setEditing(true);
  };

  const save = () => {
    setEditing(false);
    const next = draft.replace(/\s+/g, " ").trim();
    if (next === shown) return;
    const before = shown;
    setShown(next);
    startTransition(async () => {
      const r = await saveAction(appName, next);
      if (r.ok) {
        setError(null);
      } else {
        setShown(before);
        setError(r.error);
      }
    });
  };

  if (editing) {
    return (
      <input
        ref={inputRef}
        className={`${pageStyles.input} ${styles.input}`}
        value={draft}
        maxLength={MAX_LEN}
        placeholder="Add a comment"
        aria-label={`Comment for ${appName}`}
        autoFocus
        onChange={(e) => setDraft(e.target.value)}
        onBlur={save}
        onKeyDown={(e) => {
          if (e.key === "Enter") {
            e.preventDefault();
            save();
          } else if (e.key === "Escape") {
            setEditing(false);
          }
        }}
      />
    );
  }

  return (
    <button
      type="button"
      className={`${styles.cell} ${shown ? "" : styles.empty} ${error ? styles.failed : ""}`}
      onClick={startEdit}
      title={error ? `Not saved: ${error}` : shown ? `${shown}\n(click to edit)` : "Add a comment"}
      aria-label={shown ? `Comment for ${appName}: ${shown}. Edit` : `Add a comment for ${appName}`}
    >
      {shown || "Add comment"}
      {pending && <span className={styles.saving}> · saving</span>}
    </button>
  );
}

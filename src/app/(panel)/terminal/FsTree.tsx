"use client";

import { useCallback, useEffect, useState } from "react";
import { listDirsAction } from "@/server-actions/terminal";
import styles from "./terminal.module.css";

/**
 * Lazy directory tree rooted at `/`.
 *
 * Loads one level at a time — expanding a node fetches only its children — so
 * opening the page never walks the filesystem. Directories only: this picks a
 * working directory, it isn't a file manager (that's /files).
 */

function join(parent: string, name: string): string {
  return parent === "/" ? `/${name}` : `${parent}/${name}`;
}

/** Every ancestor of `path`, root first: /a/b/c -> ["/", "/a", "/a/b"]. */
function ancestorsOf(path: string): string[] {
  const out = ["/"];
  const segments = path.split("/").filter(Boolean);
  let acc = "";
  for (const seg of segments.slice(0, -1)) {
    acc += `/${seg}`;
    out.push(acc);
  }
  return out;
}

export default function FsTree({
  selected,
  onSelect,
}: {
  selected: string;
  onSelect: (path: string) => void;
}) {
  // path -> child directory names. Presence in the map means "loaded".
  const [children, setChildren] = useState<Record<string, string[]>>({});
  const [expanded, setExpanded] = useState<Set<string>>(new Set(["/"]));
  const [loading, setLoading] = useState<Set<string>>(new Set());
  const [errors, setErrors] = useState<Record<string, string>>({});

  const load = useCallback(async (path: string) => {
    setLoading((prev) => new Set(prev).add(path));
    const res = await listDirsAction(path);
    setLoading((prev) => {
      const next = new Set(prev);
      next.delete(path);
      return next;
    });
    if (res.ok) {
      setChildren((prev) => ({ ...prev, [res.path]: res.dirs }));
      setErrors((prev) => {
        const { [path]: _drop, ...rest } = prev;
        return rest;
      });
    } else {
      // Unreadable directories are normal (/proc/<pid>, mountpoints with no
      // permission). Record it against the node rather than failing the tree.
      setErrors((prev) => ({ ...prev, [path]: res.error }));
    }
  }, []);

  // Reveal the selection: expand every ancestor and load what's missing. This
  // is what makes `cd /srv/apps` typed in the console walk the tree open.
  useEffect(() => {
    const ancestors = ancestorsOf(selected);
    setExpanded((prev) => {
      const next = new Set(prev);
      for (const a of ancestors) next.add(a);
      return next;
    });
    for (const a of ancestors) {
      if (!(a in children) && !loading.has(a)) void load(a);
    }
    // `children`/`loading` deliberately omitted: including them re-runs this on
    // every load completion, which re-triggers the loads it just finished.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [selected, load]);

  const toggle = (path: string) => {
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(path)) {
        next.delete(path);
      } else {
        next.add(path);
        if (!(path in children) && !loading.has(path)) void load(path);
      }
      return next;
    });
  };

  const renderNode = (path: string, label: string, depth: number) => {
    const isOpen = expanded.has(path);
    const isSelected = path === selected;
    const kids = children[path];
    const isLoading = loading.has(path);
    const error = errors[path];

    return (
      <div key={path}>
        <div
          className={[styles.treeRow, isSelected ? styles.treeRowActive : ""].join(" ").trim()}
          style={{ paddingLeft: 4 + depth * 12 }}
        >
          <button
            type="button"
            className={styles.treeToggle}
            onClick={() => toggle(path)}
            aria-label={isOpen ? `Collapse ${label}` : `Expand ${label}`}
            aria-expanded={isOpen}
          >
            {isLoading ? "·" : isOpen ? "▾" : "▸"}
          </button>
          <button
            type="button"
            className={styles.treeName}
            onClick={() => onSelect(path)}
            title={path}
          >
            {label}
          </button>
        </div>
        {isOpen && error && (
          <div className={styles.treeError} style={{ paddingLeft: 20 + depth * 12 }}>
            {error}
          </div>
        )}
        {isOpen && kids && kids.length === 0 && !error && (
          <div className={styles.treeEmpty} style={{ paddingLeft: 20 + depth * 12 }}>
            no subfolders
          </div>
        )}
        {isOpen &&
          kids?.map((name) => renderNode(join(path, name), name, depth + 1))}
      </div>
    );
  };

  return <div className={styles.tree}>{renderNode("/", "/", 0)}</div>;
}

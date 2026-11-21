import { useEffect, useRef, useState } from "react";
import { api } from "./api";

export const DIR_ICON_KEY = "__dir__";

const CACHE_LIMIT = 512;
const BATCH_LIMIT = 256;

const cache = new Map<string, string | null>();
const inflight = new Set<string>();
const pending = new Set<string>();
const listeners = new Set<(updated: ReadonlySet<string>) => void>();
let flushScheduled = false;

export function iconKeyForFile(name: string): string {
  if (!name || name.startsWith(".")) return "";
  const dot = name.lastIndexOf(".");
  if (dot <= 0 || dot + 1 >= name.length) return "";
  return name.slice(dot).toLowerCase();
}

export function iconKeyForNode(
  name: string,
  kind: "file" | "directory",
): string {
  return kind === "directory" ? DIR_ICON_KEY : iconKeyForFile(name);
}

function store(key: string, value: string | null) {
  if (!cache.has(key) && cache.size >= CACHE_LIMIT) {
    const oldest = cache.keys().next().value;
    if (oldest !== undefined) cache.delete(oldest);
  }
  cache.set(key, value);
}

function notify(updated: ReadonlySet<string>) {
  if (!updated.size) return;
  for (const listener of listeners) listener(updated);
}

function flush() {
  flushScheduled = false;
  if (!pending.size) return;
  if (typeof api.getSystemIcons !== "function") {
    pending.clear();
    return;
  }
  const batch: string[] = [];
  for (const key of pending) {
    pending.delete(key);
    if (!cache.has(key) && !inflight.has(key)) {
      inflight.add(key);
      batch.push(key);
      if (batch.length >= BATCH_LIMIT) break;
    }
  }
  if (pending.size && !flushScheduled) {
    flushScheduled = true;
    queueMicrotask(flush);
  }
  if (!batch.length) return;
  void Promise.resolve(api.getSystemIcons(batch)).then(
    (icons) => {
      const updated = new Set<string>();
      const record = icons && typeof icons === "object" ? icons : {};
      for (const key of batch) {
        inflight.delete(key);
        const value = typeof record[key] === "string" && record[key] ? record[key] : null;
        store(key, value);
        if (value) updated.add(key);
      }
      notify(updated);
    },
    () => {
      for (const key of batch) {
        inflight.delete(key);
        store(key, null);
      }
    },
  );
}

function requestIcons(keys: Iterable<string>) {
  if (typeof api.getSystemIcons !== "function") return;
  let queued = false;
  for (const key of keys) {
    if (!cache.has(key) && !inflight.has(key) && !pending.has(key)) {
      pending.add(key);
      queued = true;
    }
  }
  if (queued && !flushScheduled) {
    flushScheduled = true;
    queueMicrotask(flush);
  }
}

export function useSystemIcons(keys: string[]): (key: string) => string | undefined {
  const [, setTick] = useState(0);
  const wanted = useRef<Set<string>>(new Set());
  const unique = new Set(keys);
  wanted.current = unique;

  const signature = [...unique].sort().join("\0");
  useEffect(() => {
    const onUpdate = (updated: ReadonlySet<string>) => {
      for (const key of updated) {
        if (wanted.current.has(key)) {
          setTick((v) => v + 1);
          break;
        }
      }
    };
    listeners.add(onUpdate);
    return () => {
      listeners.delete(onUpdate);
    };
  }, []);

  useEffect(() => {
    if (wanted.current.size) requestIcons(wanted.current);
  }, [signature]);

  return (key: string) => cache.get(key) ?? undefined;
}

export function resetSystemIconCacheForTest() {
  cache.clear();
  inflight.clear();
  pending.clear();
  flushScheduled = false;
}

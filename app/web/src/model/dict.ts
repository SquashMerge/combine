/**
 * Plain-object dictionaries keyed by opaque IDs from the wire: tile, panel, space and host IDs.
 * Any string is a valid ID, including `__proto__`, `constructor` and `toString`, so a dictionary
 * has no prototype and is read only through own-property lookups. Use a `Map` where the value
 * never crosses IPC; use these where it is serialized as a JSON object.
 */
export type Dict<V> = Record<string, V>;

export function emptyDict<V>(): Dict<V> {
  return Object.create(null) as Dict<V>;
}

/** The value stored under `key`, ignoring anything inherited from a prototype. */
export function dictGet<V>(dict: Readonly<Dict<V>>, key: string): V | undefined {
  return Object.hasOwn(dict, key) ? dict[key] : undefined;
}

/** A copy of `dict` with `key` set to `value`. */
export function dictWith<V>(dict: Readonly<Dict<V>>, key: string, value: V): Dict<V> {
  const next = emptyDict<V>();
  for (const [k, v] of Object.entries(dict)) next[k] = v;
  next[key] = value;
  return next;
}

/** A copy of `dict` without the listed keys. */
export function dictWithout<V>(dict: Readonly<Dict<V>>, keys: readonly string[]): Dict<V> {
  const next = emptyDict<V>();
  for (const [k, v] of Object.entries(dict)) {
    if (!keys.includes(k)) next[k] = v;
  }
  return next;
}

const INTEGER_FORMATTER = new Intl.NumberFormat("en-US", {
  maximumFractionDigits: 0,
});

/** Format whole-number UI counts with stable thousands separators. */
export function formatInteger(value: number): string {
  return INTEGER_FORMATTER.format(value);
}

/** A count as a badge pill shows it: "99+" above 99. */
export function countBadge(count: number): string {
  return count > 99 ? "99+" : String(count);
}

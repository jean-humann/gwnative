// Page-side metrics, batched to the host.
//
// The same three kinds the host keeps, so a name means one thing wherever it
// was recorded: a count accumulates, a gauge keeps the last value, a peak keeps
// the largest. Counts sent here are increments since the last flush, because
// the host is what accumulates them — resending a running total would make
// every flush add the whole history again.
//
// Batched on the host's own one-second cadence so that a page metric and the
// process sample it belongs with land in adjacent records.

const FLUSH_MS = 1000;

let pending = { count: {}, gauge: {}, peak: {} };
let dirty = false;

export const count = (name, value = 1) => {
  pending.count[name] = (pending.count[name] ?? 0) + value;
  dirty = true;
};

export const gauge = (name, value) => {
  pending.gauge[name] = value;
  dirty = true;
};

export const peak = (name, value) => {
  const seen = pending.peak[name];
  if (seen === undefined || value > seen) pending.peak[name] = value;
  dirty = true;
};

const flush = () => {
  if (!dirty) return;
  const batch = pending;
  pending = { count: {}, gauge: {}, peak: {} };
  dirty = false;
  // Peaks reset with the batch rather than accumulating here, which is only
  // correct because the host maxes them: the largest across every flush is
  // still the largest overall.
  fetch('__diag', {
    method: 'POST',
    headers: {
      'Content-Type': 'application/json',
      'X-Gwnative-Token': window.__gwnativeToken ?? '',
    },
    body: JSON.stringify(batch),
  }).catch(() => {});
};

setInterval(flush, FLUSH_MS);

// A tab going away takes the last second of metrics with it otherwise, and
// that second is the one before a crash.
window.addEventListener('pagehide', flush);

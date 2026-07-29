// Cadence for the optional command-line frame-rate ceiling.
//
// requestAnimationFrame follows the display, so a 60 FPS request on a 75 Hz
// panel cannot be implemented by keeping every second callback: that produces
// 37.5 FPS and throws the unused quarter of the display away. Keep an absolute
// deadline instead. A late display callback advances that deadline by whole
// target intervals, preserving the fractional remainder across frames.

export function createFrameCadence(frameMilliseconds) {
  if (!Number.isFinite(frameMilliseconds) || frameMilliseconds <= 0) {
    return Object.freeze({ accept: () => true });
  }
  let deadline = null;
  return Object.freeze({
    accept(timestamp) {
      if (!Number.isFinite(timestamp)) return false;
      if (deadline === null) {
        deadline = timestamp + frameMilliseconds;
        return true;
      }
      if (timestamp + 0.05 < deadline) return false;
      const elapsedIntervals =
        Math.floor(Math.max(0, timestamp - deadline) / frameMilliseconds) + 1;
      deadline += elapsedIntervals * frameMilliseconds;
      return true;
    },
  });
}
